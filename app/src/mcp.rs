//! Confetti MCP tool, registered with the agent when it advertises MCP support.
//!
//! Tool invocation handling is out of scope here; the tool currently returns
//! a fixed placeholder result.

use crate::bridge::SessionEvent;

use agent_client_protocol::mcp_server::McpServer;
use agent_client_protocol::schema::v1::InitializeResponse;
use agent_client_protocol::schema::v1::{McpServer as SchemaMcpServer, McpServerStdio};
use agent_client_protocol::tool_fn;
use agent_client_protocol::{Agent, Error, ErrorCode, RunWithConnectionTo};
use agent_client_protocol_rmcp::McpServerExt;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use std::env;
use std::fs;
use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

/// Stable name of the semantic event emitted by git-commit-fix-open.
pub const COMMIT_FIX_EVENT: &str = "commit-fix";

/// Stable name of the semantic event emitted when a commit-fix session closes.
pub const COMMIT_FIX_DONE_EVENT: &str = "commit-fix-done";

pub const CONFETTI_TOOL_NAME: &str = "confetti";

/// Empty input contract: the confetti tool takes no parameters.
#[derive(Debug, Deserialize, JsonSchema)]
struct ConfettiParams {}

/// Whether the connected agent advertises MCP-over-ACP support.
pub fn supports_mcp(init_response: &InitializeResponse) -> bool {
    init_response.agent_capabilities.mcp_capabilities.acp
}

fn emit_confetti(event_tx: &UnboundedSender<SessionEvent>) -> Result<(), Error> {
    event_tx
        .send(SessionEvent::Confetti)
        .map_err(|_| Error::from(ErrorCode::InternalError))
}

/// Builds the MCP server exposing the confetti tool for attachment to a session.
pub fn confetti_mcp_server(
    event_tx: UnboundedSender<SessionEvent>,
) -> McpServer<Agent, impl RunWithConnectionTo<Agent>> {
    McpServer::builder("confetti-tools")
        .tool_fn(
            CONFETTI_TOOL_NAME,
            "Triggers a confetti celebration",
            async move |_params: ConfettiParams, _cx| {
                tracing::debug!("confetti MCP tool invoked");
                emit_confetti(&event_tx)?;
                Ok::<_, Error>("confetti invoked")
            },
            tool_fn!(),
        )
        .build()
}

/// Builds the stdio MCP server configuration used by agents without
/// MCP-over-ACP support.
pub fn confetti_stdio_mcp_server(socket_name: &str) -> io::Result<SchemaMcpServer> {
    let command = env::current_exe()?.with_file_name("kid-agentic-coding-confetti");
    Ok(SchemaMcpServer::Stdio(
        McpServerStdio::new("confetti-tools", command)
            .args(vec!["--socket".to_owned(), socket_name.to_owned()]),
    ))
}

/// Builds the stdio MCP server configuration for the rust-mcp tools, used
/// by agents without MCP-over-ACP support.
pub fn rust_stdio_mcp_server() -> io::Result<SchemaMcpServer> {
    let command = env::current_exe()?.with_file_name("kid-agentic-coding-rust");
    Ok(SchemaMcpServer::Stdio(McpServerStdio::new(
        "rust-tools",
        command,
    )))
}

/// Builds the stdio MCP server configuration for the python-mcp tools, used
/// by agents without MCP-over-ACP support.
pub fn python_stdio_mcp_server() -> io::Result<SchemaMcpServer> {
    let command = env::current_exe()?.with_file_name("kid-agentic-coding-python");
    Ok(SchemaMcpServer::Stdio(McpServerStdio::new(
        "python-tools",
        command,
    )))
}

/// Builds the stdio MCP server configuration for the git-commit-fix-open tool.
/// The socket receives semantic workflow events from the server process.
pub fn git_commit_fix_open_stdio_mcp_server(socket_name: &str) -> io::Result<SchemaMcpServer> {
    let command = env::current_exe()?.with_file_name("kid-agentic-coding-git-commit-fix-open");
    Ok(SchemaMcpServer::Stdio(
        McpServerStdio::new("git-commit-fix-open-tools", command)
            .args(vec!["--socket".to_owned(), socket_name.to_owned()]),
    ))
}

