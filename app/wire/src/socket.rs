use std::io::{self, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::Path;

/// Resolves a bridge socket identifier to a socket address. Identifiers
/// containing a path separator are filesystem paths (the sandboxed-agent
/// fallback); bare identifiers are Linux abstract-namespace names.
pub fn socket_address(socket: &str) -> io::Result<SocketAddr> {
    if socket.contains('/') {
        SocketAddr::from_pathname(Path::new(socket))
    } else {
        SocketAddr::from_abstract_name(socket.as_bytes())
    }
}

pub fn connect_to_bridge(socket: &str) -> io::Result<UnixStream> {
    UnixStream::connect_addr(&socket_address(socket)?)
}

/// Writes `line` followed by the newline that terminates every message.
pub fn send_line(stream: &mut impl Write, line: &str) -> io::Result<()> {
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")
}

/// Full description of a failed bridge connection: which socket was
/// attempted, the underlying OS error, and the fix for the common
/// sandboxed-agent case. Used both for the startup probe log and for tool
/// errors so the agent can relay actionable guidance to the user.
pub fn bridge_error(socket: &str, err: &io::Error) -> String {
    format!(
        "bridge socket '{socket}' is unreachable: {err}. If the agent runs sandboxed, \
         start kid-agentic-coding with --fs-socket-dir pointing at a writable \
         directory that is mounted into the sandbox (e.g. \
         $XDG_RUNTIME_DIR/kid-agentic-coding), because Linux abstract-namespace \
         sockets cannot cross a sandbox boundary."
    )
}
