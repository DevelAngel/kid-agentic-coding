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
use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener};
use std::process;

/// Stable name of the confetti tool, as advertised to the agent.
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

/// Creates a Linux abstract-namespace Unix socket for the stdio MCP bridge.
pub fn confetti_socket_name() -> String {
    format!("kid-agentic-coding-confetti-{}", process::id())
}

pub fn bind_confetti_socket(socket_name: &str) -> io::Result<UnixListener> {
    let address = SocketAddr::from_abstract_name(socket_name.as_bytes())?;
    let listener = UnixListener::bind_addr(&address)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
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
