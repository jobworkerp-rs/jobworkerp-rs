use crate::handler::McpHandler;
use anyhow::Result;
use rmcp::{transport::stdio, ServiceExt};

/// Boot the MCP Server in Stdio mode.
///
/// This mode is suitable for integration with MCP clients that communicate
/// via stdin/stdout (e.g., Claude Desktop in stdio mode).
pub async fn boot_stdio_server(handler: McpHandler) -> Result<()> {
    tracing::info!("Starting MCP Stdio Server");

    let service = handler.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("MCP stdio serve error: {:?}", e);
    })?;

    service.waiting().await?;
    Ok(())
}
