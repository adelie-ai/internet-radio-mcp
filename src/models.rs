#![deny(warnings)]

// Radio station data models.

use serde::{Deserialize, Serialize};

/// A radio station as returned by the Radio Browser API.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Station {
    /// Unique station UUID on Radio Browser.
    #[serde(rename = "stationuuid")]
    pub uuid: String,

    /// Display name.
    pub name: String,

    /// Resolved direct stream URL (preferred over `url`).
    pub url_resolved: String,

    /// Country name.
    #[serde(default)]
    pub country: String,

    /// Comma-separated genre/tag list.
    #[serde(default)]
    pub tags: String,

    /// Stream bitrate in kbps (0 = unknown).
    #[serde(default)]
    pub bitrate: u32,

    /// Audio codec.
    #[serde(default)]
    pub codec: String,

    /// Community vote count.
    #[serde(default)]
    pub votes: i64,
}
