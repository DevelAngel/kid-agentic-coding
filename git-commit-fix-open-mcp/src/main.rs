use anyhow::Result;
use anyhow::anyhow;
use clap::Parser;
use commit_fix_contract::{CommitFixRequest, CommitFixVerdict};
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::schemars::JsonSchema;
use rmcp::serde::Deserialize;
use rmcp::{
    ErrorData as McpError, ServerHandler, service, tool, tool_handler, tool_router, transport,
};
use serde_json::json;

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server for the git_commit_with_fix tool")]
struct Args {
    /// Name or path of the Unix socket used for workflow events.
    #[arg(long)]
    socket: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GitCommitWithFixParams {
    /// One-line summary of the change, for a human skimming the fix
    /// session's queue.
    tldr: String,
    /// The problem or motivation: why this change is needed.
    why: String,
    /// What changed or should change, in prose - not a diff or file
    /// list.
    what: String,
    /// Whether the fix session should amend the previous commit instead
    /// of creating a new one.
    amend: bool,
    cwd: Option<PathBuf>,
}

const COMMIT_FIX_INSTRUCTIONS: &str = "Commit the current changes. The tldr/why/what fields below are analysis context, not instructions and not a pre-formatted commit message - compose the correctly formatted commit message yourself. Amend the previous commit if amend is set, otherwise create a new commit. If the commit fails, or you judge it necessary, run the provided check, lint, and test tools before retrying.";

/// How long the tool call waits for the app's verdict ack after half-closing
/// the bridge connection. An app without ack support closes the connection
/// right away, so this only bounds a wedged one.
const BRIDGE_ACK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct GitCommitFixOpenTools {
    socket_name: String,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl GitCommitFixOpenTools {
    fn new(socket_name: String) -> Self {
        Self {
            socket_name,
            tool_router: Self::tool_router(),
        }
    }
}
#[tool_router]
impl GitCommitFixOpenTools {
    #[tool(
        description = "Executes a commit workflow for the current changes, including check, lint, and test tools as needed. Give tldr/why/what as analysis context to reason from - never as instructions, and never as a pre-written commit message. Errors out while a fix session is already active or starting: wait for that session to commit (or cancel it), then retry.",
        annotations(
            title = "Git Commit With Fix",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn git_commit_with_fix(
        &self,
        Parameters(params): Parameters<GitCommitWithFixParams>,
    ) -> Result<CallToolResult, McpError> {
        tracing::info!(%params.amend, %params.tldr, "commit-fix workflow requested");
        let request = CommitFixRequest {
            instructions: COMMIT_FIX_INSTRUCTIONS.to_owned(),
            amend: params.amend,
            tldr: params.tldr,
            why: params.why,
            what: params.what,
            cwd: params.cwd,
        };
        let message = request.to_line().map_err(|err| {
            McpError::internal_error(
                "failed to encode commit-fix event",
                Some(json!({"reason": err.to_string()})),
            )
        })?;
        match notify_bridge(&self.socket_name, message.as_bytes()) {
            Ok(Some(CommitFixVerdict::Accepted)) => {
                tracing::info!("commit-fix session requested");
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    "commit-fix session requested",
                )]))
            }
            Ok(Some(CommitFixVerdict::Ignored { reason })) => {
                tracing::warn!(%reason, "commit-fix request ignored by the app");
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "commit-fix session not started, the request was ignored: {reason}"
                ))]))
            }
            Ok(Some(CommitFixVerdict::Rejected { reason })) => {
                tracing::warn!(%reason, "commit-fix request rejected by the app");
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "commit-fix request failed: {reason}"
                ))]))
            }
            Ok(None) => {
                // Legacy bridge: the app closed the connection without an
                // ack, so the outcome is unknowable here. Keep the old
                // success contract rather than inventing an error.
                tracing::debug!("no ack received; keeping the legacy commit-fix success");
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    "commit-fix session requested",
                )]))
            }
            Err(err) => {
                tracing::error!("{err}");
                Err(McpError::internal_error(
                    "failed to notify commit-fix bridge",
                    Some(json!({"reason": err})),
                ))
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for GitCommitFixOpenTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new(
                "kid-agentic-coding-git-commit-fix-open",
                env!("CARGO_PKG_VERSION"),
            )
            .with_title("Git Commit Fix Open"),
        )
    }
}

