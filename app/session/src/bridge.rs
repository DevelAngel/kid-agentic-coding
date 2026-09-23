//! Async channel bridge between a running interactive session task and its consumer.
//!
//! This module has no ACP-specific knowledge beyond the event payloads it carries;
//! the protocol handling lives in [`crate::session`].

use std::sync::atomic::{AtomicUsize, Ordering};
use thiserror::Error;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot::Sender;
use wire::{CloseAction, CommitFixRequest};
pub use wire::{CloseActionVerdict, CommitFixVerdict};

/// Generates a process-unique id for [`SessionHandle`] lifecycle logging.
/// Unrelated to the agent's own protocol-level session id.
pub(crate) fn next_session_id() -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(1);
    format!("session-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Lifecycle state of a tool call as reported by the agent.
#[derive(Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Running,
    Done,
    Failed,
}

/// A choice offered to the user when the agent requests permission.
#[derive(Debug, Clone)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
}

/// Why the agent's turn ended.
#[derive(Debug, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    Cancelled,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    /// Debug text of a future variant not yet known to this crate.
    Other(String),
}

/// Events emitted from an interactive session to a UI layer.
#[derive(Debug)]
pub enum SessionEvent {
    /// A chunk of agent message content.
    Chunk(String),

    /// A chunk of the agent's internal reasoning.
    Thought(String),

    /// A new tool call has been initiated.
    ToolCall {
        id: String,
        title: String,
        status: ToolStatus,
        parameters: Option<String>,
        result: Option<String>,
    },

    /// A status or content update for an existing tool call.
    ToolCallUpdate {
        id: String,
        status: Option<ToolStatus>,
        parameters: Option<String>,
        result: Option<String>,
    },

    /// The agent reported a new active model.
    ModelChanged(String),

    /// The agent requests permission to proceed with a tool call.
    ///
    /// `tool_call_id` links the request to the tool call announced via a
    /// `tool_call` update, so the UI can look up the exact tool name;
    /// `title` names the specific call and `parameters` carries the raw
    /// input for display. Reply with `Some(option_id)` to select an option,
    /// or `None` to cancel.
    PermissionRequest {
        tool_call_id: String,
        title: String,
        parameters: Option<String>,
        options: Vec<PermissionOption>,
        reply: Sender<Option<String>>,
    },

    /// The commit workflow requested a dedicated fix session.
    CommitFix {
        request: CommitFixRequest,
        /// The requesting bridge connection awaits this verdict, so every
        /// request must be answered exactly once.
        verdict: Sender<CommitFixVerdict>,
    },

    /// A prompt was automatically sent by the workflow manager.
    AutoPrompt(String),

    /// The fix session committed its changes, closing the commit-fix workflow.
    CommitFixDone { commit_message: String },

    /// The fix session's close server requests authorization for a Git
    /// action (staging or committing) before it runs.
    CloseAction {
        action: CloseAction,
        /// The requesting bridge connection awaits this verdict, so every
        /// request must be answered exactly once.
        verdict: Sender<CloseActionVerdict>,
    },

    /// The fix session's close server reports that a previously authorized
    /// Git action has run, releasing the one-attempt-at-a-time gate. Not
    /// sent for a successful commit - `CommitFixDone` already reports that.
    CloseActionOutcome {
        action: CloseAction,
        success: bool,
        reason: Option<String>,
    },

    /// The confetti MCP tool was invoked successfully.
    Confetti,
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
    pub(crate) cancel_tx: UnboundedSender<()>,
    pub(crate) session_id: String,
    pub(crate) workflow_name: Option<String>,
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

    /// Cancels the current turn, if one is active.
    pub fn cancel(&self) {
        let _ = self.cancel_tx.send(());
    }

    /// Identifies this session for lifecycle logging. Unique per handle,
    /// unrelated to the agent's own protocol-level session id.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Name of the fix workflow this session was opened for, or `None` for
    /// the main session.
    pub fn workflow_name(&self) -> Option<&str> {
        self.workflow_name.as_deref()
    }

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
        let (prompt_tx, _prompt_rx) = mpsc::unbounded_channel();
        let (_event_tx, event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = mpsc::unbounded_channel();
        Self {
            prompt_tx,
            event_rx,
            cancel_tx,
            session_id: next_session_id(),
            workflow_name: None,
        }
    }

    /// Builds a `SessionHandle` alongside its prompt receiver, for tests that
    /// need `send_prompt` to succeed rather than report a closed session.
    #[doc(hidden)]
    pub fn new_connected_for_test() -> (Self, UnboundedReceiver<String>) {
        let (prompt_tx, prompt_rx) = mpsc::unbounded_channel();
        let (_event_tx, event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = mpsc::unbounded_channel();
        (
            Self {
                prompt_tx,
                event_rx,
                cancel_tx,
                session_id: next_session_id(),
                workflow_name: None,
            },
            prompt_rx,
        )
    }

    #[doc(hidden)]
    pub fn new_cancelable_for_test() -> (Self, UnboundedReceiver<()>) {
        let (prompt_tx, _prompt_rx) = mpsc::unbounded_channel();
        let (_event_tx, event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, cancel_rx) = mpsc::unbounded_channel();
        (
            Self {
                prompt_tx,
                event_rx,
                cancel_tx,
                session_id: next_session_id(),
                workflow_name: None,
            },
            cancel_rx,
        )
    }
}
