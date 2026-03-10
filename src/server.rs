#![deny(warnings)]

// MCP server implementation

use crate::error::{McpError, Result};
use crate::tools::ToolRegistry;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

/// MCP server state
pub struct McpServer {
    tool_registry: Arc<ToolRegistry>,
    initialized: Arc<RwLock<bool>>,
}

impl McpServer {
    /// Create a new MCP server
    pub fn new() -> Self {
        Self {
            tool_registry: Arc::new(ToolRegistry::new()),
            initialized: Arc::new(RwLock::new(false)),
        }
    }

    /// Handle initialize request
    pub async fn handle_initialize(
        &self,
        protocol_version: &str,
        _client_capabilities: &Value,
    ) -> Result<Value> {
        if protocol_version != "2024-11-05"
            && protocol_version != "2025-06-18"
            && protocol_version != "2025-11-25"
        {
            return Err(McpError::InvalidProtocolVersion(protocol_version.to_string()).into());
        }

        let tools = self.tool_registry.list_tools().await;

        Ok(serde_json::json!({
            "protocolVersion": protocol_version,
            "serverInfo": {
                "name": "internet-radio-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": {
                    "listChanged": false,
                },
            },
            "tools": tools,
        }))
    }

    /// Handle initialized notification
    pub async fn handle_initialized(&self) -> Result<()> {
        let mut initialized = self.initialized.write().await;
        *initialized = true;
        Ok(())
    }

    /// Handle tool call. Returns (result, tools_changed).
    pub async fn handle_tool_call(
        &self,
        tool_name: &str,
        arguments: &Value,
    ) -> Result<(Value, bool)> {
        self.tool_registry.execute_tool(tool_name, arguments).await
    }

    /// Handle shutdown request
    pub async fn handle_shutdown(&self) -> Result<()> {
        let mut initialized = self.initialized.write().await;
        *initialized = false;
        Ok(())
    }

    /// List tools in MCP schema format
    pub async fn list_tools(&self) -> Value {
        self.tool_registry.list_tools().await
    }

    /// Check if server is initialized
    pub async fn is_initialized(&self) -> bool {
        *self.initialized.read().await
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}