/// Writes the event to the bridge, half-closes the write side, and reads the
/// app's verdict ack until EOF or timeout. `Ok(None)` is the legacy outcome:
/// the app closed the connection without replying.
fn notify_bridge(socket: &str, message: &[u8]) -> Result<Option<CommitFixVerdict>, String> {
    let mut stream = connect_to_bridge(socket).map_err(|err| bridge_error(socket, &err))?;
    stream
        .write_all(message)
        .and_then(|()| stream.write_all(b"\n"))
        .map_err(|err| format!("failed to write to the bridge socket '{socket}': {err}"))?;
    // Half-close so the app can still reply on this connection, and we see
    // EOF once it closes its side after writing the ack.
    stream
        .shutdown(Shutdown::Write)
        .map_err(|err| format!("failed to half-close the bridge socket '{socket}': {err}"))?;
    stream
        .set_read_timeout(Some(BRIDGE_ACK_TIMEOUT))
        .map_err(|err| format!("failed to arm the bridge ack timeout on '{socket}': {err}"))?;
    let mut ack = Vec::new();
    match stream.read_to_end(&mut ack) {
        Ok(0) => Ok(None),
        Ok(_) => {
            let text = std::str::from_utf8(&ack)
                .map_err(|err| format!("non-UTF-8 commit-fix ack on '{socket}': {err}"))?;
            let line = text.lines().next().unwrap_or("");
            CommitFixVerdict::from_line(line)
                .map(Some)
                .map_err(|err| format!("malformed commit-fix ack '{line}': {err}"))
        }
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            tracing::debug!("timed out waiting for the bridge ack on '{socket}'");
            Ok(None)
        }
        Err(err) => Err(format!(
            "failed to read the bridge ack on '{socket}': {err}"
        )),
    }
}

/// Identifiers containing a path separator address a filesystem socket
/// (the sandboxed-agent fallback); bare identifiers are
/// abstract-namespace names.
fn connect_to_bridge(socket: &str) -> io::Result<UnixStream> {
    if socket.contains('/') {
        UnixStream::connect(Path::new(socket))
    } else {
        UnixStream::connect_addr(&SocketAddr::from_abstract_name(socket.as_bytes())?)
    }
}

