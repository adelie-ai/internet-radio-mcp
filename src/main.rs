//! internet-radio-mcp binary entry point.
//!
//! Delegates everything to mcp-core; this file is now just a one-liner.

use internet_radio_mcp::{build_service, server_config};
use mcp_core::run_simple;

#[tokio::main]
async fn main() -> mcp_core::Result<()> {
    run_simple(server_config(), || async { Ok(build_service()) }).await
}