/// Builds the stdio MCP server configuration for the git-commit-fix-close tools.
/// The socket receives the commit-fix-done event once a commit closes the session.
pub fn git_commit_fix_close_stdio_mcp_server(socket_name: &str) -> io::Result<SchemaMcpServer> {
    let command = env::current_exe()?.with_file_name("kid-agentic-coding-git-commit-fix-close");
    Ok(SchemaMcpServer::Stdio(
        McpServerStdio::new("git-commit-fix-close-tools", command)
            .args(vec!["--socket".to_owned(), socket_name.to_owned()]),
    ))
}

pub fn stdio_mcp_servers(
    socket_name: &str,
    workflow_socket_name: &str,
) -> io::Result<Vec<SchemaMcpServer>> {
    Ok(vec![
        confetti_stdio_mcp_server(socket_name)?,
        git_commit_fix_open_stdio_mcp_server(workflow_socket_name)?,
    ])
}

pub fn stdio_mcp_servers_without_confetti(
    workflow_socket_name: &str,
) -> io::Result<Vec<SchemaMcpServer>> {
    Ok(vec![git_commit_fix_open_stdio_mcp_server(
        workflow_socket_name,
    )?])
}

fn find_lockfile(root: &Path, file_name: &str) -> io::Result<Option<PathBuf>> {
    let mut directories = vec![root.to_path_buf()];

    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_file() && entry.file_name() == file_name {
                return Ok(Some(path));
            }
            if file_type.is_dir() {
                directories.push(path);
            }
        }
    }

    Ok(None)
}

pub fn stdio_mcp_servers_for_fix_session(
    workflow_socket_name: &str,
    session_root: &Path,
) -> io::Result<Vec<SchemaMcpServer>> {
    let rust_lockfile = find_lockfile(session_root, "Cargo.lock")?;
    if rust_lockfile.is_some() {
        tracing::info!("Rust tools enabled: Cargo.lock found in fix-session workspace");
    } else {
        tracing::warn!("Rust tools disabled: no Cargo.lock found in fix-session workspace");
    }

    let python_lockfile = find_lockfile(session_root, "uv.lock")?;
    if python_lockfile.is_some() {
        tracing::info!("Python tools enabled: uv.lock found in fix-session workspace");
    } else {
        tracing::warn!("Python tools disabled: no uv.lock found in fix-session workspace");
    }

    let mut servers = Vec::with_capacity(3);
    if rust_lockfile.is_some() {
        servers.push(rust_stdio_mcp_server()?);
    }
    if python_lockfile.is_some() {
        servers.push(python_stdio_mcp_server()?);
    }
    servers.push(git_commit_fix_close_stdio_mcp_server(workflow_socket_name)?);
    Ok(servers)
}

/// Monotonic counter distinguishing sockets bound within the same process,
/// since two sessions (e.g. main and fix) can be alive at overlapping times.
fn next_socket_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Unique name of this process's workflow bridge socket.
pub fn workflow_socket_name() -> String {
    format!(
        "kid-agentic-coding-workflow-{}-{}",
        process::id(),
        next_socket_id()
    )
}

/// Unique name of this process's confetti bridge socket.
pub fn confetti_socket_name() -> String {
    format!(
        "kid-agentic-coding-confetti-{}-{}",
        process::id(),
        next_socket_id()
    )
}

/// Path of a bridge socket in the filesystem fallback directory. Used when
/// the agent is sandboxed and cannot reach Linux abstract-namespace sockets.
pub fn fs_socket_path(directory: &Path, socket_name: &str) -> PathBuf {
    directory.join(format!("{socket_name}.sock"))
}

