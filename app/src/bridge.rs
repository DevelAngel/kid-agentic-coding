//! Async channel bridge between a running interactive session task and its consumer.
//!
//! This module has no ACP-specific knowledge beyond the event payloads it carries;
//! the protocol handling lives in [`crate::session`].

use agent_client_protocol::schema::v1::{
    ContentBlock, PermissionOption, StopReason, ToolCallId, ToolCallStatus,
};
use thiserror::Error;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot::Sender;

/// Events emitted from an interactive session to a UI layer.
#[derive(Debug)]
pub enum SessionEvent {
    /// A chunk of agent message content.
    Chunk(Box<ContentBlock>),
    /// A chunk of the agent's internal reasoning.
    Thought(Box<ContentBlock>),
    /// A new tool call has been initiated.
    ToolCall {
        id: ToolCallId,
        title: String,
        status: ToolCallStatus,
    },
    /// A status or content update for an existing tool call.
    ToolCallUpdate {
        id: ToolCallId,
        status: Option<ToolCallStatus>,
    },
    /// The agent requests permission to proceed.
    ///
    /// Reply with `Some(option_id)` to select an option, or `None` to cancel.
    PermissionRequest {
        options: Vec<PermissionOption>,
        reply: Sender<Option<String>>,
    },
    /// The current turn ended with the given reason. The session stays open
    /// for further prompts.
    Stopped(StopReason),

    /// The session task ended because of an error.
    Error(String),
}

/// Returned by [`SessionHandle::send_prompt`] when the session task has already ended.
#[derive(Debug, Error)]
#[error("interactive session task has ended")]
pub struct SessionClosed;

/// Handle to a running interactive ACP session.
///
/// Prompts are sent via [`SessionHandle::send_prompt`], updates are consumed via
/// [`SessionHandle::recv_event`]. The underlying agent connection stays open across
/// multiple turns until the handle is dropped.
pub struct SessionHandle {
    pub(crate) prompt_tx: UnboundedSender<String>,
    pub(crate) event_rx: UnboundedReceiver<SessionEvent>,
}

impl SessionHandle {
    /// Sends a new prompt into the running session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionClosed`] if the session task has already ended.
    pub fn send_prompt(&self, prompt_text: impl ToString) -> Result<(), SessionClosed> {
        self.prompt_tx
            .send(prompt_text.to_string())
            .map_err(|_| SessionClosed)
    }

    /// Awaits the next event from the session.
    ///
    /// Returns `None` once the session task has ended (e.g. startup failed,
    /// or the agent connection closed).
    pub async fn recv_event(&mut self) -> Option<SessionEvent> {
        self.event_rx.recv().await
    }

    /// Builds a `SessionHandle` backed by disconnected channels, for tests
    /// that need a handle but don't exercise prompt sending or event receiving.
    #[doc(hidden)]
    pub fn new_disconnected_for_test() -> Self {
        let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            prompt_tx,
            event_rx,
        }
    }

    /// Builds a `SessionHandle` alongside its prompt receiver, for tests that
    /// need `send_prompt` to succeed rather than report a closed session.
    #[doc(hidden)]
    pub fn new_connected_for_test() -> (Self, UnboundedReceiver<String>) {
        let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                prompt_tx,
                event_rx,
            },
            prompt_rx,
        )
    }
}
