use serde_json::{Value, json};
use std::path::PathBuf;
use wire::{
    CLOSE_ACTION_EVENT, COMMIT_FIX_DONE_EVENT, COMMIT_FIX_EVENT, CloseAction, CloseActionOutcome,
    CloseActionRequest, CloseActionVerdict, CommitFixDone, CommitFixRequest, CommitFixVerdict,
    Confetti, WorkflowEvent,
};

fn request() -> CommitFixRequest {
    CommitFixRequest {
        instructions: "commit it".to_owned(),
        amend: true,
        tldr: "summary".to_owned(),
        why: "motivation".to_owned(),
        what: "change".to_owned(),
        cwd: Some(PathBuf::from("/work/dir")),
    }
}

fn request_value() -> Value {
    json!({
        "event": "commit-fix",
        "instructions": "commit it",
        "amend": true,
        "tldr": "summary",
        "why": "motivation",
        "what": "change",
        "cwd": "/work/dir",
    })
}

#[test]
fn request_is_encoded_as_one_json_line_with_the_event_tag() {
    let line = request().to_line().unwrap();

    assert!(!line.contains('\n'));
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        request_value()
    );
}

#[test]
fn request_has_a_pinned_single_line_wire_format() {
    assert_eq!(
        request().to_line().unwrap(),
        r#"{"event":"commit-fix","instructions":"commit it","amend":true,"tldr":"summary","why":"motivation","what":"change","cwd":"/work/dir"}"#
    );
}

#[test]
fn request_tag_matches_the_event_constant() {
    let line = request().to_line().unwrap();
    let value = serde_json::from_str::<Value>(&line).unwrap();

    assert_eq!(value["event"], COMMIT_FIX_EVENT);
}

#[test]
fn request_roundtrips_through_a_value() {
    let parsed = serde_json::from_value::<CommitFixRequest>(request_value()).unwrap();

    assert_eq!(parsed, request());
}

#[test]
fn request_allows_missing_or_null_cwd() {
    let mut missing = request_value();
    missing.as_object_mut().unwrap().remove("cwd");
    let mut null = request_value();
    null["cwd"] = Value::Null;

    for value in [missing, null] {
        let parsed = serde_json::from_value::<CommitFixRequest>(value).unwrap();
        assert_eq!(parsed.cwd, None);
    }
}

#[test]
fn request_rejects_missing_or_mistyped_fields() {
    let mut cases = Vec::new();
    for field in ["instructions", "amend", "tldr", "why", "what"] {
        let mut missing = request_value();
        missing.as_object_mut().unwrap().remove(field);
        cases.push(missing);
    }
    for (field, wrong) in [
        ("instructions", json!(1)),
        ("amend", json!("yes")),
        ("tldr", json!(null)),
        ("why", json!(false)),
        ("what", json!(["x"])),
        ("cwd", json!(7)),
    ] {
        let mut mistyped = request_value();
        mistyped[field] = wrong;
        cases.push(mistyped);
    }

    for case in cases {
        assert!(
            serde_json::from_value::<CommitFixRequest>(case.clone()).is_err(),
            "accepted invalid request: {case}"
        );
    }
}

