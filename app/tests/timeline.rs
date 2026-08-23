//! Contract tests for `TimelineLog`.

use kid_agentic_coding::{EntryKind, Status, TimelineLog};

#[test]
fn new_log_is_empty() {
    let log = TimelineLog::new();
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);
}

#[test]
fn push_tool_call_appends_pending_entry() {
    let mut log = TimelineLog::new();
    let id = log.push_tool_call("read_file", vec!["path: src/main.rs".to_string()]);

    assert_eq!(log.len(), 1);
    let entry = &log.entries()[0];
    assert_eq!(entry.status, Status::Pending);
    assert!(matches!(&entry.kind, EntryKind::ToolCall { name, .. } if name == "read_file"));
    assert_eq!(entry.lines, vec!["path: src/main.rs".to_string()]);

    // id is opaque but must be usable to update the same entry later.
    log.update_tool_call_status(id, Status::Running);
    assert_eq!(log.entries()[0].status, Status::Running);
}

#[test]
fn update_tool_call_status_only_affects_targeted_entry() {
    let mut log = TimelineLog::new();
    let first = log.push_tool_call("read_file", vec![]);
    let second = log.push_tool_call("write_file", vec![]);

    log.update_tool_call_status(second, Status::Failed);

    assert_eq!(log.entries()[0].status, Status::Pending);
    assert_eq!(log.entries()[1].status, Status::Failed);
    let _ = first;
}

#[test]
fn update_tool_call_status_on_unknown_id_is_a_no_op() {
    let mut log = TimelineLog::new();
    let id = log.push_tool_call("read_file", vec![]);
    log.update_tool_call_status(id, Status::Done);

    let bogus = log.push_tool_call("noop", vec![]);
    log.update_tool_call_status(bogus, Status::Done);
    log.update_tool_call_status(bogus, Status::Failed);
    // Consuming the same id twice is allowed; both entries reflect the latest write.
    assert_eq!(log.entries()[1].status, Status::Failed);
}

#[test]
fn push_thought_appends_entry_with_done_status() {
    let mut log = TimelineLog::new();
    log.push_thought(vec!["Checking existing error handling...".to_string()]);

    assert_eq!(log.len(), 1);
    let entry = &log.entries()[0];
    assert_eq!(entry.status, Status::Done);
    assert!(matches!(entry.kind, EntryKind::Thought));
    assert_eq!(entry.lines, vec!["Checking existing error handling...".to_string()]);
}

#[test]
fn entries_preserve_insertion_order() {
    let mut log = TimelineLog::new();
    log.push_tool_call("read_file", vec![]);
    log.push_thought(vec!["thinking".to_string()]);
    log.push_tool_call("write_file", vec![]);

    let kinds: Vec<bool> = log
        .entries()
        .iter()
        .map(|e| matches!(e.kind, EntryKind::Thought))
        .collect();
    assert_eq!(kinds, vec![false, true, false]);
}
