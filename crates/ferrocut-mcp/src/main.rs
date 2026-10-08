//! `ferrocut-mcp`: the Ferrocut MCP server on stdio (stdout carries only
//! JSON-RPC; diagnostics go to stderr).
//!
//! `ferrocut-mcp [--root DIR]`: every path must be inside DIR (default
//! `$FERROCUT_MCP_ROOT`, else the working directory).

use std::path::PathBuf;

use rmcp::ServiceExt as _;

const USAGE: &str = "usage: ferrocut-mcp [--root DIR] [--list-tools] [--version]\n\
    Serves MCP over stdio. All paths must resolve inside DIR (default $FERROCUT_MCP_ROOT, else the cwd).";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut root: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--version" | "-V" => {
                println!("ferrocut-mcp {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--list-tools" => {
                let tools = ferrocut_mcp::tools();
                println!("{}", serde_json::to_string_pretty(&tools)?);
                return Ok(());
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--root" => match args.next() {
                Some(d) => root = Some(d.into()),
                None => anyhow::bail!("--root needs a directory\n{USAGE}"),
            },
            s if s.starts_with("--root=") => root = Some(s["--root=".len()..].into()),
            other => anyhow::bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    let root = ferrocut_mcp::Root::from_args(root)?;
    eprintln!("ferrocut-mcp: project root {}", root.dir().display());
    ferrocut_engine::media::init();
    let service = ferrocut_mcp::FerrocutServer::new(root)
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| eprintln!("ferrocut-mcp: failed to start: {e}"))?;
    service.waiting().await?;
    Ok(())
}