/// Resolves a bridge socket identifier to a socket address. Identifiers
/// containing a path separator are filesystem paths; bare identifiers are
/// Linux abstract-namespace names.
pub fn socket_address(socket: &str) -> io::Result<SocketAddr> {
    if socket.contains('/') {
        Ok(SocketAddr::from_pathname(Path::new(socket))?)
    } else {
        Ok(SocketAddr::from_abstract_name(socket.as_bytes())?)
    }
}

fn bind_bridge_socket(socket: &str) -> io::Result<UnixListener> {
    let address = socket_address(socket)?;
    if let Some(path) = address.as_pathname() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // A stale socket file from a previous or crashed run would make the
        // bind fail, so it is removed before rebinding.
        if let Err(err) = fs::remove_file(path)
            && err.kind() != io::ErrorKind::NotFound
        {
            return Err(err);
        }
    }
    let listener = UnixListener::bind_addr(&address)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

pub fn bind_confetti_socket(socket: &str) -> io::Result<UnixListener> {
    bind_bridge_socket(socket)
}

pub fn bind_workflow_socket(socket: &str) -> io::Result<UnixListener> {
    bind_bridge_socket(socket)
}

/// Removes a filesystem bridge socket file when the session that bound it
/// ends. Abstract-namespace sockets vanish with the binding process and need
/// no such cleanup.
pub struct SocketFileGuard {
    path: Option<PathBuf>,
}

impl SocketFileGuard {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }
}

impl Drop for SocketFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take()
            && let Err(err) = fs::remove_file(&path)
            && err.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(?err, path = %path.display(), "failed to remove bridge socket file");
        }
    }
}

#[cfg(test)]
mod bridge_socket_tests {
    use super::{SocketFileGuard, bind_workflow_socket, fs_socket_path, socket_address};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::{env, fs, process};

    #[test]
    fn fs_socket_path_extends_the_fallback_directory() {
        assert_eq!(
            fs_socket_path(
                Path::new("/run/user/1000/kid-agentic-coding"),
                "kid-agentic-coding-workflow-1-2"
            ),
            Path::new("/run/user/1000/kid-agentic-coding/kid-agentic-coding-workflow-1-2.sock")
        );
    }

    #[test]
    fn socket_address_distinguishes_paths_from_abstract_names() {
        assert!(
            socket_address("/run/user/1000/kid-agentic-coding/bridge.sock")
                .expect("socket address is valid")
                .as_pathname()
                .is_some()
        );
        assert!(
            socket_address("kid-agentic-coding-workflow-1-2")
                .expect("socket address is valid")
                .as_pathname()
                .is_none()
        );
    }

