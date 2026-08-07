//! [`McpService`] implementation for internet-radio-mcp.
//!
//! Owns the shared `NowPlaying` state (a tracked `Child` + current station)
//! and dispatches the four radio tools.

use std::process::Child;
use std::sync::Arc;

use mcp_core::telemetry::metrics::{self, Label};
use mcp_core::{CallError, McpService, ServerConfig, ToolDef, ToolReply, async_trait};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::error::{InternetRadioMcpError, McpError, RadioError};
use crate::models::Station;
use crate::operations::radio;

// ── server configuration ─────────────────────────────────────────────────────

/// Model-facing summary of this server, emitted as the MCP `instructions`
/// field in the initialize response.
///
/// Why: the host captures `instructions` and uses it as the server's
/// searchable description, so it must say what the server is for, when to
/// reach for it, the tools by name, and the load-bearing constraint (playback
/// is a local `mpv` process on the host's own speakers).
pub const SERVER_INSTRUCTIONS: &str = "Discover and play live internet radio on this machine. \
Reach for it whenever the user wants to listen to, find, or control a radio station - live music \
by genre (jazz, classical, reggae), news or talk radio, or a specific broadcaster. Typical flow: \
`radio_search` finds stations by name, genre, or tag via the public Radio Browser directory and \
returns a stream URL, then `radio_play` starts it, while `radio_stop` halts playback and \
`radio_now_playing` reports the current station. Playback runs a local `mpv` process, so audio \
comes out of the host machine's own speakers and mpv must be installed; station search needs no \
API key.";

/// Build the [`ServerConfig`] for internet-radio-mcp.
///
/// Why: kept here (rather than inline in `main`) so the server-level
/// `instructions` blurb and transport settings are unit-testable without
/// standing up a transport.
pub fn server_config() -> ServerConfig {
    ServerConfig::new("internet-radio-mcp", env!("CARGO_PKG_VERSION"))
        .without_websocket()
        .instructions(SERVER_INSTRUCTIONS)
}

// ── shared state ─────────────────────────────────────────────────────────────

/// State for the currently-playing station (if any).
///
/// Holds the `Child` handle rather than a raw PID to prevent zombie processes
/// and PID-reuse hazards. Protected by a `Mutex` so play/stop sequences are
/// atomic (no double-spawn race). Closes #5, Closes #8.
pub struct NowPlaying {
    pub child: Option<Child>,
    pub station: Option<Station>,
}

impl std::fmt::Debug for NowPlaying {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NowPlaying")
            .field("pid", &self.child.as_ref().map(|c| c.id()))
            .field("station", &self.station)
            .finish()
    }
}

// `Child` does not implement `Default`, so we cannot `#[derive(Default)]`.
#[allow(clippy::derivable_impls)]
impl Default for NowPlaying {
    fn default() -> Self {
        Self {
            child: None,
            station: None,
        }
    }
}

// ── service ──────────────────────────────────────────────────────────────────

/// Overrides the Radio Browser base URL a freshly constructed [`RadioService`]
/// points at. Not an operator-facing feature: no CLI flag reaches it and the
/// README does not document it. It exists purely as a seam for
/// `tests/telemetry_stdio.rs`, which spawns the real binary as a child
/// process and has no other way to point it at a local mock server instead
/// of the live directory (mcp-core#40: no test may reach a live directory or
/// stream). Unset in every real deployment.
const RADIO_BROWSER_BASE_ENV_VAR: &str = "INTERNET_RADIO_MCP_RADIO_BROWSER_BASE";

/// The MCP service for internet-radio-mcp.
///
/// Wraps the shared `NowPlaying` state; `McpService` is wired up by mcp-core.
pub struct RadioService {
    http_client: Client,
    // The Radio Browser base URL. A field (rather than the bare constant)
    // so a test can point it at a local mock server; see
    // `RADIO_BROWSER_BASE_ENV_VAR` and `with_radio_browser_base`.
    radio_browser_base: String,
    // Mutex (not RwLock) because play and stop both mutate the child handle.
    // The Mutex ensures the full stop-prior → spawn → update sequence is
    // atomic, preventing concurrent play calls from double-spawning mpv.
    // Closes #8.
    now_playing: Arc<Mutex<NowPlaying>>,
}

