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
    let mut saw_tool_call_result = false;
    let mut saw_stopped = false;

    while !saw_stopped {
        let event = session
            .recv_event()
            .await
            .expect("session stays open until it stops");

        match event {
            SessionEvent::Thought(_) => saw_thought = true,
            SessionEvent::ToolCall { .. } => saw_tool_call = true,
            SessionEvent::ToolCallUpdate {
                result: Some(_), ..
            } => saw_tool_call_result = true,
            SessionEvent::Stopped(_) => saw_stopped = true,
            SessionEvent::ToolCallUpdate { .. }
            | SessionEvent::Chunk(_)
            | SessionEvent::PermissionRequest { .. }
            | SessionEvent::Error(_) => {}
        }
    }

    assert!(saw_thought, "expected a Thought event");
    assert!(saw_tool_call, "expected a ToolCall event");
    assert!(
        saw_tool_call_result,
        "expected a ToolCallUpdate event carrying a result"
    );
}

#[tokio::test]
async fn interactive_session_reports_lorem_agent_failure_mode() {
    let agent = AcpAgent::from_args(&[
        "cargo".to_owned(),
        "run".to_owned(),
        "--quiet".to_owned(),
        "-p".to_owned(),
        "lorem-agent".to_owned(),
        "--".to_owned(),
        "--fail-session".to_owned(),
    ])
    .expect("valid agent command");

    let mut session = start_interactive_session(agent);
    session
        .send_prompt("Hello, agent!")
        .expect("session accepts the prompt");

    let mut saw_error = false;
    while !saw_error {
        if let SessionEvent::Error(_) = session
            .recv_event()
            .await
            .expect("session stays open until the failure is reported")
        {
            saw_error = true;
        }
    }
}

#[tokio::test]
async fn interactive_session_survives_lorem_agent_crash() {
    let agent = AcpAgent::from_args(&[
        "cargo".to_owned(),
        "run".to_owned(),
        "--quiet".to_owned(),
        "-p".to_owned(),
        "lorem-agent".to_owned(),
        "--".to_owned(),
        "--crash".to_owned(),
    ])
    .expect("valid agent command");

    let mut session = start_interactive_session(agent);
    session
        .send_prompt("Hello, agent!")
        .expect("session accepts the prompt");

    let event = session
        .recv_event()
        .await
        .expect("session reports the agent crash");
    assert!(matches!(event, SessionEvent::Error(_)));

    assert!(session.send_prompt("Still here?").is_err());
}
