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
use derive_more::Deref;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use std::env;
use std::fs;
use std::io;
use std::marker::PhantomData;
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

/// Builds the stdio MCP server configuration for the gh issue tools.
pub fn gh_stdio_mcp_server() -> io::Result<SchemaMcpServer> {
    let command = env::current_exe()?.with_file_name("kid-agentic-coding-gh");
    Ok(SchemaMcpServer::Stdio(McpServerStdio::new(
        "gh-issue-tools",
        command,
    )))
}

pub fn stdio_mcp_servers(
    socket_name: &str,
    workflow_socket_name: &str,
    session_root: &Path,
) -> io::Result<Vec<SchemaMcpServer>> {
    let mut servers = vec![
        confetti_stdio_mcp_server(socket_name)?,
        git_commit_fix_open_stdio_mcp_server(workflow_socket_name)?,
    ];
    push_gh_issue_tools(&mut servers, session_root)?;
    Ok(servers)
}

pub fn stdio_mcp_servers_without_confetti(
    workflow_socket_name: &str,
    session_root: &Path,
) -> io::Result<Vec<SchemaMcpServer>> {
    let mut servers = vec![git_commit_fix_open_stdio_mcp_server(workflow_socket_name)?];
    push_gh_issue_tools(&mut servers, session_root)?;
    Ok(servers)
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

/// Registers the gh issue tools only when a repository with a
/// github.com-hosted remote is found in the workspace tree, following the
/// fix-session toolchain gating pattern.
fn push_gh_issue_tools(servers: &mut Vec<SchemaMcpServer>, session_root: &Path) -> io::Result<()> {
    if has_github_repo(session_root) {
        servers.push(gh_stdio_mcp_server()?);
        tracing::info!(
            "GitHub issue tools enabled: github.com-hosted repository found in workspace"
        );
    } else {
        tracing::warn!(
            "GitHub issue tools disabled: no github.com-hosted repository found in workspace"
        );
    }
    Ok(())
}

/// Whether any directory in the tree rooted at `root` is a repository with a
/// github.com-hosted remote. A directory containing a `.git` entry (repo root
/// or worktree) is checked via git and never descended into.
fn has_github_repo(root: &Path) -> bool {
    let mut stack = vec![root.to_path_buf()];

    while let Some(directory) = stack.pop() {
        let entries: Vec<(String, PathBuf, bool)> = match fs::read_dir(&directory) {
            Ok(entries) => entries
                .flatten()
                .map(|entry| {
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        entry.path(),
                        entry.file_type().is_ok_and(|t| t.is_dir()),
                    )
                })
                .collect(),
            Err(err) => {
                tracing::debug!(
                    directory = %directory.display(),
                    ?err,
                    "skipping unreadable directory during GitHub repository scan"
                );
                continue;
            }
        };

        if entries.iter().any(|(name, _, _)| name == ".git") {
            if git_repo_has_github_remote(&directory) {
                return true;
            }
            continue;
        }

        for (name, path, is_dir) in entries {
            if is_dir && name != "target" && name != "node_modules" {
                stack.push(path);
            }
        }
    }

    false
}

/// Whether any remote of the repository rooted at `repo` is hosted on
/// github.com. Global and system git configuration is excluded so only the
/// repository's own remotes count.
fn git_repo_has_github_remote(repo: &Path) -> bool {
    let output = match process::Command::new("git")
        .args(["config", "--get-regexp", "^remote\\..*\\.url$"])
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
    {
        Ok(output) => output,
        Err(err) => {
            tracing::warn!(?err, "failed to run git while checking for a GitHub remote");
            return false;
        }
    };

    if !output.status.success() {
        // Exit status 1 means no remote is configured, 128 means git could
        // not interpret the directory as a repository.
        return false;
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.split_once(' ').map_or("", |(_, url)| url))
        .any(is_github_remote_url)
}

