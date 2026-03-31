#![deny(warnings)]

// Tool registry and MCP tool definitions.

use crate::error::{McpError, Result};
use crate::models::Station;
use crate::operations::radio;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

/// State for the currently-playing station (if any).
#[derive(Debug, Default)]
pub struct NowPlaying {
    pub pid: Option<u32>,
    pub station: Option<Station>,
}

/// Registry for all radio MCP tools.
pub struct ToolRegistry {
    http_client: Client,
    now_playing: Arc<RwLock<NowPlaying>>,
}

impl ToolRegistry {
    /// Create a new registry.
    pub fn new() -> Self {
        Self {
            http_client: Client::new(),
            now_playing: Arc::new(RwLock::new(NowPlaying::default())),
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
            _ => return Err(McpError::InvalidToolParameters("Arguments must be an object".to_string()).into()),
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

    async fn exec_radio_search(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<Value> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidToolParameters("Missing required parameter: query".to_string()))?;

        let by = args
            .get("by")
            .and_then(|v| v.as_str())
            .unwrap_or("name");

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .min(50) as u32;

        let stations = match by {
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

    async fn exec_radio_play(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<Value> {
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
                .ok_or_else(|| McpError::InvalidToolParameters(format!("Station UUID not found: {}", uuid)))?;
            let url = station.url_resolved.clone();
            let name = station.name.clone();
            (url, name)
        } else {
            return Err(McpError::InvalidToolParameters(
                "Provide either 'url' or 'uuid' to play a station".to_string(),
            )
            .into());
        };

        // Stop any current playback first (by tracked PID when available).
        let current_pid = self.now_playing.read().await.pid;
        let _ = radio::stop_playback_by_pid(current_pid);

        let pid = radio::play_station(&stream_url)?;

        // Record now-playing state.
        let mut np = self.now_playing.write().await;
        np.pid = Some(pid);
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
        let current_pid = self.now_playing.read().await.pid;
        radio::stop_playback_by_pid(current_pid)?;

        let mut np = self.now_playing.write().await;
        np.pid = None;
        np.station = None;

        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": "⏹ Playback stopped."
            }]
        }))
    }

    async fn exec_radio_now_playing(&self) -> Result<Value> {
        let np = self.now_playing.read().await;
        let text = match &np.station {
            Some(s) => format!(
                "▶ Now playing: {} — {} (pid: {})",
                s.name,
                s.url_resolved,
                np.pid.map_or_else(|| "?".to_string(), |p| p.to_string())
            ),
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
                    "enum": ["name", "tag"],
                    "description": "Search mode: 'name' searches by station name (default), 'tag' searches by genre/tag."
                },
                "limit": {
                    "type": "number",
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
        "description": "Start playback of a radio station via mpv. Provide a direct stream URL or a Radio Browser station UUID. Stops any currently-playing station first.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Direct stream URL to play (e.g. from radio_search results)."
                },
                "uuid": {
                    "type": "string",
                    "description": "Radio Browser station UUID; the server will resolve the stream URL."
                },
                "name": {
                    "type": "string",
                    "description": "Optional display name (used when 'url' is provided without a uuid lookup)."
                }
            }
        }
    })
}

fn radio_stop_schema() -> Value {
    serde_json::json!({
        "name": "radio_stop",
        "description": "Stop the currently-playing radio station (kills all mpv instances).",
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
}
