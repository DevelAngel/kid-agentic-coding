use anyhow::Result;
use anyhow::anyhow;
use clap::Parser;
use derive_more::Display;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::schemars::JsonSchema;
use rmcp::serde::{Deserialize, Serialize};
use rmcp::{
    ErrorData as McpError, Json, ServerHandler, service, tool, tool_handler, tool_router, transport,
};
use tokio::task;

use std::env;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GH: &str = "gh";

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server for GitHub issue tools backed by the gh CLI")]
struct Args {}

#[derive(Debug, Serialize, JsonSchema)]
struct GhToolResult {
    status: i32,
    stdout: String,
    stderr: String,
}

#[derive(Serialize)]
struct GhToolError {
    error: String,
    reason: String,
}

impl GhToolError {
    fn into_json_value(self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum GhIssueState {
    #[display("open")]
    Open,
    #[display("closed")]
    Closed,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GhIssueListParams {
    /// Issue state: "open" or "closed".
    state: GhIssueState,
    /// Working directory gh runs in; the repository is resolved from it.
    cwd: Option<PathBuf>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GhIssueViewParams {
    /// Issue number (without the #).
    issue_number: u32,
    /// Working directory gh runs in; the repository is resolved from it.
    cwd: Option<PathBuf>,
}

#[derive(Clone)]
struct GhTools {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl Default for GhTools {
    fn default() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl GhTools {
    #[tool(
        description = "List GitHub issues in the given state (open or closed)",
        annotations(
            title = "GitHub Issue List",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn gh_issue_list(
        &self,
        Parameters(params): Parameters<GhIssueListParams>,
    ) -> Result<Json<GhToolResult>, McpError> {
        let args = vec![
            "issue".to_string(),
            "list".to_string(),
            "--state".to_string(),
            params.state.to_string(),
            "--json".to_string(),
            "number,issueType,title".to_string(),
        ];
        run_gh(args, params.cwd.as_deref()).await
    }

    #[tool(
        description = "View a single GitHub issue including its body, milestone and comments",
        annotations(
            title = "GitHub Issue View",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn gh_issue_view(
        &self,
        Parameters(params): Parameters<GhIssueViewParams>,
    ) -> Result<Json<GhToolResult>, McpError> {
        let args = vec![
            "issue".to_string(),
            "view".to_string(),
            params.issue_number.to_string(),
            "--json".to_string(),
            "number,state,title,body,milestone,comments".to_string(),
        ];
        run_gh(args, params.cwd.as_deref()).await
    }
}

async fn run_gh(args: Vec<String>, cwd: Option<&Path>) -> Result<Json<GhToolResult>, McpError> {
    let workspace_root = env::current_dir().map_err(|err| {
        McpError::internal_error(
            "failed to determine working directory",
            GhToolError {
                error: "failed to determine working directory".to_string(),
                reason: err.to_string(),
            }
            .into_json_value(),
        )
    })?;
    let working_directory = cwd.map(PathBuf::from).unwrap_or(workspace_root);

    let output = task::spawn_blocking(move || {
        Command::new(GH)
            .args(&args)
            .current_dir(working_directory)
            .stdin(Stdio::null())
            .output()
    })
    .await
    .map_err(|err| {
        tracing::error!(?err, "gh task failed");
        McpError::internal_error(
            "failed to run gh",
            GhToolError {
                error: "failed to run gh".to_string(),
                reason: err.to_string(),
            }
            .into_json_value(),
        )
    })?;

    let output = output.map_err(|err| {
        tracing::error!(?err, "gh failed to execute");
        let reason = if err.kind() == ErrorKind::NotFound {
            "gh is not installed".to_string()
        } else {
            err.to_string()
        };
        McpError::internal_error(
            "failed to execute gh",
            GhToolError {
                error: "failed to execute gh".to_string(),
                reason,
            }
            .into_json_value(),
        )
    })?;

    let status = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(Json(GhToolResult {
        status,
        stdout: stdout.into_owned(),
        stderr: stderr.into_owned(),
    }))
}

#[tool_handler]
impl ServerHandler for GhTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("kid-agentic-coding-gh", env!("CARGO_PKG_VERSION"))
                .with_title("GitHub Issue Tools"),
        )
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .try_init()
        .map_err(|err| anyhow!("failed to initialize logging: {err}"))?;
    tracing::debug!("gh logging initialized");

    let _args = Args::parse();
    let server = GhTools::default();
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;
    Ok(())
}