/// Strict github.com host match covering https://, ssh:// and scp-style
/// `git@github.com:` URLs while excluding GitHub Enterprise hosts.
fn is_github_remote_url(url: &str) -> bool {
    let url = url.trim();
    let host = if let Some((_, rest)) = url.split_once("://") {
        rest.split(['/', ':']).next().unwrap_or("")
    } else if let Some((host, _path)) = url.split_once(':') {
        host
    } else {
        return false;
    };

    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    host.eq_ignore_ascii_case("github.com")
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

/// Directory for filesystem-fallback bridge sockets, distinct from
/// `Option<PathBuf>` so it cannot be swapped with a session root at a
/// call site without a type error.
#[derive(Debug, Clone, Deref)]
pub struct FsSocketDir(pub Option<PathBuf>);

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

pub struct Unbound;
pub struct WorkflowBound;
pub struct AllBound;
pub struct BridgeSockets<State> {
    workflow_socket: String,
    confetti_socket: Option<String>,
    workflow_listener: Option<UnixListener>,
    confetti_listener: Option<UnixListener>,
    state: PhantomData<State>,
}

impl BridgeSockets<Unbound> {
    pub fn new(workflow_socket: String, confetti_socket: Option<String>) -> Self {
        Self {
            workflow_socket,
            confetti_socket,
            workflow_listener: None,
            confetti_listener: None,
            state: PhantomData,
        }
    }

    pub fn bind_workflow(mut self) -> io::Result<BridgeSockets<WorkflowBound>> {
        self.workflow_listener = Some(bind_bridge_socket(&self.workflow_socket)?);
        Ok(BridgeSockets {
            workflow_socket: self.workflow_socket,
            confetti_socket: self.confetti_socket,
            workflow_listener: self.workflow_listener,
            confetti_listener: self.confetti_listener,
            state: PhantomData,
        })
    }
}

impl BridgeSockets<WorkflowBound> {
    pub fn bind_confetti(mut self) -> io::Result<BridgeSockets<AllBound>> {
        let socket = match self.confetti_socket.as_deref() {
            Some(socket) => socket,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "confetti socket is not configured",
                ));
            }
        };
        self.confetti_listener = Some(bind_bridge_socket(socket)?);
        Ok(BridgeSockets {
            workflow_socket: self.workflow_socket,
            confetti_socket: self.confetti_socket,
            workflow_listener: self.workflow_listener,
            confetti_listener: self.confetti_listener,
            state: PhantomData,
        })
    }

    pub fn stdio_mcp_servers_without_confetti(
        &self,
        session_root: &Path,
    ) -> io::Result<Vec<SchemaMcpServer>> {
        stdio_mcp_servers_without_confetti(&self.workflow_socket, session_root)
    }

    pub fn stdio_mcp_servers_for_fix_session(
        &self,
        session_root: &Path,
    ) -> io::Result<Vec<SchemaMcpServer>> {
        stdio_mcp_servers_for_fix_session(&self.workflow_socket, session_root)
    }
}
impl BridgeSockets<AllBound> {
    pub fn stdio_mcp_servers(&self, session_root: &Path) -> io::Result<Vec<SchemaMcpServer>> {
        let confetti_socket = self.confetti_socket.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "confetti socket is not configured",
            )
        })?;
        stdio_mcp_servers(confetti_socket, &self.workflow_socket, session_root)
    }
}

impl BridgeSockets<WorkflowBound> {
    pub fn into_listeners(self) -> io::Result<(UnixListener, Option<UnixListener>)> {
        let workflow_listener = match self.workflow_listener {
            Some(listener) => listener,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "workflow socket is not bound",
                ));
            }
        };
        Ok((workflow_listener, self.confetti_listener))
    }
}

