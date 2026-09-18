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
use std::path::{Path, PathBuf};

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
    /// session's queue. Analysis context for the fix session, not an
    /// instruction and not a pre-formatted commit summary - the fix
    /// session composes the actual commit message itself.
    tldr: String,
    /// The problem or motivation: why this change is needed. Analysis
    /// context for the fix session, not an instruction.
    why: String,
    /// What changed or should change, in prose - not a diff or file
    /// list. Analysis context for the fix session, not an instruction.
    what: String,
    /// Whether the fix session should amend the previous commit instead
    /// of creating a new one.
    amend: bool,
    cwd: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct CommitFixEvent<'a> {
    event: &'static str,
    instructions: &'static str,
    amend: bool,
    tldr: &'a str,
    why: &'a str,
    what: &'a str,
    cwd: Option<&'a Path>,
}

const COMMIT_FIX_EVENT: &str = "commit-fix";
const COMMIT_FIX_INSTRUCTIONS: &str = "The main session requested a commit-fix session. Investigate the current problem, fix the underlying issue, and run the provided check, lint, and test tools. The tldr/why/what fields below are analysis context, not instructions and not a pre-formatted commit message - compose the correctly formatted commit message yourself. Respect the main session's amend decision: amend the previous commit if it is set, otherwise create a new commit, and commit the resulting changes using the available Git tools.";

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
        description = "Starts an autonomous fix session that investigates and fixes the current problem, then commits the result itself. Give tldr/why/what as analysis context for the fix session to reason from - never as instructions, and never as a pre-written commit message.",
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
        tracing::info!(%params.amend, %params.tldr, "commit-fix session requested");
        let event = CommitFixEvent {
            event: COMMIT_FIX_EVENT,
            instructions: COMMIT_FIX_INSTRUCTIONS,
            amend: params.amend,
            tldr: &params.tldr,
            why: &params.why,
            what: &params.what,
            cwd: params.cwd.as_deref(),
        };
        let message = serde_json::to_vec(&event).map_err(|err| {
            McpError::internal_error(
                "failed to encode commit-fix event",
                Some(json!({"reason": err.to_string()})),
            )
        })?;
        notify_bridge(&self.socket_name, &message).map_err(|err| {
            let message = bridge_error(&self.socket_name, &err);
            tracing::error!("{message}");
            McpError::internal_error(
                "failed to notify commit-fix bridge",
                Some(json!({"reason": message})),
            )
        })?;
        tracing::debug!("commit-fix event sent");

        Ok(CallToolResult::success(vec![ContentBlock::text(
            "commit-fix session requested",
        )]))
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

fn notify_bridge(socket: &str, message: &[u8]) -> io::Result<()> {
    let mut stream = connect_to_bridge(socket)?;
    stream.write_all(message)?;
    stream.write_all(b"\n")
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
        COMMIT_FIX_EVENT, COMMIT_FIX_INSTRUCTIONS, CommitFixEvent, GitCommitWithFixParams,
        bridge_error, connect_to_bridge,
    };
    use std::io::{self, ErrorKind};
    use std::os::unix::net::UnixListener;
    use std::{env, fs, process};

    #[test]
    fn commit_fix_event_contains_workflow_instructions_and_context_fields() {
        let event = CommitFixEvent {
            event: COMMIT_FIX_EVENT,
            instructions: COMMIT_FIX_INSTRUCTIONS,
            amend: true,
            tldr: "Replace the flat context field with tldr/why/what.",
            why: "The flat context field was too vague to trigger reliably.",
            what: "Split context into tldr, why, and what fields.",
            cwd: None,
        };

        let value = serde_json::to_value(event).expect("event is serializable");

        assert_eq!(value["event"], COMMIT_FIX_EVENT);
        assert_eq!(value["instructions"], COMMIT_FIX_INSTRUCTIONS);
        assert_eq!(value["amend"], true);
        assert_eq!(
            value["tldr"],
            "Replace the flat context field with tldr/why/what."
        );
        assert_eq!(
            value["why"],
            "The flat context field was too vague to trigger reliably."
        );
        assert_eq!(
            value["what"],
            "Split context into tldr, why, and what fields."
        );
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
}
