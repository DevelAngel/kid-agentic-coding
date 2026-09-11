use anyhow::Result;
use anyhow::anyhow;
use clap::Parser;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::schemars::JsonSchema;
use rmcp::serde::Serialize;
use rmcp::{
    ErrorData as McpError, Json, ServerHandler, service, tool, tool_handler, tool_router, transport,
};
use tokio::task;

use std::env;
use std::io::{self, ErrorKind};
use std::process::{Command, Stdio};

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server for Python tools")]
struct Args {}

#[derive(Serialize, JsonSchema)]
struct PythonToolResult {
    status: i32,
    stdout: String,
    stderr: String,
}

#[derive(Serialize)]
struct PythonToolError {
    error: String,
    reason: String,
}

impl PythonToolError {
    fn into_json_value(self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }
}

#[derive(Clone)]
struct PythonTools {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl Default for PythonTools {
    fn default() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl PythonTools {
    #[tool(
        description = "Check the whole Python project, including tests",
        annotations(
            title = "Python Check",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn python_check(&self) -> Result<Json<PythonToolResult>, McpError> {
        run_uv(&["run", "ruff", "check", "--fix"], "ruff check", Some("uv")).await
    }

    #[tool(
        description = "Run the ty linter and report style/correctness issues",
        annotations(
            title = "Python Lint (ty)",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn python_lint_ty(&self) -> Result<Json<PythonToolResult>, McpError> {
        run_uv(&["run", "ty", "check"], "ty check", Some("uv")).await
    }

    #[tool(
        description = "Run the mypy linter and report style/correctness issues",
        annotations(
            title = "Python Lint (mypy)",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn python_lint(&self) -> Result<Json<PythonToolResult>, McpError> {
        run_uv(&["run", "mypy"], "mypy", Some("uv")).await
    }

    #[tool(
        description = "Run the full unit test suite",
        annotations(
            title = "Python Test",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn python_test(&self) -> Result<Json<PythonToolResult>, McpError> {
        run_uv(
            &[
                "run",
                "pytest",
                "--cov=src",
                "--cov-report=term-missing",
                "--no-cov-on-fail",
                "--override-ini=addopts=",
                "tests/unit",
            ],
            "pytest",
            Some("uv"),
        )
        .await
    }

    #[tool(
        description = "Build the project",
        annotations(
            title = "Python Package",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn python_build_packages(&self) -> Result<Json<PythonToolResult>, McpError> {
        run_uv(
            &["build", "--all-packages", "--out-dir", "dist"],
            "uv build",
            Some("uv"),
        )
        .await
    }
}

async fn run_uv(
    args: &[&str],
    operation: &str,
    missing_dependency: Option<&str>,
) -> Result<Json<PythonToolResult>, McpError> {
    let project_root = env::current_dir().map_err(|err| {
        McpError::internal_error(
            "failed to determine working directory",
            PythonToolError {
                error: "failed to determine working directory".to_string(),
                reason: err.to_string(),
            }
            .into_json_value(),
        )
    })?;

    let args = args.iter().map(ToString::to_string).collect::<Vec<_>>();
    let output = task::spawn_blocking(move || {
        Command::new("uv")
            .args(&args)
            .current_dir(project_root)
            .stdin(Stdio::null())
            .output()
    })
    .await
    .map_err(|err| {
        tracing::error!(?err, operation, "uv task failed");
        McpError::internal_error(
            format!("failed to run {operation}"),
            PythonToolError {
                error: format!("failed to run {operation}"),
                reason: err.to_string(),
            }
            .into_json_value(),
        )
    })?;

    let output = output.map_err(|err| {
        tracing::error!(?err, operation, "uv failed to execute");
        let reason = if err.kind() == ErrorKind::NotFound {
            missing_dependency.map_or_else(
                || err.to_string(),
                |dependency| format!("{dependency} is not installed"),
            )
        } else {
            err.to_string()
        };
        McpError::internal_error(
            format!("failed to execute {operation}"),
            PythonToolError {
                error: format!("failed to execute {operation}"),
                reason,
            }
            .into_json_value(),
        )
    })?;

    let status = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(Json(PythonToolResult {
        status,
        stdout: stdout.into_owned(),
        stderr: stderr.into_owned(),
    }))
}

#[tool_handler]
impl ServerHandler for PythonTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("kid-agentic-coding-python", env!("CARGO_PKG_VERSION"))
                .with_title("Python Tools"),
        )
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .try_init()
        .map_err(|err| anyhow!("failed to initialize logging: {err}"))?;
    tracing::debug!("python logging initialized");

    let _args = Args::parse();
    let server = PythonTools::default();
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;
    Ok(())
}
