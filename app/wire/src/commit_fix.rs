use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

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
