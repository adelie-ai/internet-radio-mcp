#![deny(warnings)]

// Tool registry and MCP tool definitions.

use crate::error::{McpError, Result};
use crate::models::Station;
use crate::operations::radio;
use reqwest::Client;
use serde_json::Value;
use std::process::Child;
use std::sync::Arc;
use tokio::sync::Mutex;

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

/// Registry for all radio MCP tools.
pub struct ToolRegistry {
    http_client: Client,
    // Mutex (not RwLock) because play and stop both mutate the child handle.
    // The Mutex ensures the full stop-prior → spawn → update sequence is
    // atomic, preventing concurrent play calls from double-spawning mpv.
    // Closes #8.
    now_playing: Arc<Mutex<NowPlaying>>,
}

impl ToolRegistry {
    /// Create a new registry.
    pub fn new() -> Self {
        Self {
            http_client: Client::new(),
            now_playing: Arc::new(Mutex::new(NowPlaying::default())),
        }
    }

    /// List available tools in MCP schema format.
    pub async fn list_tools(&self) -> Value {
        Value::Array(vec![
            radio_search_schema(),
            radio_play_schema(),
            radio_stop_schema(),
            radio_now_playing_schema(),
        ])
    }

    /// Execute a tool by name. Returns `(result_value, tools_changed)`.
    pub async fn execute_tool(&self, name: &str, arguments: &Value) -> Result<(Value, bool)> {
        // Accept both null/missing arguments (for no-param tools like radio_stop) and objects.
        let empty_map = serde_json::Map::new();
        let args = match arguments {
            Value::Object(m) => m,
            Value::Null => &empty_map,
            _ => {
                return Err(McpError::InvalidToolParameters(
                    "Arguments must be an object".to_string(),
                )
                .into());
            }
        };

        match name {
            "radio_search" => {
                let result = self.exec_radio_search(args).await?;
                Ok((result, false))
            }
            "radio_play" => {
                let result = self.exec_radio_play(args).await?;
                Ok((result, false))
            }
            "radio_stop" => {
                let result = self.exec_radio_stop().await?;
                Ok((result, false))
            }
            "radio_now_playing" => {
                let result = self.exec_radio_now_playing().await?;
                Ok((result, false))
            }
            _ => Err(McpError::ToolNotFound(name.to_string()).into()),
        }
    }

    // ── tool implementations ──────────────────────────────────────────────────

