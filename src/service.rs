//! [`McpService`] implementation for internet-radio-mcp.
//!
//! Owns the shared `NowPlaying` state (a tracked `Child` + current station)
//! and dispatches the four radio tools.

use std::process::Child;
use std::sync::Arc;

use mcp_core::{CallError, McpService, ServerConfig, ToolDef, ToolReply, async_trait};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::error::McpError;
use crate::models::Station;
use crate::operations::radio;

// ── server configuration ─────────────────────────────────────────────────────

/// Build the [`ServerConfig`] for internet-radio-mcp.
///
/// Why: kept here (rather than inline in `main`) so the server-level
/// `instructions` blurb and transport settings are unit-testable without
/// standing up a transport.
pub fn server_config() -> ServerConfig {
    ServerConfig::new("internet-radio-mcp", env!("CARGO_PKG_VERSION")).without_websocket()
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

/// The MCP service for internet-radio-mcp.
///
/// Wraps the shared `NowPlaying` state; `McpService` is wired up by mcp-core.
pub struct RadioService {
    http_client: Client,
    // Mutex (not RwLock) because play and stop both mutate the child handle.
    // The Mutex ensures the full stop-prior → spawn → update sequence is
    // atomic, preventing concurrent play calls from double-spawning mpv.
    // Closes #8.
    now_playing: Arc<Mutex<NowPlaying>>,
}

impl RadioService {
    /// Create a new service instance.
    pub fn new() -> Self {
        Self {
            http_client: Client::new(),
            now_playing: Arc::new(Mutex::new(NowPlaying::default())),
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
                "Search for internet radio stations via the Radio Browser API. Returns a list of stations with name, stream URL, country, genre tags, bitrate, and vote count.",
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

        let stations = match by {
            // "genre" is an accepted alias for "tag". Closes #8.
            "tag" | "genre" => radio::search_by_tag(&self.http_client, query, limit)
                .await
                .map_err(|e| CallError::tool(e.to_string()))?,
            _ => radio::search_by_name(&self.http_client, query, limit)
                .await
                .map_err(|e| CallError::tool(e.to_string()))?,
        };

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

            let station = radio::station_by_uuid(&self.http_client, uuid)
                .await
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

        let child = radio::play_station(&stream_url).map_err(|e| CallError::tool(e.to_string()))?;

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

    async fn exec_radio_stop(&self) -> Result<ToolReply, CallError> {
        let mut np = self.now_playing.lock().await;

        if let Some(child) = np.child.take() {
            radio::stop_child(child).map_err(|e| CallError::tool(e.to_string()))?;
        }
        // If nothing was playing, this is a no-op — not an error. Closes #8.
        np.station = None;

        Ok(ToolReply::text("⏹ Playback stopped."))
    }

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
}
