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
use std::io;
use std::process::{Command, Stdio};

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server for Rust tools")]
struct Args {}

#[derive(Serialize, JsonSchema)]
struct RustCheckResult {
    status: i32,
    stdout: String,
    stderr: String,
}

#[derive(Serialize)]
struct RustCheckError {
    error: String,
    reason: String,
}

impl RustCheckError {
    fn into_json_value(self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }
}

#[derive(Clone)]
struct RustTools {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl Default for RustTools {
    fn default() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl RustTools {
    #[tool(
        description = "Check the whole Rust workspace, including test targets",
        annotations(
            title = "Rust Check",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn rust_check(&self) -> Result<Json<RustCheckResult>, McpError> {
        let workspace_root = env::current_dir().map_err(|err| {
            McpError::internal_error(
                "failed to determine working directory",
                RustCheckError {
                    error: "failed to determine working directory".to_string(),
                    reason: err.to_string(),
                }
                .into_json_value(),
            )
        })?;

        let output = task::spawn_blocking(move || {
            Command::new("cargo")
                .args(["check", "--quiet", "--all-targets"])
                .current_dir(workspace_root)
                .stdin(Stdio::null())
                .output()
        })
        .await
        .map_err(|err| {
            tracing::error!(?err, "rust-check task failed");
            McpError::internal_error(
                "failed to run cargo check",
                RustCheckError {
                    error: "failed to run cargo check".to_string(),
                    reason: err.to_string(),
                }
                .into_json_value(),
            )
        })?;

        let output = output.map_err(|err| {
            tracing::error!(?err, "rust-check failed to execute cargo");
            McpError::internal_error(
                "failed to execute cargo check",
                RustCheckError {
                    error: "failed to execute cargo check".to_string(),
                    reason: err.to_string(),
                }
                .into_json_value(),
            )
        })?;

        let status = output.status.code().map_or(-1, |status| status);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(Json(RustCheckResult {
            status,
            stdout: stdout.into_owned(),
            stderr: stderr.into_owned(),
        }))
    }
}

#[tool_handler]
impl ServerHandler for RustTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("kid-agentic-coding-rust", env!("CARGO_PKG_VERSION"))
                .with_title("Rust Tools"),
        )
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .try_init()
        .map_err(|err| anyhow!("failed to initialize logging: {err}"))?;
    tracing::debug!("rust logging initialized");

    let _args = Args::parse();
    let server = RustTools::default();
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;
    Ok(())
}
