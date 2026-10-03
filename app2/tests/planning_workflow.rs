use kid_agentic_coding_app2::{
    ApplicationEntry, Defined, Planning, Running, Tool, ToolResult, Workflow,
};

#[test]
fn planning_workflow_is_initial_and_has_no_tools() {
    let workflow: Workflow<Planning, Defined> = ApplicationEntry::<Planning>::default().workflow();
    assert!(workflow.definition().tools().is_empty());

    let workflow: Workflow<Planning, Running> = workflow.start();
    let result = workflow.try_complete(&ToolResult::success(Tool::new("git_commit")));
    assert!(result.is_err());
}