#[test]
fn verdicts_have_a_pinned_single_line_wire_format() {
    let cases = [
        (CommitFixVerdict::Opening, r#"{"outcome":"opening"}"#),
        (
            CommitFixVerdict::Ignored {
                reason: "busy".to_owned(),
            },
            r#"{"outcome":"ignored","reason":"busy"}"#,
        ),
        (
            CommitFixVerdict::Rejected {
                reason: "broken".to_owned(),
            },
            r#"{"outcome":"rejected","reason":"broken"}"#,
        ),
    ];

    for (verdict, wire) in cases {
        assert_eq!(verdict.to_line().unwrap(), wire);
        assert_eq!(CommitFixVerdict::from_line(wire).unwrap(), verdict);
    }
}

#[test]
fn malformed_verdicts_fail_to_parse() {
    for line in [
        "",
        "not json",
        r#"{"outcome":"unknown"}"#,
        r#"{"outcome":"ignored"}"#,
        r#"{"reason":"busy"}"#,
    ] {
        assert!(
            CommitFixVerdict::from_line(line).is_err(),
            "accepted invalid verdict: {line}"
        );
    }
}

fn done() -> CommitFixDone {
    CommitFixDone {
        commit_message: "fix: trim\n\nBody.".to_owned(),
    }
}

#[test]
fn done_event_has_a_pinned_single_line_wire_format() {
    assert_eq!(
        done().to_line().unwrap(),
        r#"{"event":"commit-fix-done","commit_message":"fix: trim\n\nBody."}"#
    );
}

#[test]
fn done_event_tag_matches_the_event_constant() {
    let line = done().to_line().unwrap();
    let value = serde_json::from_str::<Value>(&line).unwrap();

    assert_eq!(value["event"], COMMIT_FIX_DONE_EVENT);
}

#[test]
fn confetti_has_a_pinned_wire_format() {
    assert_eq!(Confetti.to_line(), "confetti");
}

#[test]
fn confetti_parses_only_its_exact_message() {
    assert!(Confetti::parse(b"confetti\n").is_some());

    for message in [
        &b""[..],
        b"confetti",
        b"Confetti\n",
        b"confetti\nconfetti\n",
        b"{\"event\":\"confetti\"}\n",
    ] {
        assert!(
            Confetti::parse(message).is_none(),
            "accepted invalid confetti message: {message:?}"
        );
    }
}

#[test]
fn workflow_event_parses_a_commit_fix_request() {
    let line = request().to_line().unwrap();

    assert_eq!(
        WorkflowEvent::parse(line.as_bytes()),
        Ok(WorkflowEvent::CommitFix(request()))
    );
}

#[test]
fn workflow_event_parses_a_commit_fix_done_event() {
    let line = done().to_line().unwrap();

    assert_eq!(
        WorkflowEvent::parse(format!("{line}\n").as_bytes()),
        Ok(WorkflowEvent::CommitFixDone(done()))
    );
}

#[test]
fn workflow_event_accepts_a_done_event_without_a_message() {
    assert_eq!(
        WorkflowEvent::parse(br#"{"event":"commit-fix-done"}"#),
        Ok(WorkflowEvent::CommitFixDone(CommitFixDone {
            commit_message: String::new()
        }))
    );
}

#[test]
fn workflow_event_rejects_invalid_json() {
    for message in [&b""[..], b"not json", b"confetti\n"] {
        let reason = WorkflowEvent::parse(message).unwrap_err();

        assert!(
            reason.starts_with("workflow event is not valid JSON: "),
            "unexpected reason: {reason}"
        );
    }
}

#[test]
fn workflow_event_rejects_unknown_or_untagged_events() {
    let cases = [
        (
            r#"{"event":"unknown"}"#,
            "unhandled workflow event 'unknown'",
        ),
        (r#"{}"#, "unhandled workflow event '<missing>'"),
        (r#"{"event":1}"#, "unhandled workflow event '<missing>'"),
    ];

    for (message, reason) in cases {
        assert_eq!(
            WorkflowEvent::parse(message.as_bytes()),
            Err(reason.to_owned())
        );
    }
}

#[test]
fn workflow_event_rejects_a_malformed_commit_fix_request() {
    assert!(WorkflowEvent::parse(br#"{"event":"commit-fix"}"#).is_err());
}

#[test]
fn close_action_request_has_a_pinned_single_line_wire_format() {
    let cases = [
        (
            CloseActionRequest {
                action: CloseAction::Add,
            },
            r#"{"event":"close-action","action":"add"}"#,
        ),
        (
            CloseActionRequest {
                action: CloseAction::Commit,
            },
            r#"{"event":"close-action","action":"commit"}"#,
        ),
    ];

    for (request, wire) in cases {
        assert_eq!(request.to_line().unwrap(), wire);
        let value = serde_json::from_str::<Value>(wire).unwrap();
        assert_eq!(value["event"], CLOSE_ACTION_EVENT);
    }
}

#[test]
fn close_action_verdicts_have_a_pinned_single_line_wire_format() {
    let cases = [
        (
            CloseActionVerdict::Authorized,
            r#"{"outcome":"authorized"}"#,
        ),
        (
            CloseActionVerdict::Rejected {
                reason: "no fix session is active".to_owned(),
            },
            r#"{"outcome":"rejected","reason":"no fix session is active"}"#,
        ),
    ];

    for (verdict, wire) in cases {
        assert_eq!(verdict.to_line().unwrap(), wire);
        assert_eq!(CloseActionVerdict::from_line(wire).unwrap(), verdict);
    }
}

#[test]
fn malformed_close_action_verdicts_fail_to_parse() {
    for line in [
        "",
        "not json",
        r#"{"outcome":"unknown"}"#,
        r#"{"outcome":"rejected"}"#,
    ] {
        assert!(
            CloseActionVerdict::from_line(line).is_err(),
            "accepted invalid verdict: {line}"
        );
    }
}

#[test]
fn close_action_outcome_has_a_pinned_single_line_wire_format() {
    let outcome = CloseActionOutcome {
        action: CloseAction::Commit,
        success: false,
        reason: Some("git commit failed".to_owned()),
    };

    assert_eq!(
        outcome.to_line().unwrap(),
        r#"{"event":"close-action-outcome","action":"commit","success":false,"reason":"git commit failed"}"#
    );
}

#[test]
fn close_action_outcome_allows_a_missing_reason() {
    let parsed = serde_json::from_value::<CloseActionOutcome>(json!({
        "event": "close-action-outcome",
        "action": "add",
        "success": true,
    }))
    .unwrap();

    assert_eq!(
        parsed,
        CloseActionOutcome {
            action: CloseAction::Add,
            success: true,
            reason: None,
        }
    );
}

#[test]
fn workflow_event_parses_a_close_action_request() {
    let request = CloseActionRequest {
        action: CloseAction::Add,
    };
    let line = request.to_line().unwrap();

    assert_eq!(
        WorkflowEvent::parse(line.as_bytes()),
        Ok(WorkflowEvent::CloseAction(request))
    );
}

#[test]
fn workflow_event_parses_a_close_action_outcome_event() {
    let outcome = CloseActionOutcome {
        action: CloseAction::Commit,
        success: false,
        reason: Some("git commit failed".to_owned()),
    };
    let line = outcome.to_line().unwrap();

    assert_eq!(
        WorkflowEvent::parse(line.as_bytes()),
        Ok(WorkflowEvent::CloseActionOutcome(outcome))
    );
}

#[test]
fn workflow_event_rejects_a_malformed_close_action_request() {
    assert!(WorkflowEvent::parse(br#"{"event":"close-action"}"#).is_err());
}
