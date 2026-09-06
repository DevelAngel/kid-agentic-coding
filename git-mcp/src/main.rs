use anyhow::Result;
use clap::Parser;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::schemars::JsonSchema;
use rmcp::serde::Deserialize;
use rmcp::{
    ErrorData as McpError, ServerHandler, service, tool, tool_handler, tool_router, transport,
};
use serde_json::json;
use std::env;
use std::process::{Command, Stdio};
use tokio::task;

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server exposing Git tools")]
struct Args {}

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
struct GitTools {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl Default for GitTools {
    fn default() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl GitTools {
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
        description = "Creates a Git commit with the given message",
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
        command_result("git", &["commit", "-m", &params.message], "git commit").await
    }
}

#[tool_handler]
impl ServerHandler for GitTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("kid-agentic-coding-git", env!("CARGO_PKG_VERSION"))
                .with_title("Git Tools"),
        )
    }
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
            format!("{operation} failed"),
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
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|err| anyhow::anyhow!("failed to initialize logging: {err}"))?;
    let _args = Args::parse();
    let server = GitTools::default();
    let transport = transport::io::stdio();
    let _running = service::serve_server(server, transport).await?;
    Ok(())
}
