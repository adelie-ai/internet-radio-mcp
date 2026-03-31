#![deny(warnings)]

// Radio Browser API search and mpv playback operations.

use crate::error::{RadioError, Result};
use crate::models::Station;

const RADIO_BROWSER_BASE: &str = "https://de1.api.radio-browser.info/json";
const USER_AGENT: &str = "AdelieInternetRadioMcp/0.1 (contact: local)";

/// Search for stations by name.
pub async fn search_by_name(
    client: &reqwest::Client,
    name: &str,
    limit: u32,
) -> Result<Vec<Station>> {
    let url = format!(
        "{}/stations/search?name={}&limit={}&hidebroken=true&order=votes",
        RADIO_BROWSER_BASE,
        urlenccode(name),
        limit
    );
    fetch_stations(client, &url, name).await
}

/// Search for stations by tag/genre.
pub async fn search_by_tag(
    client: &reqwest::Client,
    tag: &str,
    limit: u32,
) -> Result<Vec<Station>> {
    let url = format!(
        "{}/stations/search?tag={}&limit={}&hidebroken=true&order=votes",
        RADIO_BROWSER_BASE,
        urlenccode(tag),
        limit
    );
    fetch_stations(client, &url, tag).await
}

/// Look up a single station by its UUID.
pub async fn station_by_uuid(
    client: &reqwest::Client,
    uuid: &str,
) -> Result<Option<Station>> {
    let url = format!("{}/stations/byuuid/{}", RADIO_BROWSER_BASE, uuid);
    let stations = fetch_stations(client, &url, uuid).await?;
    Ok(stations.into_iter().next())
}

async fn fetch_stations(
    client: &reqwest::Client,
    url: &str,
    query: &str,
) -> Result<Vec<Station>> {
    let resp = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| RadioError::ApiError(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(RadioError::ApiError(format!(
            "HTTP {} from Radio Browser",
            resp.status()
        ))
        .into());
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

/// Minimal percent-encoding for URL query parameters (encodes spaces and special chars).
fn urlenccode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => {
                out.push('%');
                out.push(char::from_digit((other >> 4) as u32, 16).unwrap_or('0'));
                out.push(char::from_digit((other & 0xf) as u32, 16).unwrap_or('0'));
            }
        }
    }
    out
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

/// Play a station URL with mpv (non-blocking). Returns the spawned child PID.
pub fn play_station(url: &str) -> Result<u32> {
    use std::process::{Command, Stdio};

    validate_stream_url(url)?;

    let child = Command::new("mpv")
        .args(["--no-video", "--really-quiet", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| RadioError::PlayerError(format!("Failed to spawn mpv: {}", e)))?;

    let pid = child.id();

    // Detach: we intentionally do not wait on the child.
    std::mem::forget(child);

    Ok(pid)
}

/// Stop a specific mpv process by PID, or all tracked instances.
pub fn stop_playback_by_pid(pid: Option<u32>) -> Result<()> {
    if let Some(pid) = pid {
        // Kill a specific process by PID instead of blindly killing all mpv instances.
        #[cfg(unix)]
        {
            use std::process::Command;
            let status = Command::new("kill")
                .arg(pid.to_string())
                .status()
                .map_err(|e| RadioError::PlayerError(format!("kill failed: {e}")))?;
            if !status.success() {
                let code = status.code().unwrap_or(-1);
                // Exit code 1 from kill usually means "no such process" — already stopped.
                if code != 1 {
                    return Err(RadioError::PlayerError(format!(
                        "kill exited with unexpected status {code}"
                    ))
                    .into());
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            return Err(RadioError::PlayerError(
                "PID-based stop not supported on this platform".to_string(),
            )
            .into());
        }
    } else {
        // Fallback: stop all mpv instances (backward compatible).
        stop_all_mpv()?;
    }
    Ok(())
}

/// Stop all running mpv instances (legacy fallback).
fn stop_all_mpv() -> Result<()> {
    use std::process::Command;

    let status = Command::new("pkill")
        .arg("mpv")
        .status()
        .map_err(|e| RadioError::PlayerError(format!("pkill failed: {e}")))?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        if code != 1 {
            return Err(RadioError::PlayerError(format!(
                "pkill exited with unexpected status {code}"
            ))
            .into());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_urlenccode_passthrough() {
        assert_eq!(urlenccode("rock"), "rock");
        assert_eq!(urlenccode("classic rock"), "classic+rock");
        assert_eq!(urlenccode("jazz&blues"), "jazz%26blues");
    }

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
}
