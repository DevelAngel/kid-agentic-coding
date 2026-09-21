use crate::commit_fix::{COMMIT_FIX_DONE_EVENT, COMMIT_FIX_EVENT, CommitFixDone, CommitFixRequest};

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