impl RadioService {
    /// Create a new service instance.
    pub fn new() -> Self {
        let radio_browser_base = std::env::var(RADIO_BROWSER_BASE_ENV_VAR)
            .unwrap_or_else(|_| radio::RADIO_BROWSER_BASE.to_string());
        Self {
            http_client: Client::new(),
            radio_browser_base,
            now_playing: Arc::new(Mutex::new(NowPlaying::default())),
        }
    }

    /// Create a service instance pointed at a custom Radio Browser base URL,
    /// bypassing `RADIO_BROWSER_BASE_ENV_VAR`. For a test that builds the
    /// service in-process (rather than through a spawned child) and wants a
    /// local mock server without touching the environment.
    pub fn with_radio_browser_base(base_url: impl Into<String>) -> Self {
        Self {
            radio_browser_base: base_url.into(),
            ..Self::new()
        }
    }
}

impl Default for RadioService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl McpService for RadioService {
    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef::new(
                "radio_search",
                "Find internet radio stations to listen to - search by station name, genre, or tag (e.g. jazz, news, classical) using the public Radio Browser directory. Returns matching stations, each with a stream URL, country, genre tags, bitrate, and popularity (vote count), best-voted first. Pass a returned stream URL to radio_play to start listening.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search term: station name, genre, or tag depending on 'by'."
                        },
                        "by": {
                            "type": "string",
                            "enum": ["name", "tag", "genre"],
                            "description": "Search mode: 'name' searches by station name (default), 'tag'/'genre' searches by genre/tag."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum results to return (1–50, default 10)."
                        }
                    },
                    "required": ["query"]
                }),
            ),
            ToolDef::new(
                "radio_play",
                "Start playback of a radio station via mpv. Provide a direct stream URL (preferred) or a Radio Browser station UUID. Exactly one of 'url' or 'uuid' is required. Stops any currently-playing station first.",
                json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "Direct stream URL to play (e.g. from radio_search results). Must use http:// or https://."
                        },
                        "uuid": {
                            "type": "string",
                            "description": "Radio Browser station UUID (format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx); the server will resolve the stream URL."
                        },
                        "name": {
                            "type": "string",
                            "description": "Optional display name (used when 'url' is provided without a uuid lookup)."
                        }
                    },
                    "required": ["url"]
                }),
            ),
            ToolDef::new(
                "radio_stop",
                "Stop the currently tracked radio station by terminating its mpv process. No-op if nothing is playing.",
                json!({
                    "type": "object",
                    "properties": {}
                }),
            ),
            ToolDef::new(
                "radio_now_playing",
                "Return the name and stream URL of the currently-playing station, or a message indicating nothing is playing.",
                json!({
                    "type": "object",
                    "properties": {}
                }),
            ),
        ]
    }

    async fn call_tool(&self, name: &str, arguments: &Value) -> Result<ToolReply, CallError> {
        // Accept both null/missing arguments (for no-param tools) and objects.
        let empty_map = serde_json::Map::new();
        let args = match arguments {
            Value::Object(m) => m,
            Value::Null => &empty_map,
            _ => {
                return Err(CallError::invalid_params("arguments must be an object"));
            }
        };

        match name {
            "radio_search" => self.exec_radio_search(args).await,
            "radio_play" => self.exec_radio_play(args).await,
            "radio_stop" => self.exec_radio_stop().await,
            "radio_now_playing" => self.exec_radio_now_playing().await,
            other => Err(CallError::tool(format!("unknown tool: {other}"))),
        }
    }
}

// ── tool implementations ─────────────────────────────────────────────────────

impl RadioService {
    // `args` carries the search query -- a tool argument, so it is content
    // (mcp-core#40, epic D10) -- so `skip_all`. The span still gives this
    // handler's own work its own timing, nested under mcp-core's
    // `mcp.tools.call` span.
    #[tracing::instrument(skip_all)]
    async fn exec_radio_search(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<ToolReply, CallError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CallError::invalid_params("missing required parameter: query"))?;

        let by = args.get("by").and_then(|v| v.as_str()).unwrap_or("name");

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .min(50) as u32;

        // "genre" is an accepted alias for "tag". Closes #8.
        let search_result = match by {
            "tag" | "genre" => {
                radio::search_by_tag(&self.http_client, &self.radio_browser_base, query, limit)
                    .await
            }
            _ => {
                radio::search_by_name(&self.http_client, &self.radio_browser_base, query, limit)
                    .await
            }
        };
        record_upstream_failure("radio_search", &search_result);
        let stations = search_result.map_err(|e| CallError::tool(e.to_string()))?;

