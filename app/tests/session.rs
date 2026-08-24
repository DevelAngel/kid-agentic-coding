//! Integration test: runs `start_interactive_session` against the real
//! `lorem-agent` binary over a subprocess ACP connection, verifying that
//! thought and tool-call notifications reach the event channel.

use agent_client_protocol::AcpAgent;
use kid_agentic_coding::{SessionEvent, start_interactive_session};

#[tokio::test]
async fn interactive_session_relays_thought_and_tool_call_events() {
    let agent = AcpAgent::from_args(&[
        "cargo".to_owned(),
        "run".to_owned(),
        "--quiet".to_owned(),
        "-p".to_owned(),
        "lorem-agent".to_owned(),
    ])
    .expect("valid agent command");

    let mut session = start_interactive_session(agent);
    session
        .send_prompt("Hello, agent!")
        .expect("session accepts the prompt");

    let mut saw_thought = false;
    let mut saw_tool_call = false;
    let mut saw_tool_call_update = false;
    let mut saw_stopped = false;

    while !saw_stopped {
        let event = session
            .recv_event()
            .await
            .expect("session stays open until it stops");

        match event {
            SessionEvent::Thought(_) => saw_thought = true,
            SessionEvent::ToolCall { .. } => saw_tool_call = true,
            SessionEvent::ToolCallUpdate { .. } => saw_tool_call_update = true,
            SessionEvent::Stopped(_) => saw_stopped = true,
            SessionEvent::Chunk(_) | SessionEvent::PermissionRequest { .. } => {}
        }
    }

    assert!(saw_thought, "expected a Thought event");
    assert!(saw_tool_call, "expected a ToolCall event");
    assert!(saw_tool_call_update, "expected a ToolCallUpdate event");
}
