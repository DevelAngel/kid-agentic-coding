use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{
    SessionId, SessionNotification, SessionUpdate, ToolCallId, ToolCallStatus,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{Tool, ToolCall, ToolEvent, ToolResult};

/// Delivers the tool events of each session to the sink registered for it.
#[derive(Clone, Default)]
pub(super) struct Router {
    sessions: Arc<Mutex<HashMap<SessionId, Route>>>,
}

struct Route {
    sink: mpsc::Sender<ToolEvent>,
    tools: HashMap<ToolCallId, Tracked>,
}

struct Tracked {
    tool: Tool,
    input: Option<Value>,
}

impl Router {
    pub(super) fn register(&self, session_id: SessionId, sink: mpsc::Sender<ToolEvent>) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(
                session_id,
                Route {
                    sink,
                    tools: HashMap::new(),
                },
            );
        }
    }

    pub(super) fn remove(&self, session_id: &SessionId) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(session_id);
        }
    }

    pub(super) async fn dispatch(&self, notification: &SessionNotification) {
        let Some((sink, events)) = self.translate(notification) else {
            return;
        };
        for event in events {
            if sink.send(event).await.is_err() {
                tracing::warn!(session_id = ?notification.session_id, "tool event sink closed");
                break;
            }
        }
    }

    fn translate(
        &self,
        notification: &SessionNotification,
    ) -> Option<(mpsc::Sender<ToolEvent>, Vec<ToolEvent>)> {
        // A poisoned registry means another dispatch panicked; dropping the update is safe.
        let mut sessions = self.sessions.lock().ok()?;
        let Some(route) = sessions.get_mut(&notification.session_id) else {
            tracing::debug!(session_id = ?notification.session_id, "no route for session update");
            return None;
        };
        let events = route.translate(&notification.update);
        Some((route.sink.clone(), events))
    }
}

impl Route {
    fn translate(&mut self, update: &SessionUpdate) -> Vec<ToolEvent> {
        match update {
            SessionUpdate::ToolCall(call) => {
                let tracked = Tracked {
                    tool: Tool::new(call.title.as_str()),
                    input: None,
                };
                self.observe(
                    call.tool_call_id.clone(),
                    tracked,
                    call.raw_input.as_ref(),
                    Some(call.status),
                )
            }
            SessionUpdate::ToolCallUpdate(update) => {
                let Some(tracked) = self.tools.remove(&update.tool_call_id) else {
                    tracing::debug!(tool_call_id = ?update.tool_call_id, "update for unknown tool call");
                    return Vec::new();
                };
                self.observe(
                    update.tool_call_id.clone(),
                    tracked,
                    update.fields.raw_input.as_ref(),
                    update.fields.status,
                )
            }
            _ => Vec::new(),
        }
    }

    fn observe(
        &mut self,
        id: ToolCallId,
        mut tracked: Tracked,
        raw_input: Option<&Value>,
        status: Option<ToolCallStatus>,
    ) -> Vec<ToolEvent> {
        let mut events = Vec::new();
        if let Some(input) = raw_input
            && tracked.input.as_ref() != Some(input)
        {
            tracked.input = Some(input.clone());
            events.push(ToolEvent::Call(ToolCall::new(
                tracked.tool.clone(),
                input.clone(),
            )));
        }
        match status {
            Some(ToolCallStatus::Completed) => {
                events.push(ToolEvent::Result(ToolResult::success(tracked.tool)));
            }
            Some(ToolCallStatus::Failed) => {
                events.push(ToolEvent::Result(ToolResult::failure(tracked.tool)));
            }
            _ => {
                self.tools.insert(id, tracked);
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        ToolCall as AcpToolCall, ToolCallUpdate, ToolCallUpdateFields,
    };
    use serde_json::json;

    fn notification(session: &str, update: SessionUpdate) -> SessionNotification {
        SessionNotification::new(SessionId::new(session), update)
    }

    fn started(session: &str, id: &str, title: &str, input: Value) -> SessionNotification {
        notification(
            session,
            SessionUpdate::ToolCall(
                AcpToolCall::new(ToolCallId::new(id), title)
                    .status(ToolCallStatus::InProgress)
                    .raw_input(input),
            ),
        )
    }

    fn finished(session: &str, id: &str, status: ToolCallStatus) -> SessionNotification {
        notification(
            session,
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new(id),
                ToolCallUpdateFields::new().status(status),
            )),
        )
    }