impl BridgeSockets<AllBound> {
    pub fn into_listeners(self) -> io::Result<(UnixListener, UnixListener)> {
        let workflow_listener = match self.workflow_listener {
            Some(listener) => listener,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "workflow socket is not bound",
                ));
            }
        };
        let confetti_listener = match self.confetti_listener {
            Some(listener) => listener,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "confetti socket is not bound",
                ));
            }
        };
        Ok((workflow_listener, confetti_listener))
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
    tracing::debug!(socket, ?address, "binding bridge socket");
    let listener = UnixListener::bind_addr(&address)?;
    tracing::info!(socket, "bridge socket bound successfully");
    listener.set_nonblocking(true)?;
    Ok(listener)
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
    use super::{BridgeSockets, SocketFileGuard, Unbound, fs_socket_path, socket_address};
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
        let listener = BridgeSockets::<Unbound>::new(identifier, None)
            .bind_workflow()
            .expect("bind succeeds")
            .into_listeners()
            .expect("workflow socket is bound")
            .0;
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

        let _listener = BridgeSockets::<Unbound>::new(identifier, None)
            .bind_workflow()
            .expect("bind over a stale file succeeds")
            .into_listeners()
            .expect("workflow socket is bound")
            .0;
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

#[cfg(test)]
mod github_registration_tests {
    use super::{is_github_remote_url, stdio_mcp_servers, stdio_mcp_servers_without_confetti};
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process;

    const GITHUB_REMOTE: &str = "https://github.com/owner/repo.git";
    const NON_GITHUB_REMOTE: &str = "https://gitlab.com/owner/repo.git";

    fn temp_workspace() -> PathBuf {
        let path = env::temp_dir().join(format!(
            "kid-agentic-coding-github-{}-{}",
            process::id(),
            super::next_socket_id()
        ));
        fs::create_dir_all(&path).expect("workspace is created");
        path
    }

    fn make_repo(root: &Path, remote_url: &str) {
        process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .output()
            .expect("git init runs");
        process::Command::new("git")
            .args(["remote", "add", "origin", remote_url])
            .current_dir(root)
            .output()
            .expect("git remote add runs");
    }

    #[test]
    fn is_github_remote_url_matches_only_github_com_hosts() {
        assert!(is_github_remote_url("https://github.com/owner/repo.git"));
        assert!(is_github_remote_url("git@github.com:owner/repo.git"));
        assert!(is_github_remote_url("ssh://git@github.com/owner/repo.git"));
        assert!(!is_github_remote_url(
            "https://github.example.com/owner/repo.git"
        ));
        assert!(!is_github_remote_url("https://gitlab.com/owner/repo.git"));
        assert!(!is_github_remote_url("owner/repo.git"));
    }

    #[test]
    fn registers_gh_tools_for_a_github_repo_at_the_workspace_root() {
        let workspace = temp_workspace();
        make_repo(&workspace, GITHUB_REMOTE);

        let servers = stdio_mcp_servers("confetti", "workflow", &workspace)
            .expect("MCP server registration succeeds");
        assert_eq!(servers.len(), 3);

        fs::remove_dir_all(workspace).expect("workspace is removed");
    }

    #[test]
    fn registers_gh_tools_for_a_nested_github_repo() {
        let workspace = temp_workspace();
        let project = workspace.join("projects").join("web");
        fs::create_dir_all(&project).expect("project dir is created");
        make_repo(&project, GITHUB_REMOTE);

        let servers = stdio_mcp_servers_without_confetti("workflow", &workspace)
            .expect("MCP server registration succeeds");
        assert_eq!(servers.len(), 2);

        fs::remove_dir_all(workspace).expect("workspace is removed");
    }

    #[test]
    fn skips_gh_tools_without_a_github_repo() {
        let workspace = temp_workspace();
        let project = workspace.join("project");
        fs::create_dir_all(&project).expect("project dir is created");
        make_repo(&project, NON_GITHUB_REMOTE);

        let servers = stdio_mcp_servers_without_confetti("workflow", &workspace)
            .expect("MCP server registration succeeds");
        assert_eq!(servers.len(), 1);

        fs::remove_dir_all(workspace).expect("workspace is removed");
    }
}
