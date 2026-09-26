use kid_agentic_coding_app2::{
    CompletionReason, Tool, ToolResult, ToolSet, WorkflowAction, WorkflowDefinition,
    WorkflowRuntime,
};

fn commit_fix_workflow() -> WorkflowRuntime {
    let status = Tool::new("git_status");
    let diff = Tool::new("git_diff");
    let commit = Tool::new("git_commit");

    let tools = ToolSet::default()
        .with(status)
        .with(diff)
        .with(commit.clone());

    let definition = WorkflowDefinition::new("commit-fix")
        .with_tools(tools)
        .completes_on_successful_tool(commit);

    WorkflowRuntime::start(definition)
}

#[test]
fn commit_fix_completes_after_successful_commit() {
    // Given a running commit-fix workflow.
    let mut workflow = commit_fix_workflow();

    // When the workflow completes its preparation steps.
    assert_eq!(
        workflow.handle_tool_result(&ToolResult::success(Tool::new("git_status"))),
        WorkflowAction::Continue
    );
    assert_eq!(
        workflow.handle_tool_result(&ToolResult::success(Tool::new("git_diff"))),
        WorkflowAction::Continue
    );

    // And the commit succeeds.
    let action = workflow.handle_tool_result(&ToolResult::success(Tool::new("git_commit")));

    // Then the workflow completes.
    assert_eq!(
        action,
        WorkflowAction::Complete(CompletionReason::ToolSucceeded {
            tool: Tool::new("git_commit").into(),
        })
    );
    assert!(!workflow.is_running());
}

#[test]
fn commit_fix_stays_active_after_failed_commit() {
    // Given a running commit-fix workflow.
    let mut workflow = commit_fix_workflow();

    // When the workflow completes its preparation steps.
    assert_eq!(
        workflow.handle_tool_result(&ToolResult::success(Tool::new("git_status"))),
        WorkflowAction::Continue
    );
    assert_eq!(
        workflow.handle_tool_result(&ToolResult::success(Tool::new("git_diff"))),
        WorkflowAction::Continue
    );

    // And the commit fails.
    let action = workflow.handle_tool_result(&ToolResult::failure(Tool::new("git_commit")));

    // Then the workflow remains active.
    assert_eq!(action, WorkflowAction::Continue);
    assert!(workflow.is_running());
}