    async fn exec_radio_search(&self, args: &serde_json::Map<String, Value>) -> Result<Value> {
        let query = args.get("query").and_then(|v| v.as_str()).ok_or_else(|| {
            McpError::InvalidToolParameters("Missing required parameter: query".to_string())
        })?;

        let by = args.get("by").and_then(|v| v.as_str()).unwrap_or("name");

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .min(50) as u32;

        let stations = match by {
            // "genre" is an accepted alias for "tag". Closes #8 (document/keep alias).
            "tag" | "genre" => radio::search_by_tag(&self.http_client, query, limit).await?,
            _ => radio::search_by_name(&self.http_client, query, limit).await?,
        };

        let items: Vec<Value> = stations
            .iter()
            .map(|s| {
                serde_json::json!({
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

        Ok(serde_json::json!({
            "content": [{
                "type": "json",
                "value": items
            }]
        }))
    }

    async fn exec_radio_play(&self, args: &serde_json::Map<String, Value>) -> Result<Value> {
        // Accept either a direct URL or a uuid (looked up first).
        let url_opt = args.get("url").and_then(|v| v.as_str());
        let uuid_opt = args.get("uuid").and_then(|v| v.as_str());
        let name_opt = args.get("name").and_then(|v| v.as_str());

        let (stream_url, station_name) = if let Some(url) = url_opt {
            let name = name_opt.unwrap_or("Unknown station").to_string();
            (url.to_string(), name)
        } else if let Some(uuid) = uuid_opt {
            let station = radio::station_by_uuid(&self.http_client, uuid)
                .await?
                .ok_or_else(|| {
                    McpError::InvalidToolParameters(format!("Station UUID not found: {}", uuid))
                })?;
            let url = station.url_resolved.clone();
            let name = station.name.clone();
            (url, name)
        } else {
            return Err(McpError::InvalidToolParameters(
                "Provide either 'url' or 'uuid' to play a station".to_string(),
            )
            .into());
        };

        // Hold the mutex across the entire stop-prior → spawn → update sequence
        // to prevent concurrent play calls from double-spawning mpv. Closes #8.
        let mut np = self.now_playing.lock().await;

        // Stop any current playback first (using the Child handle). Closes #5.
        if let Some(child) = np.child.take() {
            let _ = radio::stop_child(child);
        }

        let child = radio::play_station(&stream_url)?;

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

        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("▶ Now playing: {} ({})", station_name, stream_url)
            }]
        }))
    }

    async fn exec_radio_stop(&self) -> Result<Value> {
        let mut np = self.now_playing.lock().await;

        if let Some(child) = np.child.take() {
            radio::stop_child(child)?;
        }
        // If nothing was playing, this is a no-op — not an error. Closes #8.
        np.station = None;

        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": "⏹ Playback stopped."
            }]
        }))
    }

    async fn exec_radio_now_playing(&self) -> Result<Value> {
        let np = self.now_playing.lock().await;
        // PID is an implementation detail; omit from user-facing output. Closes #8.
        let text = match &np.station {
            Some(s) => format!("▶ Now playing: {} — {}", s.name, s.url_resolved),
            None => "⏹ Nothing is currently playing.".to_string(),
        };
        Ok(serde_json::json!({
            "content": [{"type": "text", "text": text}]
        }))
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── JSON Schema definitions ───────────────────────────────────────────────────

fn radio_search_schema() -> Value {
    serde_json::json!({
        "name": "radio_search",
        "description": "Search for internet radio stations via the Radio Browser API. Returns a list of stations with name, stream URL, country, genre tags, bitrate, and vote count.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search term: station name, genre, or tag depending on 'by'."
                },
                "by": {
                    "type": "string",
                    // "genre" is an accepted alias for "tag". Closes #8.
                    "enum": ["name", "tag", "genre"],
                    "description": "Search mode: 'name' searches by station name (default), 'tag'/'genre' searches by genre/tag."
                },
                "limit": {
                    // integer, not number. Closes #8.
                    "type": "integer",
                    "description": "Maximum results to return (1–50, default 10)."
                }
            },
            "required": ["query"]
        }
    })
}

fn radio_play_schema() -> Value {
    serde_json::json!({
        "name": "radio_play",
        "description": "Start playback of a radio station via mpv. Provide a direct stream URL (preferred) or a Radio Browser station UUID. Exactly one of 'url' or 'uuid' is required. Stops any currently-playing station first.",
        "inputSchema": {
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
            // url is required; uuid is an alternative — document the constraint. Closes #8.
            "required": ["url"]
        }
    })
}

fn radio_stop_schema() -> Value {
    serde_json::json!({
        "name": "radio_stop",
        // Updated — no longer kills all mpv instances. Closes #8.
        "description": "Stop the currently tracked radio station by terminating its mpv process. No-op if nothing is playing.",
        "inputSchema": {
            "type": "object",
            "properties": {}
        }
    })
}

