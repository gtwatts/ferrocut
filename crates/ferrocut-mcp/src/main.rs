//! `ferrocut-mcp`: the Ferrocut MCP server on stdio (stdout carries only
//! JSON-RPC; diagnostics go to stderr).

use rmcp::ServiceExt as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("ferrocut-mcp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if std::env::args().any(|a| a == "--list-tools") {
        let tools = ferrocut_mcp::tools();
        println!("{}", serde_json::to_string_pretty(&tools)?);
        return Ok(());
    }
    ferrocut_engine::media::init();
    let service = ferrocut_mcp::FerrocutServer
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| eprintln!("ferrocut-mcp: failed to start: {e}"))?;
    service.waiting().await?;
    Ok(())
}