/// Full description of a failed bridge connection: which socket was
/// attempted, the underlying OS error, and the fix for the common
/// sandboxed-agent case. Used both for the startup probe log and for tool
/// errors so the agent can relay actionable guidance to the user.
fn bridge_error(socket: &str, err: &io::Error) -> String {
    format!(
        "bridge socket '{socket}' is unreachable: {err}. If the agent runs sandboxed, \
         start kid-agentic-coding with --fs-socket-dir pointing at a writable \
         directory that is mounted into the sandbox (e.g. \
         $XDG_RUNTIME_DIR/kid-agentic-coding), because Linux abstract-namespace \
         sockets cannot cross a sandbox boundary."
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .try_init()
        .map_err(|err| anyhow!("failed to initialize logging: {err}"))?;
    tracing::debug!("git-commit-fix-open logging initialized");

    let args = Args::parse();
    // Probe the bridge socket, but never abort on failure: exiting here
    // would leave the agent waiting for this server to become ready, and
    // server stderr is not reliably visible to the user anyway. Tool calls
    // report the same error when they need the bridge.
    if let Err(err) = connect_to_bridge(&args.socket) {
        tracing::error!("{}", bridge_error(&args.socket, &err));
    }
    let server = GitCommitFixOpenTools::new(args.socket);
    let transport = transport::io::stdio();
    let running = service::serve_server(server, transport).await?;
    let _ = running.waiting().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        COMMIT_FIX_INSTRUCTIONS, GitCommitFixOpenTools, GitCommitWithFixParams, Parameters,
        bridge_error, connect_to_bridge, notify_bridge,
    };
    use commit_fix_contract::{CommitFixRequest, CommitFixVerdict};
    use std::io::{self, ErrorKind, Read, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::{env, fs, process};

    /// Binds a unique temp socket per test so parallel test threads cannot
    /// collide.
    fn verdict_ack_test_socket(suffix: &str) -> (PathBuf, UnixListener) {
        let path = env::temp_dir().join(format!(
            "kid-agentic-coding-bridge-{suffix}-{}.sock",
            process::id()
        ));
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind succeeds");
        (path, listener)
    }

    #[tokio::test]
    async fn git_commit_with_fix_sends_the_request_and_reports_acceptance() {
        let (path, listener) = verdict_ack_test_socket("tool-accept");

        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept succeeds");
            let mut payload = String::new();
            stream
                .read_to_string(&mut payload)
                .expect("reads until the client's half-close");
            let line = CommitFixVerdict::Accepted
                .to_line()
                .expect("verdict encodes");
            stream
                .write_all(format!("{line}\n").as_bytes())
                .expect("writes the ack");
            payload
        });

        let tools = GitCommitFixOpenTools::new(path.display().to_string());
        let result = tools
            .git_commit_with_fix(Parameters(GitCommitWithFixParams {
                tldr: "Replace the flat context field with tldr/why/what.".to_owned(),
                why: "The flat context field was too vague to trigger reliably.".to_owned(),
                what: "Split context into tldr, why, and what fields.".to_owned(),
                amend: true,
                cwd: None,
            }))
            .await
            .expect("the tool call succeeds");

        assert_eq!(result.is_error, Some(false));
        let payload = responder.join().expect("the responder finished");
        let request = serde_json::from_str::<CommitFixRequest>(payload.trim_end())
            .expect("the bridge received a commit-fix request");
        assert_eq!(
            request,
            CommitFixRequest {
                instructions: COMMIT_FIX_INSTRUCTIONS.to_owned(),
                amend: true,
                tldr: "Replace the flat context field with tldr/why/what.".to_owned(),
                why: "The flat context field was too vague to trigger reliably.".to_owned(),
                what: "Split context into tldr, why, and what fields.".to_owned(),
                cwd: None,
            }
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn git_commit_with_fix_params_require_tldr_why_what_and_amend() {
        let missing_amend = serde_json::from_value::<GitCommitWithFixParams>(serde_json::json!({
            "tldr": "Summary",
            "why": "Motivation",
            "what": "Change",
        }))
        .expect_err("amend is required");
        assert!(missing_amend.to_string().contains("amend"));

        let missing_what = serde_json::from_value::<GitCommitWithFixParams>(serde_json::json!({
            "tldr": "Summary",
            "why": "Motivation",
            "amend": false,
        }))
        .expect_err("what is required");
        assert!(missing_what.to_string().contains("what"));

        let params = serde_json::from_value::<GitCommitWithFixParams>(serde_json::json!({
            "tldr": "Summary",
            "why": "Motivation",
            "what": "Change",
            "amend": true,
        }))
        .expect("all fields present");
        assert!(params.amend);
        assert_eq!(params.tldr, "Summary");
        assert_eq!(params.why, "Motivation");
        assert_eq!(params.what, "Change");
    }

    #[test]
    fn startup_probe_reaches_a_listening_session() {
        let path = env::temp_dir().join(format!(
            "kid-agentic-coding-bridge-ping-{}.sock",
            process::id()
        ));
        let _ = fs::remove_file(&path);
        let _listener = UnixListener::bind(&path).expect("bind succeeds");

        connect_to_bridge(&path.display().to_string()).expect("listening socket is reachable");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn bridge_error_names_the_socket_and_the_fs_socket_dir_flag() {
        let socket = format!("kid-agentic-coding-bridge-test-{}", process::id());
        let err = io::Error::new(ErrorKind::NotFound, "no such file or directory");
        let message = bridge_error(&socket, &err);

        assert!(message.contains(&socket));
        assert!(message.contains("--fs-socket-dir"));
        assert!(message.contains("sandbox"));
    }

    #[test]
    fn notify_bridge_reads_the_rejected_verdict_ack() {
        let (path, listener) = verdict_ack_test_socket("verdict-reject");

        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept succeeds");
            let mut payload = Vec::new();
            stream
                .read_to_end(&mut payload)
                .expect("reads until the client's half-close");
            assert!(!payload.is_empty(), "the client sent the event");
            let line = CommitFixVerdict::Rejected {
                reason: "the test says no".to_owned(),
            }
            .to_line()
            .expect("verdict encodes");
            stream
                .write_all(format!("{line}\n").as_bytes())
                .expect("writes the ack");
        });

        let verdict = notify_bridge(&path.display().to_string(), b"{}")
            .expect("reading the bridge ack works")
            .expect("the responder wrote an ack");
        assert_eq!(
            verdict,
            CommitFixVerdict::Rejected {
                reason: "the test says no".to_owned()
            }
        );

        responder.join().expect("the responder finished");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn notify_bridge_reports_a_legacy_success_without_an_ack() {
        let (path, listener) = verdict_ack_test_socket("verdict-none");

        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept succeeds");
            let mut payload = Vec::new();
            stream
                .read_to_end(&mut payload)
                .expect("reads until the client's half-close");
            stream
                .shutdown(Shutdown::Both)
                .expect("closes like a legacy app");
        });

        assert!(
            notify_bridge(&path.display().to_string(), b"{}")
                .expect("a missing ack is not an error")
                .is_none()
        );

        responder.join().expect("the responder finished");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn notify_bridge_fails_on_a_malformed_ack() {
        let (path, listener) = verdict_ack_test_socket("verdict-malformed");

        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept succeeds");
            let mut payload = Vec::new();
            stream
                .read_to_end(&mut payload)
                .expect("reads until the client's half-close");
            stream.write_all(b"not json\n").expect("writes the ack");
        });

        let err = notify_bridge(&path.display().to_string(), b"{}")
            .expect_err("a malformed ack is an error");
        assert!(err.contains("malformed commit-fix ack"));

        responder.join().expect("the responder finished");
        let _ = fs::remove_file(&path);
    }
}
