use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

mod socket;

pub use socket::{bridge_error, connect_to_bridge, send_line, socket_address};

pub const COMMIT_FIX_EVENT: &str = "commit-fix";
pub const COMMIT_FIX_DONE_EVENT: &str = "commit-fix-done";

/// How long the app takes to decide on a request before it answers with a
/// rejection.
pub const VERDICT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the requester waits for the verdict. Exceeds
/// [`VERDICT_TIMEOUT`] so the app's rejection reason reaches the requester
/// before it gives up.
pub const ACK_WAIT: Duration = VERDICT_TIMEOUT.saturating_add(Duration::from_secs(5));

/// The `commit-fix` workflow event, sent as one JSON line.
/// Only encoding writes the `event` tag; the receiver routes on it before
/// parsing.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename = "commit-fix")]
pub struct CommitFixRequest {
    pub instructions: String,
    pub amend: bool,
    pub tldr: String,
    pub why: String,
    pub what: String,
    pub cwd: Option<PathBuf>,
}

impl CommitFixRequest {
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// The app's answer to a [`CommitFixRequest`], sent back as one JSON line.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum CommitFixVerdict {
    /// The app is opening a fix session. Sent before the open completes,
    /// so the open itself can still fail.
    Opening,
    /// A fix session is already active or starting; the request was not
    /// executed.
    Ignored { reason: String },
    /// The request could not be honored: malformed payload, unknown event,
    /// or a session that is shutting down or did not answer in time.
    Rejected { reason: String },
}

impl CommitFixVerdict {
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// The `commit-fix-done` workflow event, sent as one JSON line once a
/// commit closes the fix session.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename = "commit-fix-done")]
pub struct CommitFixDone {
    #[serde(default)]
    pub commit_message: String,
}

impl CommitFixDone {
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// The confetti notification. Not JSON: the message is the bare word on
/// its own line.
pub struct Confetti;

impl Confetti {
    pub fn to_line(&self) -> &'static str {
        "confetti"
    }

    /// Accepts only the exact message a sender writes, newline included.
    pub fn parse(message: &[u8]) -> Option<Self> {
        (message == b"confetti\n").then_some(Self)
    }
}

/// A message received on the workflow socket.
#[derive(Debug, PartialEq)]
pub enum WorkflowEvent {
    CommitFix(CommitFixRequest),
    CommitFixDone(CommitFixDone),
}

impl WorkflowEvent {
    /// Routes on the `event` tag, then parses the payload. The error is the
    /// reason to reject the message with.
    pub fn parse(message: &[u8]) -> Result<Self, String> {
        let value = serde_json::from_slice::<serde_json::Value>(message)
            .map_err(|err| format!("workflow event is not valid JSON: {err}"))?;
        match value.get("event").and_then(serde_json::Value::as_str) {
            Some(COMMIT_FIX_EVENT) => serde_json::from_value(value)
                .map(Self::CommitFix)
                .map_err(|err| err.to_string()),
            Some(COMMIT_FIX_DONE_EVENT) => serde_json::from_value(value)
                .map(Self::CommitFixDone)
                .map_err(|err| err.to_string()),
            other => Err(format!(
                "unhandled workflow event '{}'",
                other.unwrap_or("<missing>")
            )),
        }
    }
}
