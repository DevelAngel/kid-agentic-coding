//! Standalone stdio MCP server exposing the `confetti` tool.
//!
//! Registered by `kid-agentic-coding` as a classic MCP server for agents
//! that cannot use MCP-over-ACP. Each invocation notifies the parent
//! `kid-agentic-coding` process over a Unix socket (a Linux
//! abstract-namespace socket, or a filesystem socket when the agent is
//! sandboxed), so the TUI (running in a different process) can trigger its
//! confetti animation.

use anyhow::Result;
use anyhow::anyhow;
use clap::Parser;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData as McpError, ServerHandler};
use rmcp::{service, tool, tool_handler, tool_router, transport};
use serde_json::json;
use wire::{Confetti, bridge_error, connect_to_bridge, send_line};

use std::io;

#[derive(Parser, Debug)]
#[command(about = "Standalone MCP server exposing the confetti tool")]
struct Args {
    /// Name or path of the Unix socket the parent kid-agentic-coding
    /// process listens on for confetti notifications.
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
            let message = bridge_error(&self.socket_name, &err);
            tracing::error!("{message}");
            McpError::internal_error(
                "failed to notify confetti bridge",
                Some(json!({"reason": message})),
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

/// Connects to the parent process's bridge socket and writes the fixed
/// notify message. A short-lived blocking connection is simplest here: one
/// write per invocation, no response expected.
fn notify_bridge(socket: &str) -> io::Result<()> {
    let mut stream = connect_to_bridge(socket)?;
    send_line(&mut stream, Confetti.to_line())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .try_init()
        .map_err(|err| anyhow!("failed to initialize logging: {err}"))?;
    tracing::debug!("confetti logging initialized");

    let args = Args::parse();
    // Probe the bridge socket, but never abort on failure: exiting here
    // would leave the agent waiting for this server to become ready, and
    // server stderr is not reliably visible to the user anyway. Tool calls
    // report the same error when they need the bridge.
    if let Err(err) = connect_to_bridge(&args.socket) {
        tracing::error!("{}", bridge_error(&args.socket, &err));
    }
    let server = ConfettiTools::new(args.socket);
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;
    Ok(())
}
