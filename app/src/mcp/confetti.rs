//! Confetti MCP tool, registered with the agent when it advertises MCP support.
//!
//! Tool invocation handling is out of scope here; the tool currently returns
//! a fixed placeholder result.

use crate::bridge::SessionEvent;
use agent_client_protocol::mcp_server::McpServer;
use agent_client_protocol::tool_fn;
use agent_client_protocol_rmcp::McpServerExt;

use agent_client_protocol::{Agent, Error, ErrorCode, RunWithConnectionTo};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

/// Stable name of the confetti tool, as advertised to the agent.
pub const CONFETTI_TOOL_NAME: &str = "confetti";

/// Empty input contract: the confetti tool takes no parameters.
#[derive(Debug, Deserialize, JsonSchema)]
struct ConfettiParams {}

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
