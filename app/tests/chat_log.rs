//! Contract tests for `ChatLog`.

use kid_agentic_coding::{ChatLog, Message, SessionNoticeKind, Status, Step};

#[test]
fn new_log_is_empty() {
    let log = ChatLog::new();
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);
}

#[test]
fn push_user_appends_user_message() {
    let mut log = ChatLog::new();
    log.push_user("hello");

    assert_eq!(log.len(), 1);
    assert!(matches!(log.messages()[0], Message::User(ref m) if m.text == "hello"));
}

#[test]
fn push_agent_appends_agent_message() {
    let mut log = ChatLog::new();
    log.push_agent("hi there");

    assert_eq!(log.len(), 1);
    assert!(matches!(log.messages()[0], Message::Agent(ref m) if m.text == "hi there"));
}

#[test]
fn push_session_notice_appends_outcome() {
    let mut log = ChatLog::new();
    log.push_session_notice(SessionNoticeKind::Error, "Session failed: connection lost");

    assert!(matches!(
        &log.messages()[0],
        Message::SessionNotice(notice)
            if notice.kind == SessionNoticeKind::Error
                && notice.text == "Session failed: connection lost"
    ));
}


#[test]
fn messages_preserve_insertion_order() {
    let mut log = ChatLog::new();
    log.push_user("one");
    log.push_agent("two");
    log.push_user("three");

    let texts: Vec<&str> = log
        .messages()
        .iter()
        .map(|m| match m {
            Message::User(u) => u.text.as_str(),
            Message::Agent(a) => a.text.as_str(),
            Message::ToolCluster(_) | Message::SessionNotice(_) => "",
        })
        .collect();

    assert_eq!(texts, vec!["one", "two", "three"]);
}

fn tool_cluster(log: &ChatLog, message_index: usize) -> &kid_agentic_coding::ToolCluster {
    let Message::ToolCluster(cluster) = &log.messages()[message_index] else {
        panic!("expected a tool cluster at index {message_index}");
    };
    cluster
}

#[test]
fn push_thought_starts_a_tool_cluster() {
    let mut log = ChatLog::new();
    log.push_thought("checking existing error handling");

    assert_eq!(log.len(), 1);
    let cluster = tool_cluster(&log, 0);
    assert_eq!(cluster.steps().len(), 1);
    assert!(matches!(
        cluster.steps()[0],
        Step::Thought(ref t) if t == "checking existing error handling"
    ));
}

#[test]
fn consecutive_tool_calls_join_one_cluster() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");
    log.push_tool_call("git_switch_branch");

    assert_eq!(log.len(), 1);
    let cluster = tool_cluster(&log, 0);
    assert_eq!(cluster.tool_call_count(), 2);
    let Step::ToolCall(first) = &cluster.steps()[0] else {
        panic!("expected a tool call");
    };
    let Step::ToolCall(second) = &cluster.steps()[1] else {
        panic!("expected a tool call");
    };
    assert_eq!(first.name, "git_status");
    assert_eq!(second.name, "git_switch_branch");
}

#[test]
fn thought_between_tool_calls_stays_in_the_same_cluster() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");
    log.push_thought("wrong branch, switching back to main");
    log.push_tool_call("git_switch_branch");

    assert_eq!(log.len(), 1);
    let cluster = tool_cluster(&log, 0);
    assert_eq!(cluster.steps().len(), 3);
    assert!(matches!(cluster.steps()[0], Step::ToolCall(_)));
    assert!(matches!(cluster.steps()[1], Step::Thought(_)));
    assert!(matches!(cluster.steps()[2], Step::ToolCall(_)));
}

#[test]
fn agent_message_ends_the_open_cluster() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");
    log.push_agent("done checking");
    log.push_tool_call("git_pull");

    assert_eq!(log.len(), 3);
    assert!(matches!(log.messages()[0], Message::ToolCluster(_)));
    assert!(matches!(log.messages()[1], Message::Agent(_)));
    assert!(matches!(log.messages()[2], Message::ToolCluster(_)));
}

