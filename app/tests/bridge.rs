//! Contract tests for `SessionHandle` identity, used for lifecycle logging.

use kid_agentic_coding::SessionHandle;

#[test]
fn disconnected_test_handle_exposes_a_session_id() {
    let session = SessionHandle::new_disconnected_for_test();

    assert!(!session.session_id().is_empty());
}

#[test]
fn main_session_has_no_workflow_name() {
    let session = SessionHandle::new_disconnected_for_test();

    assert_eq!(session.workflow_name(), None);
}
