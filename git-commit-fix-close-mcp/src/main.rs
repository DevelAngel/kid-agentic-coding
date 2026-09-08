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
use thiserror::Error;
use tokio::task;

use std::env;
use std::io::{self, Write};
use std::mem;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::process::{Command, Stdio};
use std::result;

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
    /// Conventional Commit type.
    #[serde(rename = "type")]
    commit_type: String,
    /// Optional scope for the commit.
    #[serde(default)]
    scope: Option<String>,
    /// Commit description.
    description: String,
    /// Non-empty commit body.
    body: String,
    /// Optional breaking-change note.
    #[serde(default)]
    breaking_change_note: Option<String>,
    /// Amend the previous commit instead of creating a new one.
    #[serde(default)]
    amend: bool,
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

const VALID_COMMIT_TYPES: [&str; 9] = [
    "build", "chore", "ci", "docs", "feat", "fix", "refactor", "style", "test",
];

#[derive(Debug, Error)]
enum CommitMessageError {
    #[error("invalid commit type: {0}")]
    InvalidType(String),
    #[error("commit body must not be empty")]
    EmptyBody,
    #[error("commit body must not contain BREAKING CHANGE")]
    BreakingChangeInBody,
    #[error("commit summary is {0} characters long and longer than 50 characters")]
    LongSummary(usize),
    #[error("commit body line {line} is {length} characters long and longer than 72 characters")]
    LongBodyLine { line: usize, length: usize },
}

fn build_commit_message(params: &CommitParams) -> result::Result<String, Vec<CommitMessageError>> {
    let mut errors = Vec::new();

    if !VALID_COMMIT_TYPES.contains(&params.commit_type.as_str()) {
        errors.push(CommitMessageError::InvalidType(params.commit_type.clone()));
    }
    if params.body.trim().is_empty() {
        errors.push(CommitMessageError::EmptyBody);
    }
    if params.body.contains("BREAKING CHANGE") {
        errors.push(CommitMessageError::BreakingChangeInBody);
    }

    let description = lowercase_first_char(&params.description);
    let scope = params
        .scope
        .as_deref()
        .map(|scope| format!("({scope})"))
        .unwrap_or_default();
    let breaking = if params.breaking_change_note.is_some() {
        "!"
    } else {
        ""
    };
    let summary = format!("{}{}{}: {description}", params.commit_type, scope, breaking);

    if summary.chars().count() > 50 {
        errors.push(CommitMessageError::LongSummary(summary.chars().count()));
    }
    for (index, line) in params.body.lines().enumerate() {
        if line.chars().count() > 72 {
            errors.push(CommitMessageError::LongBodyLine {
                line: index + 1,
                length: line.chars().count(),
            });
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let mut message = format!("{summary}\n\n{}", params.body);
    if let Some(note) = &params.breaking_change_note {
        message.push_str("\n\nBREAKING CHANGE: ");
        message.push_str(&wrap_breaking_change_note(note));
    }
    Ok(message)
}

fn wrap_breaking_change_note(note: &str) -> String {
    let mut lines = Vec::new();
    let mut line = String::new();

    for word in note.split_whitespace() {
        let max_width = if lines.is_empty() { 54 } else { 68 };
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= max_width {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(mem::take(&mut line));
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }

    lines.join("\n    ")
}

fn lowercase_first_char(value: &str) -> String {
    let Some(first) = value.chars().next() else {
        return String::new();
    };
    let Some(word) = value.split_whitespace().next() else {
        return value.to_owned();
    };

    if word.chars().skip(1).any(char::is_uppercase) {
        return value.to_owned();
    }

    let lower = first.to_lowercase().to_string();
    if lower == first.to_string() {
        return value.to_owned();
    }

    format!("{lower}{remaining}", remaining = &value[lower.len()..])
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
        description = "Creates a Git commit with the given message, optionally amends the previous commit, and closes the commit-fix session",
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
        let commit_message = match build_commit_message(&params) {
            Ok(message) => message,
            Err(errors) => {
                let text = errors
                    .into_iter()
                    .map(|error| format!("- {error}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok(CallToolResult::error(vec![ContentBlock::text(text)]));
            }
        };
        let result = if params.amend {
            command_result(
                "git",
                &["commit", "--amend", "-m", &commit_message],
                "git commit (amend)",
            )
            .await?
        } else {
            command_result("git", &["commit", "-m", &commit_message], "git commit").await?
        };

        let event = CommitFixDoneEvent {
            event: COMMIT_FIX_DONE_EVENT,
            commit_message: &commit_message,
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

    #[test]
    fn commit_params_default_to_create_a_new_commit() {
        let params: CommitParams = serde_json::from_value(json!({
            "type": "fix",
            "description": "Review feedback",
            "body": "Address the review feedback.",
        }))
        .unwrap();

        assert!(!params.amend);
        assert_eq!(params.commit_type, "fix");
    }

    #[test]
    fn build_commit_message_normalizes_description_and_breaking_change() {
        let params = CommitParams {
            commit_type: "feat".to_owned(),
            scope: Some("session".to_owned()),
            description: "Improve commit handling".to_owned(),
            body: "Handle commit messages centrally.".to_owned(),
            breaking_change_note: Some("The commit input is now structured.".to_owned()),
            amend: false,
        };

        let message = build_commit_message(&params).unwrap();

        assert_eq!(
            message,
            "feat(session)!: improve commit handling\n\nHandle commit messages centrally.\n\nBREAKING CHANGE: The commit input is now structured."
        );
    }

    #[test]
    fn build_commit_message_reports_multiple_violations() {
        let params = CommitParams {
            commit_type: "unknown".to_owned(),
            scope: None,
            description: "A very long description that makes the summary too long".to_owned(),
            body: format!("BREAKING CHANGE\n{}", "x".repeat(73)),
            breaking_change_note: None,
            amend: false,
        };

        let errors = build_commit_message(&params).unwrap_err();

        assert_eq!(errors.len(), 4);
        assert!(
            errors
                .iter()
                .any(|error| error.to_string().contains("invalid commit type"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.to_string().contains("summary"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.to_string().contains("BREAKING CHANGE"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.to_string().contains("body line 2"))
        );
    }

    #[test]
    fn lowercase_first_char_handles_empty_and_unicode_text() {
        assert_eq!(lowercase_first_char(""), "");
        assert_eq!(lowercase_first_char("Review"), "review");
        assert_eq!(lowercase_first_char("Änderung"), "änderung");
    }

    #[test]
    fn breaking_change_note_wraps_and_indents_continuation_lines() {
        let note = "This breaking change note is deliberately long enough to wrap across multiple lines while keeping continuation lines indented.";

        assert_eq!(
            wrap_breaking_change_note(note),
            "This breaking change note is deliberately long enough\n    to wrap across multiple lines while keeping continuation lines\n    indented."
        );
    }

    #[test]
    fn lowercase_first_char_preserves_acronyms() {
        assert_eq!(lowercase_first_char("Add"), "add");
        assert_eq!(lowercase_first_char("XML parser"), "XML parser");
    }
}
