use serde::{Deserialize, Serialize};

pub const CLOSE_ACTION_EVENT: &str = "close-action";
pub const CLOSE_ACTION_OUTCOME_EVENT: &str = "close-action-outcome";

/// A close-side Git action gated by the workflow state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseAction {
    Add,
    Commit,
}

/// The `close-action` workflow event, sent as one JSON line before the
/// requester runs the corresponding Git command.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename = "close-action")]
pub struct CloseActionRequest {
    pub action: CloseAction,
}

impl CloseActionRequest {
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// The app's answer to a [`CloseActionRequest`], sent back as one JSON line.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum CloseActionVerdict {
    Authorized,
    Rejected { reason: String },
}

impl CloseActionVerdict {
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// The `close-action-outcome` workflow event, sent as one JSON line once the
/// requester's Git command has run, so the app can release the
/// one-attempt-at-a-time gate. Not sent when a commit succeeds -
/// `CommitFixDone` already reports that outcome.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename = "close-action-outcome")]
pub struct CloseActionOutcome {
    pub action: CloseAction,
    pub success: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

impl CloseActionOutcome {
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}
