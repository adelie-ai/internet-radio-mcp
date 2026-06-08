//! Domain errors for internet-radio-mcp.
//!
//! Transport and protocol errors are owned by mcp-core; only radio-specific
//! and parameter errors live here.

use thiserror::Error;

/// Errors related to radio operations (search, playback, etc.)
#[derive(Error, Debug)]
pub enum RadioError {
    /// Radio Browser API request failed.
    #[error("Radio Browser API error: {0}")]
    ApiError(String),

    /// mpv process management error.
    #[error("Player error: {0}")]
    PlayerError(String),

    /// No stations matched the search.
    #[error("No stations found for query: {0}")]
    NoStationsFound(String),
}

/// Errors related to tool parameter validation.
#[derive(Error, Debug)]
pub enum McpError {
    /// Requested tool name does not exist.
    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    /// Tool parameters were missing or invalid.
    #[error("Invalid tool parameters: {0}")]
    InvalidToolParameters(String),
}

/// Top-level error type used across the crate.
#[derive(Error, Debug)]
pub enum InternetRadioMcpError {
    /// Tool parameter / validation errors.
    #[error("MCP error: {0}")]
    Mcp(#[from] McpError),

    /// Radio operation errors.
    #[error("Radio error: {0}")]
    Radio(#[from] RadioError),

    /// JSON serialization/deserialization errors.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Underlying I/O errors.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience result alias for crate APIs.
pub type Result<T> = std::result::Result<T, InternetRadioMcpError>;
