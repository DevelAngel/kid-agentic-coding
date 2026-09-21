use serde_json::{Value, json};
use std::path::PathBuf;
use wire::{COMMIT_FIX_EVENT, CommitFixRequest, CommitFixVerdict};

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
