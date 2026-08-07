#![deny(warnings)]

// Radio Browser API search and mpv playback operations.

use crate::error::{McpError, RadioError, Result};
use crate::models::Station;
use std::process::Child;

/// The live Radio Browser directory. [`crate::service::RadioService::new`]
/// points here by default; a test points at a local mock server instead
/// (mcp-core#40: no test may reach a live directory or stream).
pub(crate) const RADIO_BROWSER_BASE: &str = "https://de1.api.radio-browser.info/json";
const USER_AGENT: &str = "AdelieInternetRadioMcp/0.1 (contact: local)";

/// Search for stations by name.
pub async fn search_by_name(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
    limit: u32,
) -> Result<Vec<Station>> {
    search_stations(client, base_url, "name", name, limit).await
}

/// Search for stations by tag/genre.
pub async fn search_by_tag(
    client: &reqwest::Client,
    base_url: &str,
    tag: &str,
    limit: u32,
) -> Result<Vec<Station>> {
    search_stations(client, base_url, "tag", tag, limit).await
}

async fn search_stations(
    client: &reqwest::Client,
    base_url: &str,
    field: &str,
    query: &str,
    limit: u32,
) -> Result<Vec<Station>> {
    let url = format!("{base_url}/stations/search");
    let limit_str = limit.to_string();
    let params = [
        (field, query),
        ("limit", &limit_str),
        ("hidebroken", "true"),
        ("order", "votes"),
    ];

    let resp = client
        .get(&url)
        .query(&params)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| RadioError::ApiError(e.to_string()))?;

    parse_stations_response(resp, query).await
}

/// Validate that a UUID matches the expected `[0-9a-fA-F-]{36}` format.
///
/// This prevents malformed input from altering the Radio Browser request URL.
/// Closes #7.
pub fn validate_uuid(uuid: &str) -> Result<()> {
    if uuid.len() != 36 {
        return Err(McpError::InvalidToolParameters(format!(
            "UUID must be 36 characters, got {}",
            uuid.len()
        ))
        .into());
    }
    if !uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(McpError::InvalidToolParameters(format!(
            "UUID contains invalid characters (expected [0-9a-fA-F-]): {uuid}"
        ))
        .into());
    }
    Ok(())
}

