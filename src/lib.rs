//! Internet Radio MCP — public API surface.

pub mod error;
pub mod models;
pub mod operations;
pub mod service;

pub use service::{RadioService, server_config};

/// Construct the internet-radio service with built-in defaults, for in-process (compiled-in) hosting.
pub fn build_service() -> RadioService {
    RadioService::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_core::McpService;

    #[test]
    fn build_service_exposes_tools() {
        let svc = build_service();
        assert!(
            !svc.tools().is_empty(),
            "radio build_service() must expose at least one tool"
        );
    }
}
