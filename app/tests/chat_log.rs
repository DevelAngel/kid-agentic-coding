//! Contract tests for `ChatLog`.

use kid_agentic_coding::{ChatLog, Message};

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
        })
        .collect();

    assert_eq!(texts, vec!["one", "two", "three"]);
}
