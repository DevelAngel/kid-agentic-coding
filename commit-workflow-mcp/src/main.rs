use anyhow::Result;
use anyhow::anyhow;
use clap::Parser;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::schemars::JsonSchema;
use rmcp::serde::{Deserialize, Serialize};
use rmcp::{
    ErrorData as McpError, ServerHandler, service, tool, tool_handler, tool_router, transport,
};
use serde_json::json;

use std::io::{self, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server for the git_commit_with_check tool")]
struct Args {
    /// Name of the abstract-namespace Unix socket used for workflow events.
    #[arg(long)]
    socket: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GitCommitWithCheckParams {
    /// The commit message to carry into the fix session.
    message: String,
}

#[derive(Debug, Serialize)]
struct CommitFixEvent<'a> {
    event: &'static str,
    instructions: &'static str,
    commit_message: &'a str,
}

const COMMIT_FIX_EVENT: &str = "commit-fix";
const COMMIT_FIX_INSTRUCTIONS: &str = "The main session requested a commit-fix session. Investigate the current problem, fix the underlying issue, run the provided check, lint, and test tools, and commit the resulting changes with the supplied commit message using the available Git tools.";

#[derive(Clone)]
struct CommitWorkflowTools {
    socket_name: String,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl CommitWorkflowTools {
    fn new(socket_name: String) -> Self {
        Self {
            socket_name,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl CommitWorkflowTools {
    #[tool(
        description = "Requests a commit-fix session for the current work",
        annotations(
            title = "Git Commit With Check",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn git_commit_with_check(
        &self,
        Parameters(params): Parameters<GitCommitWithCheckParams>,
    ) -> Result<CallToolResult, McpError> {
        let event = CommitFixEvent {
            event: COMMIT_FIX_EVENT,
            instructions: COMMIT_FIX_INSTRUCTIONS,
            commit_message: &params.message,
        };
        let message = serde_json::to_vec(&event).map_err(|err| {
            McpError::internal_error(
                "failed to encode commit-fix event",
                Some(json!({"reason": err.to_string()})),
            )
        })?;
        notify_bridge(&self.socket_name, &message).map_err(|err| {
            tracing::error!(?err, "commit-fix event notification failed");
            McpError::internal_error(
                "failed to notify commit-fix bridge",
                Some(json!({"reason": err.to_string()})),
            )
        })?;

        Ok(CallToolResult::success(vec![ContentBlock::text(
            "commit-fix session requested",
        )]))
    }
}

#[tool_handler]
impl ServerHandler for CommitWorkflowTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new(
                "kid-agentic-coding-commit-workflow",
                env!("CARGO_PKG_VERSION"),
            )
            .with_title("Commit Workflow"),
        )
    }
}

fn notify_bridge(socket_name: &str, message: &[u8]) -> io::Result<()> {
    let addr = SocketAddr::from_abstract_name(socket_name.as_bytes())?;
    let mut stream = UnixStream::connect_addr(&addr)?;
    stream.write_all(message)?;
    stream.write_all(b"\n")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .try_init()
        .map_err(|err| anyhow!("failed to initialize logging: {err}"))?;
    tracing::debug!("commit-workflow logging initialized");

    let args = Args::parse();
    let server = CommitWorkflowTools::new(args.socket);
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{COMMIT_FIX_EVENT, COMMIT_FIX_INSTRUCTIONS, CommitFixEvent};

    #[test]
    fn commit_fix_event_contains_workflow_instructions_and_message() {
        let event = CommitFixEvent {
            event: COMMIT_FIX_EVENT,
            instructions: COMMIT_FIX_INSTRUCTIONS,
            commit_message: "feat: preserve workflow",
        };

        let value = serde_json::to_value(event).expect("event is serializable");

        assert_eq!(value["event"], COMMIT_FIX_EVENT);
        assert_eq!(value["instructions"], COMMIT_FIX_INSTRUCTIONS);
        assert_eq!(value["commit_message"], "feat: preserve workflow");
    }
}
