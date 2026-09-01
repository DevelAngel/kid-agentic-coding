//! Linux namespace sandbox for isolating an agent process.
//!
//! Confines a process to a project directory using mount, user, PID, and
//! network namespaces, dropping root privileges before the process image is
//! replaced by the sandboxed command.

use std::ffi::CString;
use std::path::Path;

use nix::mount::{MsFlags, mount};
use nix::sched::{CloneFlags, unshare};
use nix::sys::wait::waitpid;
use nix::unistd::{ForkResult, Gid, Uid, execvp, fork, getgid, getuid, setgid, setuid};
use thiserror::Error;

/// Errors returned while entering or running inside the sandbox.
#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("failed to unshare namespaces: {0}")]
    Unshare(#[source] nix::Error),
    #[error("failed to write {0}: {1}")]
    IdMap(&'static str, #[source] std::io::Error),
    #[error("failed to mount {0}: {1}")]
    Mount(&'static str, #[source] nix::Error),
    #[error("failed to drop privileges: {0}")]
    DropPrivileges(#[source] nix::Error),
    #[error("failed to create sandbox directory {0}: {1}")]
    CreateDir(&'static str, #[source] std::io::Error),
    #[error("failed to fork sandbox init process: {0}")]
    Fork(#[source] nix::Error),
    #[error("failed to wait for sandbox init process: {0}")]
    Wait(#[source] nix::Error),
    #[error("no command given to sandbox")]
    EmptyCommand,
    #[error("argument is not a valid C string: {0}")]
    InvalidArgument(String),
    #[error("failed to exec {0}: {1}")]
    Exec(String, #[source] nix::Error),
}

/// The path inside the sandbox where the project is exposed.
pub const SANDBOX_PROJECT_PATH: &str = "/projects";

/// Unprivileged user and group id used inside the sandbox, regardless of
/// the privileges of the process entering the sandbox.
const UNPRIVILEGED_ID: u32 = 65534; // nobody / nogroup

/// Enters a new mount, user, PID, and network namespace, exposes
/// `project_path` at [`SANDBOX_PROJECT_PATH`], drops to an unprivileged
/// user, then replaces the process image with `command` (program followed
/// by its arguments).
///
/// The calling process forks: the parent waits for the sandboxed child to
/// exit so the child becomes PID 1 of the new PID namespace. All sandbox
/// setup (mount changes, privilege drop, exec) happens in the child.
///
/// # Errors
///
/// Returns [`SandboxError`] if any sandbox setup step fails. On success in
/// the child, this function does not return: the process image has been
/// replaced by `execvp`.
pub fn enter_and_exec(project_path: &Path, command: &[String]) -> Result<(), SandboxError> {
    let outer_uid = getuid();
    let outer_gid = getgid();

    unshare(
        CloneFlags::CLONE_NEWUSER
            | CloneFlags::CLONE_NEWNS
            | CloneFlags::CLONE_NEWPID
            | CloneFlags::CLONE_NEWNET,
    )
    .map_err(SandboxError::Unshare)?;

    write_id_map("/proc/self/uid_map", outer_uid.as_raw())?;
    std::fs::write("/proc/self/setgroups", b"deny")
        .map_err(|source| SandboxError::IdMap("/proc/self/setgroups", source))?;
    write_id_map("/proc/self/gid_map", outer_gid.as_raw())?;

    // CLONE_NEWPID only affects children, not the calling process itself:
    // fork so the sandboxed command actually becomes PID 1 of the new
    // PID namespace.
    match unsafe { fork() }.map_err(SandboxError::Fork)? {
        ForkResult::Parent { child } => {
            waitpid(child, None).map_err(SandboxError::Wait)?;
            Ok(())
        }
        ForkResult::Child => run_sandboxed_child(project_path, command),
    }
}

fn run_sandboxed_child(project_path: &Path, command: &[String]) -> ! {
    if let Err(error) = mount_project(project_path).and_then(|()| drop_privileges()) {
        eprintln!("sandbox: {error}");
        std::process::exit(1);
    }
    let error = exec_command(command).unwrap_err();
    eprintln!("sandbox: {error}");
    std::process::exit(1);
}

fn write_id_map(path: &'static str, id: u32) -> Result<(), SandboxError> {
    std::fs::write(path, format!("0 {id} 1\n")).map_err(|source| SandboxError::IdMap(path, source))
}

fn mount_project(project_path: &Path) -> Result<(), SandboxError> {
    // Make the root mount private first so the bind mount below does not
    // propagate back to the host mount namespace.
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_PRIVATE | MsFlags::MS_REC,
        None::<&str>,
    )
    .map_err(|source| SandboxError::Mount("/", source))?;

    std::fs::create_dir_all(SANDBOX_PROJECT_PATH)
        .map_err(|source| SandboxError::CreateDir(SANDBOX_PROJECT_PATH, source))?;

    mount(
        Some(project_path),
        SANDBOX_PROJECT_PATH,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .map_err(|source| SandboxError::Mount(SANDBOX_PROJECT_PATH, source))
}

fn drop_privileges() -> Result<(), SandboxError> {
    setgid(Gid::from_raw(UNPRIVILEGED_ID)).map_err(SandboxError::DropPrivileges)?;
    setuid(Uid::from_raw(UNPRIVILEGED_ID)).map_err(SandboxError::DropPrivileges)
}

fn exec_command(command: &[String]) -> Result<(), SandboxError> {
    let [program, args @ ..] = command else {
        return Err(SandboxError::EmptyCommand);
    };
    let to_cstring = |argument: &String| {
        CString::new(argument.as_bytes())
            .map_err(|_| SandboxError::InvalidArgument(argument.clone()))
    };
    let program_c = to_cstring(program)?;
    let mut argv = vec![program_c.clone()];
    for argument in args {
        argv.push(to_cstring(argument)?);
    }

    execvp(&program_c, &argv)
        .map(|_| ())
        .map_err(|source| SandboxError::Exec(program.clone(), source))
}
