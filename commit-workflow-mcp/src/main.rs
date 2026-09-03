use anyhow::Result;
use anyhow::anyhow;
use clap::Parser;
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
use std::process::{Command, Stdio};

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server for the git_commit_with_check tool")]
struct Args {}

#[derive(Debug, Deserialize, JsonSchema)]
struct GitCommitWithCheckParams {
    /// The commit message, used verbatim.
    message: String,
}

#[derive(Serialize, JsonSchema)]
struct GitCommitWithCheckResult {
    committed: bool,
    message: String,
}

#[derive(Serialize)]
struct CheckStepFailure {
    failed_step: String,
    status: i32,
    stdout: String,
    stderr: String,
}

impl CheckStepFailure {
    fn into_json_value(self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }
}

#[derive(Serialize)]
struct CommitWorkflowError {
    error: String,
    reason: String,
}

impl CommitWorkflowError {
    fn into_json_value(self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }
}

#[derive(Clone)]
struct CommitWorkflowTools {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl Default for CommitWorkflowTools {
    fn default() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl CommitWorkflowTools {
    #[tool(
        description = "Runs check, lint, and test; commits with the given message on green",
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
    ) -> Result<Json<GitCommitWithCheckResult>, McpError> {
        for (step, args, operation, missing_dependency) in [
            (
                "check",
                &["check", "--quiet", "--all-targets"][..],
                "cargo check",
                None,
            ),
            ("lint", &["clippy"][..], "cargo clippy", None),
            (
                "test",
                &["nextest", "run", "--cargo-quiet"][..],
                "cargo nextest",
                Some("cargo-nextest"),
            ),
        ] {
            let output = run_cargo(args, operation, missing_dependency).await?;
            if output.status != 0 {
                return Err(McpError::internal_error(
                    format!("{operation} failed"),
                    CheckStepFailure {
                        failed_step: step.to_string(),
                        status: output.status,
                        stdout: output.stdout,
                        stderr: output.stderr,
                    }
                    .into_json_value(),
                ));
            }
        }

        run_git_commit(&params.message).await?;

        Ok(Json(GitCommitWithCheckResult {
            committed: true,
            message: params.message,
        }))
    }
}

#[cfg_attr(test, derive(Debug))]
struct ProcessOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

/// Runs `program` with `args` in the workspace root, capturing its output.
/// Shared by the cargo check/lint/test steps and the git commit step so both
/// go through the same spawn and error-mapping logic.
async fn run_process(
    program: &str,
    args: &[String],
    operation: &str,
    missing_dependency: Option<&str>,
) -> Result<ProcessOutput, McpError> {
    let workspace_root = env::current_dir().map_err(|err| {
        McpError::internal_error(
            "failed to determine working directory",
            CommitWorkflowError {
                error: "failed to determine working directory".to_string(),
                reason: err.to_string(),
            }
            .into_json_value(),
        )
    })?;

    let program = program.to_string();
    let args = args.to_vec();
    let output = task::spawn_blocking(move || {
        Command::new(program)
            .args(&args)
            .current_dir(workspace_root)
            .stdin(Stdio::null())
            .output()
    })
    .await
    .map_err(|err| {
        tracing::error!(?err, operation, "process task failed");
        McpError::internal_error(
            format!("failed to run {operation}"),
            CommitWorkflowError {
                error: format!("failed to run {operation}"),
                reason: err.to_string(),
            }
            .into_json_value(),
        )
    })?;

    let output = output.map_err(|err| {
        tracing::error!(?err, operation, "process failed to execute");
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
            CommitWorkflowError {
                error: format!("failed to execute {operation}"),
                reason,
            }
            .into_json_value(),
        )
    })?;

    let status = output.status.code().map_or(-1, |status| status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(ProcessOutput {
        status,
        stdout: stdout.into_owned(),
        stderr: stderr.into_owned(),
    })
}

async fn run_cargo(
    args: &[&str],
    operation: &str,
    missing_dependency: Option<&str>,
) -> Result<ProcessOutput, McpError> {
    let args = args.iter().map(ToString::to_string).collect::<Vec<_>>();
    run_process("cargo", &args, operation, missing_dependency).await
}

async fn run_git_commit(message: &str) -> Result<(), McpError> {
    let args = vec!["commit".to_string(), "-m".to_string(), message.to_string()];
    let output = run_process("git", &args, "git commit", None).await?;

    if output.status == 0 {
        Ok(())
    } else {
        Err(McpError::internal_error(
            "git commit failed",
            CommitWorkflowError {
                error: "git commit failed".to_string(),
                reason: output.stderr,
            }
            .into_json_value(),
        ))
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .try_init()
        .map_err(|err| anyhow!("failed to initialize logging: {err}"))?;
    tracing::debug!("commit-workflow logging initialized");

    let _args = Args::parse();
    let server = CommitWorkflowTools::default();
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;
    Ok(())
}

#[cfg(test)]
mod run_process_tests {
    use super::run_process;

    #[tokio::test]
    async fn success_exit_code_is_reported() {
        let output = run_process("true", &[], "true", None)
            .await
            .expect("true is always available");

        assert_eq!(output.status, 0);
    }

    #[tokio::test]
    async fn failure_exit_code_is_reported() {
        let output = run_process("false", &[], "false", None)
            .await
            .expect("false is always available");

        assert_ne!(output.status, 0);
    }

    #[tokio::test]
    async fn missing_program_reports_missing_dependency_reason() {
        let err = run_process(
            "kid-agentic-coding-nonexistent-program",
            &[],
            "phantom step",
            Some("phantom-tool"),
        )
        .await
        .expect_err("program does not exist");

        let message = err.message.to_string();
        assert!(message.contains("phantom step"));
    }
}
