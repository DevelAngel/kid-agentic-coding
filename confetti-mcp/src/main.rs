//! Standalone stdio MCP server exposing the `confetti` tool.
//!
//! Registered by `kid-agentic-coding` as a classic MCP server for agents
//! that cannot use MCP-over-ACP. Each invocation notifies the parent
//! `kid-agentic-coding` process over a Linux abstract-namespace Unix
//! socket, so the TUI (running in a different process) can trigger its
//! confetti animation.

use anyhow::Result;
use anyhow::anyhow;
use clap::Parser;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData as McpError, ServerHandler};
use rmcp::{service, tool, tool_handler, tool_router, transport};
use serde_json::json;

use std::io::{self, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};

/// The message written to the bridge socket on every `confetti` invocation.
/// Fixed content: the socket carries exactly one kind of event.
const NOTIFY_MESSAGE: &[u8] = b"confetti\n";

#[derive(Parser, Debug)]
#[command(about = "Standalone MCP server exposing the confetti tool")]
struct Args {
    /// Name of the abstract-namespace Unix socket the parent
    /// kid-agentic-coding process listens on for confetti notifications.
    #[arg(long)]
    socket: String,
}

#[derive(Clone)]
struct ConfettiTools {
    socket_name: String,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl ConfettiTools {
    fn new(socket_name: String) -> Self {
        Self {
            socket_name,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl ConfettiTools {
    #[tool(
        description = "Triggers a confetti celebration",
        annotations(
            title = "Confetti Celebration",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn confetti(&self) -> Result<CallToolResult, McpError> {
        tracing::info!("confetti tool invoked");
        notify_bridge(&self.socket_name).map_err(|err| {
            tracing::error!(?err, "confetti tool failed");
            McpError::internal_error(
                "failed to notify confetti bridge",
                Some(json!({"reason": err.to_string()})),
            )
        })?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            "confetti invoked",
        )]))
    }
}

#[tool_handler]
impl ServerHandler for ConfettiTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("kid-agentic-coding-confetti", env!("CARGO_PKG_VERSION"))
                .with_title("Confetti"),
        )
    }
}

/// Connects to the parent process's abstract-namespace bridge socket and
/// writes the fixed notify message. A short-lived blocking connection is
/// simplest here: one write per invocation, no response expected.
fn notify_bridge(socket_name: &str) -> io::Result<()> {
    let addr = SocketAddr::from_abstract_name(socket_name.as_bytes())?;
    let mut stream = UnixStream::connect_addr(&addr)?;
    Write::write_all(&mut stream, NOTIFY_MESSAGE)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .try_init()
        .map_err(|err| anyhow!("failed to initialize logging: {err}"))?;
    tracing::debug!("confetti logging initialized");

    let args = Args::parse();
    let server = ConfettiTools::new(args.socket);
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;
    Ok(())
}
