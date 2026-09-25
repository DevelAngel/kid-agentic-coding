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
use thiserror::Error;
use tokio::task;
use wire::{
    ACK_WAIT, CloseAction, CloseActionOutcome, CloseActionRequest, CloseActionVerdict,
    CommitFixDone, bridge_error, connect_to_bridge, send_line,
};

use std::env;
use std::io::{self, Read};
use std::mem;
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::result;

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server for investigating and closing a commit-fix session")]
struct Args {
    /// Name or path of the Unix socket used for workflow events.
    #[arg(long)]
    socket: String,
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
    /// Commit body as plain text; blank lines separate paragraphs, each word-wrapped independently.
    body: String,
    /// Optional breaking-change note.
    #[serde(default)]
    breaking_change_note: Option<String>,
    /// Amend the previous commit instead of creating a new one.
    #[serde(default)]
    amend: bool,
    /// Working directory for the Git command.
    #[serde(default)]
    cwd: Option<PathBuf>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AddParams {
    /// File or directory path relative to the workspace root.
    path: PathBuf,
    /// Working directory for the Git command.
    #[serde(default)]
    cwd: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct CwdParams {
    /// Working directory for the Git command.
    #[serde(default)]
    cwd: Option<PathBuf>,
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
    #[error(
        "commit body wraps to {lines} lines ({words} words across {paragraphs} paragraph(s)) \
         and is longer than 12 lines — shorten the paragraphs"
    )]
    TooManyBodyLines {
        lines: usize,
        words: usize,
        paragraphs: usize,
    },
    #[error(
        "breaking change note wraps to {lines} lines ({words} words) and is longer than \
         4 lines — shorten the note"
    )]
    TooManyBreakingChangeLines { lines: usize, words: usize },
}

fn build_commit_message(params: &CommitParams) -> result::Result<String, Vec<CommitMessageError>> {
    let mut errors = Vec::new();
    let paragraphs = split_paragraphs(&params.body);

    if !VALID_COMMIT_TYPES.contains(&params.commit_type.as_str()) {
        errors.push(CommitMessageError::InvalidType(params.commit_type.clone()));
    }
    if paragraphs.iter().all(|paragraph| paragraph.is_empty()) {
        errors.push(CommitMessageError::EmptyBody);
    }
    if paragraphs
        .iter()
        .any(|paragraph| paragraph.contains("BREAKING CHANGE"))
    {
        errors.push(CommitMessageError::BreakingChangeInBody);
    }

    let body_lines = wrap_body_lines(&paragraphs);
    if body_lines.len() > 12 {
        errors.push(CommitMessageError::TooManyBodyLines {
            lines: body_lines.len(),
            words: word_count(&paragraphs),
            paragraphs: paragraphs.len(),
        });
    }

    let breaking_change_lines = params
        .breaking_change_note
        .as_deref()
        .map(wrap_breaking_change_note_lines);
    if let Some(lines) = &breaking_change_lines
        && lines.len() > 4
    {
        errors.push(CommitMessageError::TooManyBreakingChangeLines {
            lines: lines.len(),
            words: params
                .breaking_change_note
                .as_deref()
                .map(|note| note.split_whitespace().count())
                .unwrap_or_default(),
        });
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

    if !errors.is_empty() {
        return Err(errors);
    }

    let mut message = format!("{summary}\n\n{}", body_lines.join("\n"));
    if let Some(lines) = &breaking_change_lines {
        message.push_str("\n\nBREAKING CHANGE: ");
        message.push_str(&lines.join("\n    "));
    }
    Ok(message)
}

/// Splits body text into paragraphs at blank lines, trimming and dropping empty ones.
fn split_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Total word count across all body paragraphs — reported alongside a
/// line-count violation so the caller, which cannot predict how many
/// lines its text will wrap to, has a metric it set directly.
fn word_count(paragraphs: &[String]) -> usize {
    paragraphs
        .iter()
        .map(|paragraph| paragraph.split_whitespace().count())
        .sum()
}

/// Word-wraps each paragraph to `72` columns independently and rejoins
/// them with a blank separator line, the same layout a hand-wrapped
/// Conventional Commit body would have.
fn wrap_body_lines(paragraphs: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    for (index, paragraph) in paragraphs.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        lines.extend(wrap_paragraph_lines(paragraph, 72, 72));
    }
    lines
}

fn wrap_breaking_change_note_lines(note: &str) -> Vec<String> {
    wrap_paragraph_lines(note, 54, 68)
}