    #[test]
    fn filesystem_socket_is_reachable_and_removed_by_the_guard() {
        let path =
            env::temp_dir().join(format!("kid-agentic-coding-bridge-{}.sock", process::id()));
        let identifier = path.display().to_string();

        let guard = SocketFileGuard::new(Some(path.clone()));
        let listener = bind_workflow_socket(&identifier).expect("bind succeeds");
        assert!(path.exists());
        UnixStream::connect(&path).expect("socket is reachable via the path");
        drop(listener);
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn binding_replaces_a_stale_socket_file() {
        let path = env::temp_dir().join(format!("kid-agentic-coding-stale-{}.sock", process::id()));
        let identifier = path.display().to_string();
        fs::write(&path, b"stale").expect("stale file is created");

        let _listener = bind_workflow_socket(&identifier).expect("bind over a stale file succeeds");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn guard_tolerates_a_missing_file() {
        let path =
            env::temp_dir().join(format!("kid-agentic-coding-missing-{}.sock", process::id()));
        let _ = SocketFileGuard::new(Some(path));
    }
}

#[cfg(test)]
mod confetti_tests {
    use super::emit_confetti;
    use crate::bridge::SessionEvent;
    use tokio::sync::mpsc::unbounded_channel;

    #[tokio::test]
    async fn invocation_emits_a_confetti_event() {
        let (event_tx, mut event_rx) = unbounded_channel();

        emit_confetti(&event_tx).expect("event receiver is connected");

        assert!(matches!(
            event_rx.recv().await,
            Some(SessionEvent::Confetti)
        ));
    }

    #[test]
    fn invocation_fails_when_event_receiver_is_disconnected() {
        let (event_tx, event_rx) = unbounded_channel();
        drop(event_rx);

        assert!(emit_confetti(&event_tx).is_err());
    }
}

#[cfg(test)]
mod supports_mcp_tests {
    use super::supports_mcp;
    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::schema::v1::InitializeResponse;

    #[test]
    fn true_when_agent_advertises_mcp_acp_capability() {
        let mut response = InitializeResponse::new(ProtocolVersion::V1);
        response.agent_capabilities.mcp_capabilities.acp = true;

        assert!(supports_mcp(&response));
    }

    #[test]
    fn false_when_agent_does_not_advertise_mcp_acp_capability() {
        let response = InitializeResponse::new(ProtocolVersion::V1);

        assert!(!supports_mcp(&response));
    }
}

#[cfg(test)]
mod fix_session_toolchain_tests {
    use super::{find_lockfile, stdio_mcp_servers_for_fix_session};
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::process;

    fn temp_workspace() -> PathBuf {
        let path = env::temp_dir().join(format!(
            "kid-agentic-coding-lockfiles-{}-{}",
            process::id(),
            super::next_socket_id()
        ));
        fs::create_dir_all(&path).expect("workspace is created");
        path
    }

    #[test]
    fn finds_lockfiles_in_the_workspace_root() {
        let workspace = temp_workspace();
        fs::write(workspace.join("Cargo.lock"), b"").expect("Cargo.lock is created");
        fs::write(workspace.join("uv.lock"), b"").expect("uv.lock is created");

        assert!(
            find_lockfile(&workspace, "Cargo.lock")
                .expect("lockfile search succeeds")
                .is_some()
        );
        assert!(
            find_lockfile(&workspace, "uv.lock")
                .expect("lockfile search succeeds")
                .is_some()
        );

        fs::remove_dir_all(workspace).expect("workspace is removed");
    }

    #[test]
    fn finds_lockfiles_in_workspace_subdirectories() {
        let workspace = temp_workspace();
        let rust_project = workspace.join("rust-project");
        let python_project = workspace.join("python-project");
        fs::create_dir_all(&rust_project).expect("Rust project is created");
        fs::create_dir_all(&python_project).expect("Python project is created");
        fs::write(rust_project.join("Cargo.lock"), b"").expect("Cargo.lock is created");
        fs::write(python_project.join("uv.lock"), b"").expect("uv.lock is created");

        let servers = stdio_mcp_servers_for_fix_session("workflow", &workspace)
            .expect("MCP server registration succeeds");
        assert_eq!(servers.len(), 3);

        fs::remove_dir_all(workspace).expect("workspace is removed");
    }

    #[test]
    fn registers_only_git_tools_without_lockfiles() {
        let workspace = temp_workspace();

        let servers = stdio_mcp_servers_for_fix_session("workflow", &workspace)
            .expect("MCP server registration succeeds");
        assert_eq!(servers.len(), 1);

        fs::remove_dir_all(workspace).expect("workspace is removed");
    }

    #[test]
    fn registers_only_the_matching_toolchain() {
        let workspace = temp_workspace();
        fs::write(workspace.join("Cargo.lock"), b"").expect("Cargo.lock is created");

        let servers = stdio_mcp_servers_for_fix_session("workflow", &workspace)
            .expect("MCP server registration succeeds");
        assert_eq!(servers.len(), 2);

        fs::remove_file(workspace.join("Cargo.lock")).expect("Cargo.lock is removed");
        fs::write(workspace.join("uv.lock"), b"").expect("uv.lock is created");

        let servers = stdio_mcp_servers_for_fix_session("workflow", &workspace)
            .expect("MCP server registration succeeds");
        assert_eq!(servers.len(), 2);

        fs::remove_dir_all(workspace).expect("workspace is removed");
    }
}