        let items: Vec<Value> = stations
            .iter()
            .map(|s| {
                json!({
                    "uuid": s.uuid,
                    "name": s.name,
                    "url": s.url_resolved,
                    "country": s.country,
                    "tags": s.tags,
                    "bitrate_kbps": s.bitrate,
                    "codec": s.codec,
                    "votes": s.votes,
                })
            })
            .collect();

        ToolReply::json(&items).map_err(CallError::from)
    }

    // `args` carries the url, uuid and/or name a caller chose to play --
    // content (mcp-core#40, epic D10) -- so `skip_all`.
    #[tracing::instrument(skip_all)]
    async fn exec_radio_play(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<ToolReply, CallError> {
        let url_opt = args.get("url").and_then(|v| v.as_str());
        let uuid_opt = args.get("uuid").and_then(|v| v.as_str());
        let name_opt = args.get("name").and_then(|v| v.as_str());

        let (stream_url, station_name) = if let Some(url) = url_opt {
            let name = name_opt.unwrap_or("Unknown station").to_string();
            (url.to_string(), name)
        } else if let Some(uuid) = uuid_opt {
            // Validate UUID before injecting into URL. Closes #7.
            radio::validate_uuid(uuid).map_err(|e| match e {
                crate::error::InternetRadioMcpError::Mcp(McpError::InvalidToolParameters(m)) => {
                    CallError::invalid_params(m)
                }
                other => CallError::tool(other.to_string()),
            })?;

            let lookup_result =
                radio::station_by_uuid(&self.http_client, &self.radio_browser_base, uuid).await;
            record_upstream_failure("radio_play", &lookup_result);
            let station = lookup_result
                .map_err(|e| CallError::tool(e.to_string()))?
                .ok_or_else(|| CallError::tool(format!("Station UUID not found: {uuid}")))?;
            (station.url_resolved.clone(), station.name.clone())
        } else {
            return Err(CallError::invalid_params(
                "provide either 'url' or 'uuid' to play a station",
            ));
        };

        // Hold the mutex across the entire stop-prior → spawn → update sequence
        // to prevent concurrent play calls from double-spawning mpv. Closes #8.
        let mut np = self.now_playing.lock().await;

        // Stop any current playback first (using the Child handle). Closes #5.
        if let Some(child) = np.child.take() {
            let _ = radio::stop_child(child);
        }

        let play_result = radio::play_station(&stream_url);
        record_upstream_failure("radio_play", &play_result);
        let child = play_result.map_err(|e| CallError::tool(e.to_string()))?;

        np.child = Some(child);
        np.station = Some(Station {
            uuid: uuid_opt.unwrap_or("").to_string(),
            name: station_name.clone(),
            url_resolved: stream_url.clone(),
            country: String::new(),
            tags: String::new(),
            bitrate: 0,
            codec: String::new(),
            votes: 0,
        });

        Ok(ToolReply::text(format!(
            "▶ Now playing: {} ({})",
            station_name, stream_url
        )))
    }

    // No arguments to skip, but instrumented for consistency: every tool
    // handler opens its own span, nested under mcp-core's `mcp.tools.call`.
    #[tracing::instrument(skip_all)]
    async fn exec_radio_stop(&self) -> Result<ToolReply, CallError> {
        let mut np = self.now_playing.lock().await;

        if let Some(child) = np.child.take() {
            radio::stop_child(child).map_err(|e| CallError::tool(e.to_string()))?;
        }
        // If nothing was playing, this is a no-op — not an error. Closes #8.
        np.station = None;

        Ok(ToolReply::text("⏹ Playback stopped."))
    }

    #[tracing::instrument(skip_all)]
    async fn exec_radio_now_playing(&self) -> Result<ToolReply, CallError> {
        let np = self.now_playing.lock().await;
        // PID is an implementation detail; omit from user-facing output. Closes #8.
        let text = match &np.station {
            Some(s) => format!("▶ Now playing: {} — {}", s.name, s.url_resolved),
            None => "⏹ Nothing is currently playing.".to_string(),
        };
        Ok(ToolReply::text(text))
    }
}

