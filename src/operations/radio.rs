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

/// Play a station URL with mpv (non-blocking). Returns the spawned child PID.
pub fn play_station(url: &str) -> Result<u32> {
    use std::process::{Command, Stdio};

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

/// Stop all running mpv instances launched by this server.
pub fn stop_playback() -> Result<()> {
    use std::process::Command;

    // pkill exits 1 if no processes matched — treat that as "nothing was playing".
    let status = Command::new("pkill")
        .arg("mpv")
        .status()
        .map_err(|e| RadioError::PlayerError(format!("pkill failed: {}", e)))?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        if code != 1 {
            return Err(RadioError::PlayerError(format!(
                "pkill exited with unexpected status {}",
                code
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
}