    fn routed(session: &str) -> (Router, mpsc::Receiver<ToolEvent>) {
        let router = Router::default();
        let (sink, events) = mpsc::channel(8);
        router.register(SessionId::new(session), sink);
        (router, events)
    }

    #[tokio::test]
    async fn call_with_input_then_completion_yields_call_and_successful_result() {
        let (router, mut events) = routed("s");

        router
            .dispatch(&started("s", "1", "git_commit", json!({"tldr": "x"})))
            .await;
        router
            .dispatch(&finished("s", "1", ToolCallStatus::Completed))
            .await;

        assert_eq!(
            events.recv().await,
            Some(ToolEvent::Call(ToolCall::new(
                Tool::new("git_commit"),
                json!({"tldr": "x"})
            )))
        );
        assert_eq!(
            events.recv().await,
            Some(ToolEvent::Result(ToolResult::success(Tool::new(
                "git_commit"
            ))))
        );
    }

    #[tokio::test]
    async fn failed_status_yields_failed_result() {
        let (router, mut events) = routed("s");

        router
            .dispatch(&started("s", "1", "git_commit", json!({})))
            .await;
        router
            .dispatch(&finished("s", "1", ToolCallStatus::Failed))
            .await;

        events.recv().await;
        assert_eq!(
            events.recv().await,
            Some(ToolEvent::Result(ToolResult::failure(Tool::new(
                "git_commit"
            ))))
        );
    }

    #[tokio::test]
    async fn update_carrying_input_yields_call_once_per_distinct_input() {
        let (router, mut events) = routed("s");
        let with_input = |input: Value| {
            notification(
                "s",
                SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    ToolCallId::new("1"),
                    ToolCallUpdateFields::new().raw_input(input),
                )),
            )
        };

        router
            .dispatch(&started("s", "1", "git_commit", json!({})))
            .await;
        router.dispatch(&with_input(json!({}))).await;
        router.dispatch(&with_input(json!({"tldr": "x"}))).await;

        let first = events.recv().await;
        let second = events.recv().await;
        assert_eq!(
            [first, second],
            [
                Some(ToolEvent::Call(ToolCall::new(
                    Tool::new("git_commit"),
                    json!({})
                ))),
                Some(ToolEvent::Call(ToolCall::new(
                    Tool::new("git_commit"),
                    json!({"tldr": "x"})
                ))),
            ]
        );
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn update_for_unknown_tool_call_is_dropped() {
        let (router, mut events) = routed("s");

        router
            .dispatch(&finished("s", "unknown", ToolCallStatus::Completed))
            .await;

        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn events_reach_only_the_owning_session() {
        let router = Router::default();
        let (alpha_sink, mut alpha) = mpsc::channel(8);
        let (beta_sink, mut beta) = mpsc::channel(8);
        router.register(SessionId::new("alpha"), alpha_sink);
        router.register(SessionId::new("beta"), beta_sink);

        router
            .dispatch(&started("alpha", "1", "git_status", json!({})))
            .await;

        assert!(alpha.try_recv().is_ok());
        assert!(beta.try_recv().is_err());
    }

    #[tokio::test]
    async fn removed_session_receives_nothing() {
        let (router, mut events) = routed("s");
        router.remove(&SessionId::new("s"));

        router
            .dispatch(&started("s", "1", "git_status", json!({})))
            .await;

        assert!(events.try_recv().is_err());
    }
}