/// Classify an upstream-call failure into the bounded reason
/// [`record_upstream_failure`] counts, or `None` for a normal "not found"
/// result rather than a fault reaching outward. Rule 8.2 keeps an
/// operational decline out of a failure counter: an empty Radio Browser
/// result is the directory doing its job, not the directory breaking.
///
/// Exhaustive over [`InternetRadioMcpError`], so a new variant forces this
/// classification to be revisited rather than silently landing as "not
/// counted".
fn upstream_failure_reason(err: &InternetRadioMcpError) -> Option<&'static str> {
    match err {
        InternetRadioMcpError::Radio(RadioError::ApiError(_)) => Some("directory"),
        InternetRadioMcpError::Radio(RadioError::PlayerError(_)) => Some("player"),
        InternetRadioMcpError::Radio(RadioError::NoStationsFound(_)) => None,
        InternetRadioMcpError::Mcp(_)
        | InternetRadioMcpError::Json(_)
        | InternetRadioMcpError::Io(_) => None,
    }
}

/// Count an upstream-call failure against `radio.upstream_failures`.
///
/// `tool` is always one of the `&'static str` literals its call sites pass,
/// so the label is bounded there rather than by anything a caller supplies;
/// `reason` is bounded the same way, by [`upstream_failure_reason`]'s fixed
/// set of return values. Neither label is ever built from a station name, a
/// search query, a UUID, or a stream URL.
fn record_upstream_failure<T>(tool: &'static str, outcome: &Result<T, InternetRadioMcpError>) {
    if let Err(err) = outcome
        && let Some(reason) = upstream_failure_reason(err)
    {
        metrics::increment(
            "radio.upstream_failures",
            &[Label::new("tool", tool), Label::new("reason", reason)],
        );
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tools_returns_four() {
        let svc = RadioService::new();
        assert_eq!(svc.tools().len(), 4);
        let tool_defs = svc.tools();
        let names: Vec<&str> = tool_defs.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"radio_search"));
        assert!(names.contains(&"radio_play"));
        assert!(names.contains(&"radio_stop"));
        assert!(names.contains(&"radio_now_playing"));
    }

    #[tokio::test]
    async fn test_tool_not_found() {
        let svc = RadioService::new();
        let res = svc.call_tool("nonexistent_tool", &json!({})).await;
        match res {
            Err(CallError::Tool(msg)) => assert!(msg.contains("unknown tool")),
            other => panic!("expected Tool error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_search_missing_query() {
        let svc = RadioService::new();
        let res = svc.call_tool("radio_search", &json!({})).await;
        match res {
            Err(CallError::InvalidParams(msg)) => assert!(msg.contains("query")),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_play_missing_url_and_uuid() {
        let svc = RadioService::new();
        let res = svc.call_tool("radio_play", &json!({})).await;
        match res {
            Err(CallError::InvalidParams(msg)) => {
                assert!(msg.contains("url") || msg.contains("uuid"))
            }
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    // Closes #7 — UUID validated before URL construction.
    #[tokio::test]
    async fn test_play_invalid_uuid_rejected() {
        let svc = RadioService::new();
        let res = svc
            .call_tool("radio_play", &json!({ "uuid": "../../etc/passwd" }))
            .await;
        assert!(
            matches!(res, Err(CallError::InvalidParams(_))),
            "expected InvalidParams, got {res:?}"
        );
    }

    // Closes #8 — file:// URL rejected.
    #[tokio::test]
    async fn test_play_file_url_rejected() {
        let svc = RadioService::new();
        let res = svc
            .call_tool("radio_play", &json!({ "url": "file:///etc/passwd" }))
            .await;
        match res {
            Err(CallError::Tool(msg)) => {
                assert!(
                    msg.contains("http") || msg.contains("allowed"),
                    "unexpected: {msg}"
                )
            }
            other => panic!("expected Tool error, got {other:?}"),
        }
    }

    // Closes #8 — stop when nothing is playing is a no-op.
    #[tokio::test]
    async fn test_stop_when_nothing_playing() {
        let svc = RadioService::new();
        let res = svc.call_tool("radio_stop", &json!({})).await;
        let reply = res.expect("stop with nothing playing should be a no-op");
        assert!(!reply.is_error);
        let text = match &reply.content[0] {
            mcp_core::Content::Text(t) => t.as_str(),
            _ => panic!("expected text content"),
        };
        assert!(text.contains("stopped") || text.contains("Playback"));
    }

    #[tokio::test]
    async fn test_now_playing_default() {
        let svc = RadioService::new();
        let reply = svc
            .call_tool("radio_now_playing", &json!({}))
            .await
            .unwrap();
        let text = match &reply.content[0] {
            mcp_core::Content::Text(t) => t.as_str(),
            _ => panic!("expected text content"),
        };
        assert!(text.contains("Nothing") || text.contains("playing"));
    }

    // Closes #8 — now_playing does not expose PID in output.
    #[tokio::test]
    async fn test_now_playing_no_pid_in_output() {
        let svc = RadioService::new();
        let reply = svc
            .call_tool("radio_now_playing", &json!({}))
            .await
            .unwrap();
        let text = match &reply.content[0] {
            mcp_core::Content::Text(t) => t.as_str(),
            _ => panic!("expected text content"),
        };
        assert!(
            !text.contains("pid:"),
            "PID should not appear in now_playing output, got: {text}"
        );
    }

    // Closes #7 — malformed UUID (wrong length) returns InvalidParams.
    #[tokio::test]
    async fn test_play_malformed_uuid_returns_error() {
        let svc = RadioService::new();
        // 35 chars — wrong length
        let res = svc
            .call_tool(
                "radio_play",
                &json!({ "uuid": "550e8400-e29b-41d4-a716-44665544000" }),
            )
            .await;
        assert!(
            matches!(res, Err(CallError::InvalidParams(_))),
            "expected InvalidParams, got {res:?}"
        );
    }

    // Closes #8 — limit schema uses integer type.
    #[test]
    fn test_search_schema_limit_is_integer() {
        let svc = RadioService::new();
        let tools = svc.tools();
        let search = tools.iter().find(|t| t.name == "radio_search").unwrap();
        let limit_type = search.input_schema["properties"]["limit"]["type"]
            .as_str()
            .unwrap();
        assert_eq!(limit_type, "integer");
    }

    // Closes #8 — radio_stop description is accurate.
    #[test]
    fn test_stop_schema_description_accurate() {
        let svc = RadioService::new();
        let tools = svc.tools();
        let stop = tools.iter().find(|t| t.name == "radio_stop").unwrap();
        assert!(
            !stop.description.contains("kills all mpv"),
            "description should not say 'kills all mpv', got: {}",
            stop.description
        );
    }

    // Closes #8 — genre alias is documented in the schema enum.
    #[test]
    fn test_search_schema_includes_genre_enum() {
        let svc = RadioService::new();
        let tools = svc.tools();
        let search = tools.iter().find(|t| t.name == "radio_search").unwrap();
        let by_enum = search.input_schema["properties"]["by"]["enum"]
            .as_array()
            .unwrap();
        let values: Vec<&str> = by_enum.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            values.contains(&"genre"),
            "schema should include 'genre' as a valid 'by' value"
        );
    }

    // Arguments must be an object — non-object args return InvalidParams.
    #[tokio::test]
    async fn test_non_object_args_invalid_params() {
        let svc = RadioService::new();
        let res = svc.call_tool("radio_search", &json!("not an object")).await;
        assert!(
            matches!(res, Err(CallError::InvalidParams(_))),
            "expected InvalidParams for non-object args, got {res:?}"
        );
    }

    // Server exposes a non-empty, model-facing `instructions` blurb that the
    // host uses as the server's searchable description; it must name the
    // primary tools so discovery can reason about the search -> play flow.
    #[test]
    fn test_server_config_has_nonempty_instructions() {
        let cfg = server_config();
        let instructions = cfg
            .instructions
            .expect("server config must set an instructions blurb");
        assert!(
            !instructions.trim().is_empty(),
            "instructions must be non-empty"
        );
        assert!(
            instructions.contains("radio_search"),
            "instructions should name radio_search, got: {instructions}"
        );
        assert!(
            instructions.contains("radio_play"),
            "instructions should name radio_play, got: {instructions}"
        );
    }

    // radio_search description leads with the natural intent ("listen") and
    // points at the play handoff so the model chains search -> play. Mirrors
    // web-mcp's *_description_* natural-terms pin.
    #[test]
    fn test_radio_search_description_leads_with_purpose() {
        let svc = RadioService::new();
        let tools = svc.tools();
        let search = tools
            .iter()
            .find(|t| t.name == "radio_search")
            .expect("radio_search tool must exist");
        let d = search.description.to_lowercase();
        assert!(
            d.contains("listen"),
            "description should surface the natural 'listen' intent, got: {}",
            search.description
        );
        assert!(
            d.contains("radio_play"),
            "description should point to radio_play for the search -> play flow, got: {}",
            search.description
        );
        assert!(
            d.contains("genre"),
            "description should name the genre search dimension, got: {}",
            search.description
        );
    }

    // UUID injection attempt (36 chars, but contains '?') is rejected.
    #[tokio::test]
    async fn test_play_uuid_injection_rejected() {
        let svc = RadioService::new();
        let res = svc
            .call_tool(
                "radio_play",
                &json!({ "uuid": "550e8400-e29b-41d4-a716-4466554?0000" }),
            )
            .await;
        assert!(
            matches!(res, Err(CallError::InvalidParams(_))),
            "expected InvalidParams for injection uuid, got {res:?}"
        );
    }

    // ── telemetry: upstream-failure classification (mcp-core#40) ───────────
    //
    // The metrics registry is process-global and cargo test runs a file's
    // tests concurrently by default, so every test here that touches it is
    // serialised behind this mutex (mcp-core#40, lesson 6). It holds no data
    // of its own.
    static METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_metrics() -> std::sync::MutexGuard<'static, ()> {
        METRICS_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn counter_total(name: &str, labels: &[Label]) -> u64 {
        metrics::global()
            .snapshot()
            .counters
            .iter()
            .find(|c| c.name == name && same_labels(&c.labels, labels))
            .map_or(0, |c| c.total)
    }

    fn same_labels(recorded: &[Label], wanted: &[Label]) -> bool {
        recorded.len() == wanted.len()
            && wanted.iter().all(|want| {
                recorded
                    .iter()
                    .any(|have| have.key() == want.key() && have.value() == want.value())
            })
    }

    #[test]
    fn upstream_failure_reason_counts_directory_and_player_faults() {
        assert_eq!(
            upstream_failure_reason(&InternetRadioMcpError::Radio(RadioError::ApiError(
                "x".into()
            ))),
            Some("directory"),
            "a Radio Browser API fault must count as an upstream failure"
        );
        assert_eq!(
            upstream_failure_reason(&InternetRadioMcpError::Radio(RadioError::PlayerError(
                "x".into()
            ))),
            Some("player"),
            "an mpv player fault must count as an upstream failure"
        );
    }

    #[test]
    fn upstream_failure_reason_excludes_no_stations_found() {
        // A "no stations found" result is the directory doing its job, not a
        // fault reaching outward -- rule 8.2 keeps an operational decline out
        // of a failure counter.
        assert_eq!(
            upstream_failure_reason(&InternetRadioMcpError::Radio(RadioError::NoStationsFound(
                "x".into()
            ))),
            None,
            "an empty ('no stations found') result must not count as an upstream failure"
        );
    }

    #[test]
    fn record_upstream_failure_increments_only_for_counted_reasons() {
        let _guard = lock_metrics();
        let labels = [
            Label::new("tool", "radio_search"),
            Label::new("reason", "directory"),
        ];
        let before = counter_total("radio.upstream_failures", &labels);

        let ok: Result<Vec<Station>, InternetRadioMcpError> = Ok(vec![]);
        record_upstream_failure("radio_search", &ok);
        let not_found: Result<Vec<Station>, InternetRadioMcpError> = Err(
            InternetRadioMcpError::Radio(RadioError::NoStationsFound("x".into())),
        );
        record_upstream_failure("radio_search", &not_found);
        assert_eq!(
            counter_total("radio.upstream_failures", &labels),
            before,
            "a success or an empty-result decline must not move the counter"
        );

        let api_failed: Result<Vec<Station>, InternetRadioMcpError> = Err(
            InternetRadioMcpError::Radio(RadioError::ApiError("x".into())),
        );
        record_upstream_failure("radio_search", &api_failed);
        assert_eq!(
            counter_total("radio.upstream_failures", &labels),
            before + 1,
            "a directory fault must increment the counter, labelled by tool and reason"
        );
    }
}
