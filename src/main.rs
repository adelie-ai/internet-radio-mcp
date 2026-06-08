//! internet-radio-mcp binary entry point.
//!
//! Delegates everything to mcp-core; this file is now just a one-liner.

use internet_radio_mcp::service::RadioService;
use mcp_core::{ServerConfig, run_simple};

#[tokio::main]
async fn main() -> mcp_core::Result<()> {
    let config =
        ServerConfig::new("internet-radio-mcp", env!("CARGO_PKG_VERSION")).without_websocket();
    run_simple(config, || async { Ok(RadioService::new()) }).await
}
