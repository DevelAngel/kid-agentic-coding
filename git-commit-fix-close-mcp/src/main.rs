use anyhow::Result;
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
use tokio::task;

use std::env;
use std::io::{self, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::process::{Command, Stdio};

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server for investigating and closing a commit-fix session")]
struct Args {
    /// Name of the abstract-namespace Unix socket used for workflow events.
    #[arg(long)]
    socket: String,
}

/// Stable name of the semantic event emitted when a commit-fix session closes.
const COMMIT_FIX_DONE_EVENT: &str = "commit-fix-done";

#[derive(Debug, Serialize)]
struct CommitFixDoneEvent<'a> {
    event: &'static str,
    commit_message: &'a str,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CommitParams {
    /// Commit message used verbatim.
    message: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AddParams {
    /// File or directory path relative to the workspace root.
    path: String,
}

#[derive(Debug)]
struct ProcessOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

#[derive(Clone)]
struct GitCommitFixCloseTools {
    socket_name: String,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl GitCommitFixCloseTools {
    fn new(socket_name: String) -> Self {
        Self {
            socket_name,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl GitCommitFixCloseTools {
    #[tool(
        description = "Shows the current Git status",
        annotations(
            title = "Git Status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn git_status(&self) -> Result<CallToolResult, McpError> {
        command_result("git", &["status", "--short"], "git status").await
    }

    #[tool(
        description = "Shows the current Git diff",
        annotations(
            title = "Git Diff",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn git_diff(&self) -> Result<CallToolResult, McpError> {
        command_result("git", &["diff"], "git diff").await
    }

    #[tool(
        description = "Stages a file or directory with Git",
        annotations(
            title = "Git Add",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn git_add(
        &self,
        Parameters(params): Parameters<AddParams>,
    ) -> Result<CallToolResult, McpError> {
        command_result("git", &["add", &params.path], "git add").await
    }

    #[tool(
        description = "Creates a Git commit with the given message and closes the commit-fix session",
        annotations(
            title = "Git Commit",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn git_commit(
        &self,
        Parameters(params): Parameters<CommitParams>,
    ) -> Result<CallToolResult, McpError> {
        let result =
            command_result("git", &["commit", "-m", &params.message], "git commit").await?;

        let event = CommitFixDoneEvent {
            event: COMMIT_FIX_DONE_EVENT,
            commit_message: &params.message,
        };
        let message = serde_json::to_vec(&event).map_err(|err| {
            McpError::internal_error(
                "failed to encode commit-fix-done event",
                Some(json!({"reason": err.to_string()})),
            )
        })?;
        notify_bridge(&self.socket_name, &message).map_err(|err| {
            tracing::error!(?err, "commit-fix-done event notification failed");
            McpError::internal_error(
                "failed to notify commit-fix-done bridge",
                Some(json!({"reason": err.to_string()})),
            )
        })?;
        tracing::debug!("commit-fix-done event sent");

        Ok(result)
    }
}

#[tool_handler]
impl ServerHandler for GitCommitFixCloseTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new(
                "kid-agentic-coding-git-commit-fix-close",
                env!("CARGO_PKG_VERSION"),
            )
            .with_title("Git Commit Fix Close"),
        )
    }
}

fn notify_bridge(socket_name: &str, message: &[u8]) -> io::Result<()> {
    let addr = SocketAddr::from_abstract_name(socket_name.as_bytes())?;
    let mut stream = UnixStream::connect_addr(&addr)?;
    stream.write_all(message)?;
    stream.write_all(b"\n")
}

async fn command_result(
    program: &str,
    args: &[&str],
    operation: &str,
) -> Result<CallToolResult, McpError> {
    let output = run_process(program, args, operation).await?;
    if output.status == 0 {
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "{}{}",
            output.stdout, output.stderr
        ))]))
    } else {
        Err(McpError::internal_error(
            format!("{operation} failed: {}", output.stderr.trim()),
            Some(json!({
                "status": output.status,
                "stdout": output.stdout,
                "stderr": output.stderr,
            })),
        ))
    }
}

async fn run_process(
    program: &str,
    args: &[&str],
    operation: &str,
) -> Result<ProcessOutput, McpError> {
    let workspace_root = env::current_dir().map_err(|err| {
        McpError::internal_error(
            format!("failed to determine working directory for {operation}"),
            Some(json!({"reason": err.to_string()})),
        )
    })?;
    let program = program.to_owned();
    let args = args.iter().map(ToString::to_string).collect::<Vec<_>>();
    let output = task::spawn_blocking(move || {
        Command::new(program)
            .args(args)
            .current_dir(workspace_root)
            .stdin(Stdio::null())
            .output()
    })
    .await
    .map_err(|err| {
        McpError::internal_error(
            format!("failed to run {operation}"),
            Some(json!({"reason": err.to_string()})),
        )
    })?
    .map_err(|err| {
        McpError::internal_error(
            format!("failed to execute {operation}"),
            Some(json!({"reason": err.to_string()})),
        )
    })?;

    Ok(ProcessOutput {
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .try_init()
        .map_err(|err| anyhow::anyhow!("failed to initialize logging: {err}"))?;
    tracing::debug!("git logging initialized");

    let args = Args::parse();
    let server = GitCommitFixCloseTools::new(args.socket);
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_result_includes_stderr_when_command_fails() {
        let error = command_result(
            "sh",
            &["-c", "printf 'commit failed' >&2; exit 1"],
            "git commit",
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("git commit failed: commit failed")
        );
    }
}
