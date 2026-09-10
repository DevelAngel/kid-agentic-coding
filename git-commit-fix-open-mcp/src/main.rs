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
use std::path::Path;

#[derive(Debug, Parser)]
#[command(about = "Standalone MCP server for the git_commit_with_fix tool")]
struct Args {
    /// Name or path of the Unix socket used for workflow events.
    #[arg(long)]
    socket: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GitCommitWithFixParams {
    /// The commit message to carry into the fix session.
    message: String,
}

#[derive(Debug, Serialize)]
struct CommitFixEvent<'a> {
    event: &'static str,
    instructions: &'static str,
    commit_message: &'a str,
}

const COMMIT_FIX_EVENT: &str = "commit-fix";
const COMMIT_FIX_INSTRUCTIONS: &str = "The main session requested a commit-fix session. Investigate the current problem, fix the underlying issue, run the provided check, lint, and test tools, and commit the resulting changes with the supplied commit message using the available Git tools.";

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
        description = "Requests a commit-fix session for the current work",
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
        tracing::info!(%params.message, "commit-fix session requested");
        let event = CommitFixEvent {
            event: COMMIT_FIX_EVENT,
            instructions: COMMIT_FIX_INSTRUCTIONS,
            commit_message: &params.message,
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
        COMMIT_FIX_EVENT, COMMIT_FIX_INSTRUCTIONS, CommitFixEvent, bridge_error, connect_to_bridge,
    };
    use std::io::ErrorKind;

    #[test]
    fn commit_fix_event_contains_workflow_instructions_and_message() {
        let event = CommitFixEvent {
            event: COMMIT_FIX_EVENT,
            instructions: COMMIT_FIX_INSTRUCTIONS,
            commit_message: "feat: preserve workflow",
        };

        let value = serde_json::to_value(event).expect("event is serializable");

        assert_eq!(value["event"], COMMIT_FIX_EVENT);
        assert_eq!(value["instructions"], COMMIT_FIX_INSTRUCTIONS);
        assert_eq!(value["commit_message"], "feat: preserve workflow");
    }

    #[test]
    fn startup_probe_reaches_a_listening_session() {
        let path = std::env::temp_dir().join(format!(
            "kid-agentic-coding-bridge-ping-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind succeeds");

        connect_to_bridge(&path.display().to_string()).expect("listening socket is reachable");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bridge_error_names_the_socket_and_the_fs_socket_dir_flag() {
        let socket = format!("kid-agentic-coding-bridge-test-{}", std::process::id());
        let err = std::io::Error::new(ErrorKind::NotFound, "no such file or directory");
        let message = bridge_error(&socket, &err);

        assert!(message.contains(&socket));
        assert!(message.contains("--fs-socket-dir"));
        assert!(message.contains("sandbox"));
    }
}
