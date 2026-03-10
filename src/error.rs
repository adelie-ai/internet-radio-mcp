#![deny(warnings)]

// Central error types for internet-radio-mcp.

use thiserror::Error;

/// Top-level error type used across the crate.
#[derive(Error, Debug)]
pub enum InternetRadioMcpError {
    /// MCP protocol errors.
    #[error("MCP protocol error: {0}")]
    Mcp(#[from] McpError),

    /// Transport-layer errors.
    #[error("Transport error: {0}")]
    Transport(#[from] TransportError),

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

/// Errors related to MCP/JSON-RPC semantics.
#[derive(Error, Debug)]
pub enum McpError {
    /// Unsupported MCP protocol version in initialize request.
    #[error("Unsupported protocol version: {0}")]
    InvalidProtocolVersion(String),

    /// Requested tool name does not exist.
    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    /// Tool parameters were missing or invalid.
    #[error("Invalid tool parameters: {0}")]
    InvalidToolParameters(String),
}

/// Transport-level framing and connection errors.
#[derive(Error, Debug)]
pub enum TransportError {
    /// WebSocket connection error.
    #[error("WebSocket connection error: {0}")]
    WebSocket(String),

    /// Incoming message framing or format was invalid.
    #[error("Invalid message format: {0}")]
    InvalidMessage(String),

    /// Transport stream was closed by peer.
    #[error("Connection closed")]
    ConnectionClosed,

    /// I/O error while reading or writing transport data.
    #[error("Transport IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience result alias for crate APIs.
pub type Result<T> = std::result::Result<T, InternetRadioMcpError>;