/// Look up a single station by its UUID.
pub async fn station_by_uuid(
    client: &reqwest::Client,
    base_url: &str,
    uuid: &str,
) -> Result<Option<Station>> {
    // Validate before injecting into the URL. Closes #7.
    validate_uuid(uuid)?;

    let url = format!("{base_url}/stations/byuuid/{uuid}");

    let resp = client
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| RadioError::ApiError(e.to_string()))?;

    // NoStationsFound is a valid "not found" result for a UUID lookup.
    match parse_stations_response(resp, uuid).await {
        Ok(stations) => Ok(stations.into_iter().next()),
        Err(e) => {
            // Treat "no stations" as None rather than an error for UUID lookups.
            if e.to_string().contains("No stations found") {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

async fn parse_stations_response(resp: reqwest::Response, query: &str) -> Result<Vec<Station>> {
    if !resp.status().is_success() {
        return Err(
            RadioError::ApiError(format!("HTTP {} from Radio Browser", resp.status())).into(),
        );
    }

    let stations: Vec<Station> = resp
        .json()
        .await
        .map_err(|e| RadioError::ApiError(e.to_string()))?;

    if stations.is_empty() {
        return Err(RadioError::NoStationsFound(query.to_string()).into());
    }

    Ok(stations)
}

/// Validate that a stream URL uses HTTP or HTTPS.
fn validate_stream_url(url: &str) -> Result<()> {
    // Reject non-HTTP schemes (file://, ftp://, etc.) to prevent local file access.
    let lower = url.to_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err(RadioError::PlayerError(format!(
            "only http:// and https:// stream URLs are allowed, got: {}",
            url.split("://").next().unwrap_or("unknown")
        ))
        .into());
    }
    Ok(())
}

/// Play a station URL with mpv (non-blocking).
///
/// Returns the spawned `Child` handle so the caller can store it and later stop
/// or reap the process without zombie risk or PID-reuse hazards. Closes #5.
pub fn play_station(url: &str) -> Result<Child> {
    use std::process::{Command, Stdio};

    validate_stream_url(url)?;

    let child = Command::new("mpv")
        .args(["--no-video", "--really-quiet", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| RadioError::PlayerError(format!("Failed to spawn mpv: {}", e)))?;

    Ok(child)
}

/// Stop a specific mpv process represented by its `Child` handle.
///
/// Sends SIGTERM via `nix` (no subprocess spawning, no PATH dependency) and
/// immediately reaps the child to prevent zombies. Closes #5, Closes #6.
#[cfg(unix)]
pub fn stop_child(mut child: Child) -> Result<()> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    // Check if it already exited before trying to kill it.
    match child.try_wait() {
        Ok(Some(_)) => {
            // Process already exited — nothing to kill, already reaped by try_wait.
            return Ok(());
        }
        Ok(None) => {
            // Still running; send SIGTERM.
            let pid = Pid::from_raw(child.id() as i32);
            match kill(pid, Signal::SIGTERM) {
                Ok(()) => {}
                Err(nix::errno::Errno::ESRCH) => {
                    // Raced with process exit — not an error.
                }
                Err(e) => {
                    return Err(RadioError::PlayerError(format!("SIGTERM failed: {e}")).into());
                }
            }
        }
        Err(e) => {
            return Err(RadioError::PlayerError(format!("try_wait failed: {e}")).into());
        }
    }

    // Reap the child (prevents zombie). Ignore any error from wait since the
    // process may have already exited between SIGTERM and here.
    let _ = child.wait();
    Ok(())
}

#[cfg(not(unix))]
pub fn stop_child(mut child: Child) -> Result<()> {
    child
        .kill()
        .map_err(|e| RadioError::PlayerError(format!("kill failed: {e}")))?;
    let _ = child.wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_stream_url_accepts_http() {
        assert!(validate_stream_url("http://stream.example.com/radio.mp3").is_ok());
        assert!(validate_stream_url("https://stream.example.com/radio.mp3").is_ok());
    }

    #[test]
    fn test_validate_stream_url_rejects_non_http() {
        assert!(validate_stream_url("file:///etc/passwd").is_err());
        assert!(validate_stream_url("ftp://example.com/radio.mp3").is_err());
        assert!(validate_stream_url("rtsp://example.com/stream").is_err());
    }

    // Closes #7 — UUID validation
    #[test]
    fn test_validate_uuid_accepts_valid() {
        assert!(validate_uuid("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(validate_uuid("00000000-0000-0000-0000-000000000000").is_ok());
    }

    #[test]
    fn test_validate_uuid_rejects_short() {
        assert!(validate_uuid("550e8400-e29b-41d4").is_err());
    }

    #[test]
    fn test_validate_uuid_rejects_invalid_chars() {
        assert!(validate_uuid("550e8400-e29b-41d4-a716-44665544000!").is_err());
        // Path traversal attempt
        assert!(validate_uuid("../../etc/passwd!!!!!!!!!!!!!!!!!!!").is_err());
    }

    #[test]
    fn test_validate_uuid_rejects_injection() {
        // Query-string injection attempt (36 chars, but contains '?')
        assert!(validate_uuid("550e8400-e29b-41d4-a716-4466554?0000").is_err());
    }

    // ── telemetry: outbound-call debug logging (mcp-core#40) ───────────────
    //
    // A station name, a search query, or a stream URL is a tool argument --
    // content, per epic D10 -- so each outbound call logs it at DEBUG and
    // nowhere louder. A minimal capturing layer proves it directly, the way
    // web-mcp proves the same property for its own single outbound call.
    mod outbound_debug_logging {
        use super::*;
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex};
        use tracing::Level;
        use tracing::field::{Field, Visit};
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::{Context, SubscriberExt};

        type LoggedEvent = (Level, BTreeMap<String, String>);

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<LoggedEvent>>>);

        struct Collector<'a>(&'a mut BTreeMap<String, String>);

        impl Visit for Collector<'_> {
            fn record_str(&mut self, field: &Field, value: &str) {
                self.0.insert(field.name().to_string(), value.to_string());
            }

            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                self.0
                    .insert(field.name().to_string(), format!("{value:?}"));
            }
        }

        impl<S: tracing::Subscriber> Layer<S> for Capture {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                let mut fields = BTreeMap::new();
                event.record(&mut Collector(&mut fields));
                self.0
                    .lock()
                    .expect("capture lock is only held to push one record")
                    .push((*event.metadata().level(), fields));
            }
        }

        /// AC (mcp-core#40): a call to the Radio Browser directory logs the
        /// search query at DEBUG, and only at DEBUG. The server never speaks
        /// to the real directory in a test (mcp-core#40): `MockServer` stands
        /// in for it.
        #[test]
        fn search_stations_logs_the_query_at_debug_only() {
            let server = httpmock::MockServer::start();
            let _mock = server.mock(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/json/stations/search");
                then.status(200).json_body(serde_json::json!([{
                    "stationuuid": "00000000-0000-4000-8000-000000000001",
                    "name": "Test Station",
                    "url_resolved": "https://stream.example.com/test",
                }]));
            });
            let base_url = format!("{}/json", server.base_url());
            let query = "MARKER-log-search-9f3d1c2a";

            let capture = Capture::default();
            let subscriber = tracing_subscriber::registry().with(capture.clone());
            tracing::subscriber::with_default(subscriber, || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("a current-thread runtime");
                let client = reqwest::Client::new();
                runtime.block_on(async {
                    let _ = search_by_name(&client, &base_url, query, 5).await;
                });
            });

            let events = capture
                .0
                .lock()
                .expect("capture lock is only held to push one record");
            // reqwest/hyper emit their own TRACE/DEBUG connection-pool events
            // on the same subscriber, so this looks for the specific event
            // this function logs (carrying a `query` field) rather than
            // asserting a total event count.
            let query_events: Vec<_> = events
                .iter()
                .filter(|(_, fields)| fields.contains_key("query"))
                .collect();
            assert_eq!(
                query_events.len(),
                1,
                "querying the radio directory must log exactly one event carrying the query \
                 field: {events:?}"
            );
            let (level, fields) = query_events[0];
            assert_eq!(
                *level,
                Level::DEBUG,
                "the outbound directory query must log at DEBUG, so it stays off the INFO band"
            );
            assert_eq!(
                fields.get("query").map(String::as_str),
                Some(query),
                "the event must carry the query that was searched"
            );
            assert!(
                events.iter().all(|(level, fields)| {
                    *level > Level::INFO || !fields.values().any(|v| v.contains(query))
                }),
                "the query must never reach an INFO-or-louder line: {events:?}"
            );
        }

        /// AC (mcp-core#40): an attempt to play a stream logs the url at
        /// DEBUG, and only at DEBUG -- even when the url is then locally
        /// rejected (no network reached either way, so this stays
        /// deterministic and offline).
        #[test]
        fn play_station_logs_the_url_at_debug_only() {
            let url = "ftp://forbidden.example.com/MARKER-log-play-9f3d1c2a";

            let capture = Capture::default();
            let subscriber = tracing_subscriber::registry().with(capture.clone());
            let result = tracing::subscriber::with_default(subscriber, || play_station(url));

            assert!(
                result.is_err(),
                "a disallowed stream scheme must still be rejected"
            );

            let events = capture
                .0
                .lock()
                .expect("capture lock is only held to push one record");
            assert_eq!(
                events.len(),
                1,
                "attempting playback must log exactly one event: {events:?}"
            );
            let (level, fields) = &events[0];
            assert_eq!(
                *level,
                Level::DEBUG,
                "the outbound playback attempt must log at DEBUG, so it stays off the INFO band"
            );
            assert_eq!(
                fields.get("url").map(String::as_str),
                Some(url),
                "the event must carry the url that was requested"
            );
        }
    }
}