fn radio_now_playing_schema() -> Value {
    serde_json::json!({
        "name": "radio_now_playing",
        "description": "Return the name and stream URL of the currently-playing station, or a message indicating nothing is playing.",
        "inputSchema": {
            "type": "object",
            "properties": {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_tools_returns_four() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools().await;
        let arr = tools.as_array().unwrap();
        assert_eq!(arr.len(), 4);
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"radio_search"));
        assert!(names.contains(&"radio_play"));
        assert!(names.contains(&"radio_stop"));
        assert!(names.contains(&"radio_now_playing"));
    }

    #[tokio::test]
    async fn test_tool_not_found() {
        let registry = ToolRegistry::new();
        let args = serde_json::json!({});
        let res = registry.execute_tool("nonexistent_tool", &args).await;
        assert!(res.is_err());
        let msg = format!("{}", res.err().unwrap());
        assert!(msg.contains("not found"));
    }

    #[tokio::test]
    async fn test_search_missing_query() {
        let registry = ToolRegistry::new();
        let args = serde_json::json!({});
        let res = registry.execute_tool("radio_search", &args).await;
        assert!(res.is_err());
        let msg = format!("{}", res.err().unwrap());
        assert!(msg.contains("query"));
    }

    #[tokio::test]
    async fn test_play_missing_url_and_uuid() {
        let registry = ToolRegistry::new();
        let args = serde_json::json!({});
        let res = registry.execute_tool("radio_play", &args).await;
        assert!(res.is_err());
        let msg = format!("{}", res.err().unwrap());
        assert!(msg.contains("url") || msg.contains("uuid"));
    }

    // Closes #7 — UUID validated before URL construction.
    #[tokio::test]
    async fn test_play_invalid_uuid_rejected() {
        let registry = ToolRegistry::new();
        let args = serde_json::json!({ "uuid": "../../etc/passwd" });
        let res = registry.execute_tool("radio_play", &args).await;
        assert!(res.is_err());
        let msg = format!("{}", res.err().unwrap());
        assert!(
            msg.to_lowercase().contains("uuid") || msg.to_lowercase().contains("invalid"),
            "unexpected error: {msg}"
        );
    }

    // Closes #8 — file:// URL rejected.
    #[tokio::test]
    async fn test_play_file_url_rejected() {
        let registry = ToolRegistry::new();
        let args = serde_json::json!({ "url": "file:///etc/passwd" });
        let res = registry.execute_tool("radio_play", &args).await;
        assert!(res.is_err());
        let msg = format!("{}", res.err().unwrap());
        assert!(
            msg.contains("http") || msg.contains("allowed"),
            "unexpected error: {msg}"
        );
    }

    // Closes #8 — stop when nothing is playing is a no-op.
    #[tokio::test]
    async fn test_stop_when_nothing_playing() {
        let registry = ToolRegistry::new();
        let res = registry
            .execute_tool("radio_stop", &serde_json::json!({}))
            .await;
        assert!(res.is_ok(), "stop with nothing playing should be a no-op");
        let (val, _) = res.unwrap();
        let text = val["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("stopped") || text.contains("Playback"));
    }

    #[tokio::test]
    async fn test_now_playing_default() {
        let registry = ToolRegistry::new();
        let (result, _) = registry
            .execute_tool("radio_now_playing", &serde_json::json!({}))
            .await
            .unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Nothing") || text.contains("playing"));
    }

    // Closes #8 — now_playing does not expose PID in output.
    #[tokio::test]
    async fn test_now_playing_no_pid_in_output() {
        let registry = ToolRegistry::new();
        let (result, _) = registry
            .execute_tool("radio_now_playing", &serde_json::json!({}))
            .await
            .unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(
            !text.contains("pid:"),
            "PID should not appear in now_playing output, got: {text}"
        );
    }

    // Closes #7 — malformed UUID (wrong length) returns error.
    #[tokio::test]
    async fn test_play_malformed_uuid_returns_error() {
        let registry = ToolRegistry::new();
        // 35 chars — wrong length
        let args = serde_json::json!({ "uuid": "550e8400-e29b-41d4-a716-44665544000" });
        let res = registry.execute_tool("radio_play", &args).await;
        assert!(res.is_err());
        let msg = format!("{}", res.err().unwrap());
        assert!(
            msg.to_lowercase().contains("uuid") || msg.to_lowercase().contains("invalid"),
            "expected uuid/invalid error, got: {msg}"
        );
    }

    // Closes #8 — limit schema uses integer type.
    #[test]
    fn test_search_schema_limit_is_integer() {
        let schema = radio_search_schema();
        let limit_type = schema["inputSchema"]["properties"]["limit"]["type"]
            .as_str()
            .unwrap();
        assert_eq!(limit_type, "integer");
    }

    // Closes #8 — radio_stop description is accurate.
    #[test]
    fn test_stop_schema_description_accurate() {
        let schema = radio_stop_schema();
        let desc = schema["description"].as_str().unwrap();
        assert!(
            !desc.contains("kills all mpv"),
            "description should not say 'kills all mpv', got: {desc}"
        );
    }

    // Closes #8 — genre alias is documented in the schema enum.
    #[test]
    fn test_search_schema_includes_genre_enum() {
        let schema = radio_search_schema();
        let by_enum = schema["inputSchema"]["properties"]["by"]["enum"]
            .as_array()
            .unwrap();
        let values: Vec<&str> = by_enum.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            values.contains(&"genre"),
            "schema should include 'genre' as a valid 'by' value"
        );
    }
}
