use std::fmt;

use cucumber::{World as _, given, then, when};
use kid_agentic_coding_app2::{ApplicationEntry, Programming, Tool, ToolResult, Workflow};

#[derive(Default, cucumber::World)]
struct World {
    workflow:
        Option<Workflow<kid_agentic_coding_app2::CommitFix, kid_agentic_coding_app2::Running>>,
    completed: bool,
}

impl fmt::Debug for World {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("World")
            .field("workflow_running", &self.workflow.is_some())
            .field("workflow_completed", &self.completed)
            .finish()
    }
}

#[given("a running commit-fix workflow")]
async fn running_commit_fix_workflow(world: &mut World) {
    world.workflow = Some(
        ApplicationEntry::<Programming>::default()
            .workflow()
            .start()
            .commit_fix(),
    );
}

#[when(regex = r"^git (status|diff|commit) (succeeds|fails)$")]
async fn run_git_tool(world: &mut World, tool: String, outcome: String) {
    let workflow = world.workflow.take().expect("workflow is running");
    let result = match outcome.as_str() {
        "succeeds" => ToolResult::success(Tool::new(format!("git_{tool}"))),
        "fails" => ToolResult::failure(Tool::new(format!("git_{tool}"))),
        _ => unreachable!("the feature file only provides valid outcomes"),
    };

    match workflow.try_complete(&result) {
        Ok(_) => world.completed = true,
        Err(workflow) => world.workflow = Some(workflow),
    }
}

#[then("the workflow is complete")]
async fn workflow_is_complete(world: &mut World) {
    assert!(world.completed);
    assert!(world.workflow.is_none());
}

#[then("the workflow remains active")]
async fn workflow_remains_active(world: &mut World) {
    assert!(!world.completed);
    assert!(world.workflow.is_some());
}

#[tokio::test]
async fn commit_fix_workflow() {
    World::cucumber().with_default_cli().run("tests").await;
}
