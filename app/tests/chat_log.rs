//! Contract tests for `ChatLog`.

use kid_agentic_coding::{ChatLog, Message, Status};

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
            Message::Thought(_) | Message::ToolCluster(_) => "",
        })
        .collect();

    assert_eq!(texts, vec!["one", "two", "three"]);
}

#[test]
fn push_thought_appends_thought_message() {
    let mut log = ChatLog::new();
    log.push_thought("checking existing error handling");

    assert_eq!(log.len(), 1);
    assert!(matches!(log.messages()[0], Message::Thought(ref t) if t == "checking existing error handling"));
}

#[test]
fn consecutive_tool_calls_join_one_cluster() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");
    log.push_tool_call("git_switch_branch");

    assert_eq!(log.len(), 1);
    let Message::ToolCluster(cluster) = &log.messages()[0] else {
        panic!("expected a tool cluster");
    };
    assert_eq!(cluster.entries().len(), 2);
    assert_eq!(cluster.entries()[0].name, "git_status");
    assert_eq!(cluster.entries()[1].name, "git_switch_branch");
}

#[test]
fn thought_between_tool_calls_starts_a_new_cluster() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");
    log.push_thought("wrong branch, switching back to main");
    log.push_tool_call("git_switch_branch");

    assert_eq!(log.len(), 3);
    assert!(matches!(log.messages()[0], Message::ToolCluster(_)));
    assert!(matches!(log.messages()[1], Message::Thought(_)));
    assert!(matches!(log.messages()[2], Message::ToolCluster(_)));
}

#[test]
fn update_tool_call_status_changes_the_matching_entry() {
    let mut log = ChatLog::new();
    let id = log.push_tool_call("git_status");

    log.update_tool_call_status(id, Status::Done);

    let Message::ToolCluster(cluster) = &log.messages()[0] else {
        panic!("expected a tool cluster");
    };
    assert_eq!(cluster.entries()[0].status, Status::Done);
}

#[test]
fn new_cluster_starts_collapsed() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");

    let Message::ToolCluster(cluster) = &log.messages()[0] else {
        panic!("expected a tool cluster");
    };
    assert!(!cluster.expanded());
}

#[test]
fn toggle_cluster_flips_expanded_state() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");

    log.toggle_cluster(0);
    let Message::ToolCluster(cluster) = &log.messages()[0] else {
        panic!("expected a tool cluster");
    };
    assert!(cluster.expanded());

    log.toggle_cluster(0);
    let Message::ToolCluster(cluster) = &log.messages()[0] else {
        panic!("expected a tool cluster");
    };
    assert!(!cluster.expanded());
}