/// Word-wraps `text` into lines no wider than `first_line_max` for the
/// first line and `rest_max` for every line after it — the two differ
/// for the breaking-change note, which starts mid-line after a prefix.
/// Embedded newlines are treated as ordinary whitespace, same as any
/// other run of spaces.
fn wrap_paragraph_lines(text: &str, first_line_max: usize, rest_max: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();

    for word in text.split_whitespace() {
        let max_width = if lines.is_empty() {
            first_line_max
        } else {
            rest_max
        };
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

    lines
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

    async fn git_status(
        &self,
        Parameters(params): Parameters<CwdParams>,
    ) -> Result<CallToolResult, McpError> {
        command_result(
            "git",
            &["status", "--short"],
            "git status",
            params.cwd.as_deref(),
        )
        .await
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

    async fn git_diff(
        &self,
        Parameters(params): Parameters<CwdParams>,
    ) -> Result<CallToolResult, McpError> {
        command_result("git", &["diff"], "git diff", params.cwd.as_deref()).await
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
        authorize_action(&self.socket_name, CloseAction::Add)?;

        let path = params.path.to_string_lossy();
        let result = command_result(
            "git",
            &["add", path.as_ref()],
            "git add",
            params.cwd.as_deref(),
        )
        .await;
        let outcome = CloseActionOutcome {
            action: CloseAction::Add,
            success: result.is_ok(),
            reason: result.as_ref().err().map(ToString::to_string),
        };
        notify_outcome(&self.socket_name, outcome)?;
        result
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
        authorize_action(&self.socket_name, CloseAction::Commit)?;
        let result = if params.amend {
            command_result(
                "git",
                &["commit", "--amend", "-m", &commit_message],
                "git commit (amend)",
                params.cwd.as_deref(),
            )
            .await
        } else {
            command_result(
                "git",
                &["commit", "-m", &commit_message],
                "git commit",
                params.cwd.as_deref(),
            )
            .await
        };

        match result {
            Ok(result) => {
                let line = CommitFixDone { commit_message }.to_line().map_err(|err| {
                    McpError::internal_error(
                        "failed to encode commit-fix-done event",
                        Some(json!({"reason": err.to_string()})),
                    )
                })?;
                notify_bridge(&self.socket_name, &line).map_err(|err| {
                    let message = bridge_error(&self.socket_name, &err);
                    tracing::error!("{message}");
                    McpError::internal_error(
                        "failed to notify commit-fix-done bridge",
                        Some(json!({"reason": message})),
                    )
                })?;
                tracing::debug!("commit-fix-done event sent");
                Ok(result)
            }
            Err(error) => {
                notify_outcome(
                    &self.socket_name,
                    CloseActionOutcome {
                        action: CloseAction::Commit,
                        success: false,
                        reason: Some(error.to_string()),
                    },
                )?;
                Err(error)
            }
        }
    }
}
fn authorize_action(socket: &str, action: CloseAction) -> Result<(), McpError> {
    let request = CloseActionRequest { action };
    let line = request.to_line().map_err(|err| {
        McpError::internal_error(
            "failed to encode close-action event",
            Some(json!({"reason": err.to_string()})),
        )
    })?;
    let mut stream = connect_to_bridge(socket).map_err(|err| {
        let message = bridge_error(socket, &err);
        McpError::internal_error(
            "failed to connect to close-action bridge",
            Some(json!({"reason": message})),
        )
    })?;
    send_line(&mut stream, &line).map_err(|err| {
        McpError::internal_error(
            "failed to send close-action request",
            Some(json!({"reason": err.to_string()})),
        )
    })?;
    stream.shutdown(Shutdown::Write).map_err(|err| {
        McpError::internal_error(
            "failed to half-close close-action bridge",
            Some(json!({"reason": err.to_string()})),
        )
    })?;
    stream.set_read_timeout(Some(ACK_WAIT)).map_err(|err| {
        McpError::internal_error(
            "failed to arm close-action verdict timeout",
            Some(json!({"reason": err.to_string()})),
        )
    })?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).map_err(|err| {
        McpError::internal_error(
            "failed to read close-action verdict",
            Some(json!({"reason": err.to_string()})),
        )
    })?;
    let response = std::str::from_utf8(&response).map_err(|err| {
        McpError::internal_error(
            "close-action verdict is not valid UTF-8",
            Some(json!({"reason": err.to_string()})),
        )
    })?;
    let line = response.lines().next().unwrap_or("");
    match CloseActionVerdict::from_line(line).map_err(|err| {
        McpError::internal_error(
            "malformed close-action verdict",
            Some(json!({"reason": err.to_string()})),
        )
    })? {
        CloseActionVerdict::Authorized => Ok(()),
        CloseActionVerdict::Rejected { reason } => Err(McpError::internal_error(
            "close-action rejected",
            Some(json!({"reason": reason})),
        )),
    }
}

fn notify_outcome(socket: &str, outcome: CloseActionOutcome) -> Result<(), McpError> {
    let line = outcome.to_line().map_err(|err| {
        McpError::internal_error(
            "failed to encode close-action outcome",
            Some(json!({"reason": err.to_string()})),
        )
    })?;
    notify_bridge(socket, &line).map_err(|err| {
        let message = bridge_error(socket, &err);
        tracing::error!("{message}");
        McpError::internal_error(
            "failed to notify close-action outcome",
            Some(json!({"reason": message})),
        )
    })
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

fn notify_bridge(socket: &str, line: &str) -> io::Result<()> {
    let mut stream = connect_to_bridge(socket)?;
    send_line(&mut stream, line)
}

async fn command_result(
    program: &str,
    args: &[&str],
    operation: &str,
    cwd: Option<&Path>,
) -> Result<CallToolResult, McpError> {
    let output = run_process(program, cwd, args, operation).await?;
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
    cwd: Option<&Path>,
    args: &[&str],
    operation: &str,
) -> Result<ProcessOutput, McpError> {
    let workspace_root = env::current_dir().map_err(|err| {
        McpError::internal_error(
            format!("failed to determine working directory for {operation}"),
            Some(json!({"reason": err.to_string()})),
        )
    })?;
    let working_directory = cwd.map(PathBuf::from).unwrap_or(workspace_root);

    let program = program.to_owned();
    let args = args.iter().map(ToString::to_string).collect::<Vec<_>>();
    let output = task::spawn_blocking(move || {
        Command::new(program)
            .args(args)
            .current_dir(working_directory)
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
    // Probe the bridge socket, but never abort on failure: exiting here
    // would leave the agent waiting for this server to become ready, and
    // server stderr is not reliably visible to the user anyway. Tool calls
    // report the same error when they need the bridge.
    if let Err(err) = connect_to_bridge(&args.socket) {
        tracing::error!("{}", bridge_error(&args.socket, &err));
    }
    let server = GitCommitFixCloseTools::new(args.socket);
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::{env, fs, process};

    fn test_socket(suffix: &str) -> (PathBuf, UnixListener) {
        let path = env::temp_dir().join(format!(
            "kid-agentic-coding-close-{suffix}-{}.sock",
            process::id()
        ));
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind succeeds");
        (path, listener)
    }

    #[test]
    fn authorize_action_waits_for_an_authorized_verdict() {
        let (path, listener) = test_socket("authorized");
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept succeeds");
            let mut payload = String::new();
            stream.read_to_string(&mut payload).expect("reads request");
            let request =
                serde_json::from_str::<CloseActionRequest>(payload.trim()).expect("request parses");
            assert_eq!(request.action, CloseAction::Add);
            let verdict = CloseActionVerdict::Authorized
                .to_line()
                .expect("verdict encodes");
            stream
                .write_all(format!("{verdict}\n").as_bytes())
                .expect("writes verdict");
        });

        authorize_action(&path.display().to_string(), CloseAction::Add)
            .expect("authorized action succeeds");
        responder.join().expect("responder finishes");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn authorize_action_rejects_without_running_the_git_action() {
        let (path, listener) = test_socket("rejected");
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept succeeds");
            let mut payload = String::new();
            stream.read_to_string(&mut payload).expect("reads request");
            let verdict = CloseActionVerdict::Rejected {
                reason: "another action is pending".to_owned(),
            }
            .to_line()
            .expect("verdict encodes");
            stream
                .write_all(format!("{verdict}\n").as_bytes())
                .expect("writes verdict");
        });

        let error = authorize_action(&path.display().to_string(), CloseAction::Commit)
            .expect_err("rejected action fails");
        assert!(error.to_string().contains("close-action rejected"));
        responder.join().expect("responder finishes");
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn command_result_includes_stderr_when_command_fails() {
        let error = command_result(
            "sh",
            &["-c", "printf 'commit failed' >&2; exit 1"],
            "git commit",
            None,
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("git commit failed: commit failed")
        );
    }

    #[tokio::test]
    async fn command_result_uses_requested_working_directory() {
        let directory =
            env::temp_dir().join(format!("kid-agentic-coding-git-cwd-{}", process::id()));
        fs::create_dir_all(&directory).expect("create temp directory");
        let output = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&directory)
            .output()
            .expect("git init runs");
        assert!(output.status.success());

        let result = command_result(
            "git",
            &["rev-parse", "--show-toplevel"],
            "git rev-parse",
            Some(&directory),
        )
        .await
        .expect("git runs in the requested directory");

        assert_eq!(
            result.content[0].as_text().unwrap().text.trim(),
            directory.display().to_string()
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn command_result_rejects_an_invalid_working_directory() {
        let directory =
            env::temp_dir().join(format!("kid-agentic-coding-git-missing-{}", process::id()));
        let error = command_result("git", &["status"], "git status", Some(&directory))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("failed to execute git status"));
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
            cwd: None,
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
            body: "BREAKING CHANGE".to_owned(),
            breaking_change_note: None,
            amend: false,
            cwd: None,
        };

        let errors = build_commit_message(&params).unwrap_err();

        assert_eq!(errors.len(), 3);
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
    }

    #[test]
    fn build_commit_message_rejects_a_body_with_too_many_lines() {
        let params = CommitParams {
            commit_type: "fix".to_owned(),
            scope: None,
            description: "Trim the body".to_owned(),
            body: vec!["line"; 13].join("\n\n"),
            breaking_change_note: None,
            amend: false,
            cwd: None,
        };

        let errors = build_commit_message(&params).unwrap_err();

        assert!(
            errors
                .iter()
                .any(|error| error.to_string().contains("shorten the paragraphs"))
        );
    }

    #[test]
    fn build_commit_message_rejects_a_breaking_change_note_with_too_many_lines() {
        let note = "word ".repeat(60).trim_end().to_owned();
        let params = CommitParams {
            commit_type: "feat".to_owned(),
            scope: None,
            description: "Break something".to_owned(),
            body: "Explain the break.".to_owned(),
            breaking_change_note: Some(note),
            amend: false,
            cwd: None,
        };

        let errors = build_commit_message(&params).unwrap_err();

        assert!(
            errors
                .iter()
                .any(|error| error.to_string().contains("longer than 4 lines"))
        );
    }

    #[test]
    fn too_many_body_lines_error_reports_word_and_paragraph_count() {
        let params = CommitParams {
            commit_type: "fix".to_owned(),
            scope: None,
            description: "Trim the body".to_owned(),
            body: vec!["one two three four"; 13].join("\n\n"),
            breaking_change_note: None,
            amend: false,
            cwd: None,
        };

        let errors = build_commit_message(&params).unwrap_err();

        assert!(errors.iter().any(|error| {
            let message = error.to_string();
            message.contains("52 words") && message.contains("13 paragraph")
        }));
    }

    #[test]
    fn too_many_breaking_change_lines_error_reports_word_count() {
        let note = "word ".repeat(60).trim_end().to_owned();
        let params = CommitParams {
            commit_type: "feat".to_owned(),
            scope: None,
            description: "Break something".to_owned(),
            body: "Explain the break.".to_owned(),
            breaking_change_note: Some(note),
            amend: false,
            cwd: None,
        };

        let errors = build_commit_message(&params).unwrap_err();

        assert!(
            errors
                .iter()
                .any(|error| error.to_string().contains("60 words"))
        );
    }

    #[test]
    fn build_commit_message_splits_body_paragraphs_on_blank_lines() {
        let params = CommitParams {
            commit_type: "fix".to_owned(),
            scope: None,
            description: "Explain in two paragraphs".to_owned(),
            body: "First paragraph.\n\nSecond paragraph.".to_owned(),
            breaking_change_note: None,
            amend: false,
            cwd: None,
        };

        let message = build_commit_message(&params).unwrap();

        assert_eq!(
            message,
            "fix: explain in two paragraphs\n\nFirst paragraph.\n\nSecond paragraph."
        );
    }

    #[test]
    fn build_commit_message_collapses_embedded_newlines_in_a_paragraph() {
        let params = CommitParams {
            commit_type: "fix".to_owned(),
            scope: None,
            description: "Rewrap a pre-broken paragraph".to_owned(),
            body: "Line one\nline two\nline three".to_owned(),
            breaking_change_note: None,
            amend: false,
            cwd: None,
        };

        let message = build_commit_message(&params).unwrap();

        assert_eq!(
            message,
            "fix: rewrap a pre-broken paragraph\n\nLine one line two line three"
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
            wrap_breaking_change_note_lines(note).join("\n    "),
            "This breaking change note is deliberately long enough\n    to wrap across multiple lines while keeping continuation lines\n    indented."
        );
    }

    #[test]
    fn lowercase_first_char_preserves_acronyms() {
        assert_eq!(lowercase_first_char("Add"), "add");
        assert_eq!(lowercase_first_char("XML parser"), "XML parser");
    }
}
