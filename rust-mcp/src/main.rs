use clap::Parser;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, service, transport};
use std::error::Error;
use std::io;

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server for Rust tools")]
struct Args {}

struct RustTools;

impl ServerHandler for RustTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().build()).with_server_info(
            Implementation::new("kid-agentic-coding-rust", env!("CARGO_PKG_VERSION")),
        )
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .try_init()
        .ok();
    tracing::debug!("rust logging initialized");

    let _args = Args::parse();
    let server = RustTools;
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;
    Ok(())
}