#[test]
fn user_message_ends_the_open_cluster() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");
    log.push_user("try again");
    log.push_tool_call("git_pull");

    assert_eq!(log.len(), 3);
    assert!(matches!(log.messages()[0], Message::ToolCluster(_)));
    assert!(matches!(log.messages()[1], Message::User(_)));
    assert!(matches!(log.messages()[2], Message::ToolCluster(_)));
}

#[test]
fn update_tool_call_status_changes_the_matching_step() {
    let mut log = ChatLog::new();
    let id = log.push_tool_call("git_status");

    log.update_tool_call_status(id, Status::Done);

    let Step::ToolCall(entry) = &tool_cluster(&log, 0).steps()[0] else {
        panic!("expected a tool call");
    };
    assert_eq!(entry.status, Status::Done);
}

#[test]
fn update_tool_call_status_does_not_touch_a_thought_at_the_same_index() {
    let mut log = ChatLog::new();
    log.push_thought("thinking");
    let id = log.push_tool_call("git_status");

    log.update_tool_call_status(id, Status::Done);

    assert!(matches!(tool_cluster(&log, 0).steps()[0], Step::Thought(_)));
    let Step::ToolCall(entry) = &tool_cluster(&log, 0).steps()[1] else {
        panic!("expected a tool call");
    };
    assert_eq!(entry.status, Status::Done);
}

#[test]
fn new_cluster_starts_collapsed() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");

    assert!(!tool_cluster(&log, 0).expanded());
}

#[test]
fn toggle_cluster_flips_expanded_state() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");

    log.toggle_cluster(0);
    assert!(tool_cluster(&log, 0).expanded());

    log.toggle_cluster(0);
    assert!(!tool_cluster(&log, 0).expanded());
}

#[test]
fn status_is_running_if_any_tool_call_is_running() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");
    let running_id = log.push_tool_call("git_pull");
    log.update_tool_call_status(running_id, Status::Running);

    assert_eq!(tool_cluster(&log, 0).status(), Status::Running);
}

#[test]
fn status_is_done_once_every_tool_call_is_done() {
    let mut log = ChatLog::new();
    let a = log.push_tool_call("git_status");
    let b = log.push_tool_call("git_pull");
    log.update_tool_call_status(a, Status::Done);
    log.update_tool_call_status(b, Status::Done);

    assert_eq!(tool_cluster(&log, 0).status(), Status::Done);
}

#[test]
fn status_of_a_thoughts_only_cluster_is_done() {
    let mut log = ChatLog::new();
    log.push_thought("just thinking, no tools needed");

    assert_eq!(tool_cluster(&log, 0).status(), Status::Done);
}

#[test]
fn visible_steps_are_empty_for_a_settled_collapsed_cluster() {
    let mut log = ChatLog::new();
    let id = log.push_tool_call("git_status");
    log.update_tool_call_status(id, Status::Done);

    assert!(tool_cluster(&log, 0).visible_steps().is_empty());
    assert_eq!(tool_cluster(&log, 0).visible_row_count(), 1);
}

#[test]
fn visible_steps_show_a_live_tail_while_still_running() {
    let mut log = ChatLog::new();
    log.push_tool_call("a");
    log.push_tool_call("b");
    log.push_tool_call("c");
    let running = log.push_tool_call("d");
    log.update_tool_call_status(running, Status::Running);

    let cluster = tool_cluster(&log, 0);
    assert_eq!(cluster.visible_steps().len(), 3);
    let Step::ToolCall(last_shown) = &cluster.visible_steps()[2] else {
        panic!("expected a tool call");
    };
    assert_eq!(last_shown.name, "d");
    // summary + truncation marker + 3 shown steps
    assert_eq!(cluster.visible_row_count(), 5);
}

#[test]
fn visible_steps_show_everything_once_expanded_even_if_settled() {
    let mut log = ChatLog::new();
    let a = log.push_tool_call("a");
    let b = log.push_tool_call("b");
    log.update_tool_call_status(a, Status::Done);
    log.update_tool_call_status(b, Status::Done);
    log.toggle_cluster(0);

    let cluster = tool_cluster(&log, 0);
    assert_eq!(cluster.visible_steps().len(), 2);
    // summary + 2 steps, no truncation marker since nothing is hidden
    assert_eq!(cluster.visible_row_count(), 3);
}
