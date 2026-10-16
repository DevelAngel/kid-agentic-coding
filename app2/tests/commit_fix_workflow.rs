use std::fmt;

use cucumber::{World as _, given, then, when};
use kid_agentic_coding_app2::{
    ApplicationEntry, CommitFix, Completed, CompletionTool, EntryError, Programming, Running,
    Started, Tool, ToolCall, ToolResult, Workflow, WorkflowCompletionReason,
    WorkflowPromptProvider,
};
use serde_json::{Value, json};

#[derive(Default, cucumber::World)]
struct World {
    programming: Option<Workflow<Programming, Running>>,
    started: Option<Workflow<CommitFix, Started>>,
    workflow: Option<Workflow<CommitFix, Running>>,
    error: Option<EntryError>,
    completed: Option<Workflow<CommitFix, Completed>>,
}

impl fmt::Debug for World {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("World")
            .field("programming_running", &self.programming.is_some())
            .field("commit_fix_started", &self.started.is_some())
            .field("workflow_running", &self.workflow.is_some())
            .field("workflow_completed", &self.completed.is_some())
            .finish()
    }
}

fn programming() -> Workflow<Programming, Running> {
    ApplicationEntry::<Programming>::default()
        .workflow()
        .start()
        .run()
}

fn entry_call(input: Value) -> ToolCall {
    ToolCall::new(Tool::new("git_commit_with_fix"), input)
}

fn valid_input() -> Value {
    json!({
        "tldr": "Fix parser",
        "why": "It panics on empty input",
        "what": "Return an error instead of panicking",
    })
}

fn invoke_entry(world: &mut World, input: Value) {
    let programming = world.programming.take().expect("programming is running");
    match programming.enter(&entry_call(input)) {
        Ok(started) => world.started = Some(started),
        Err((programming, error)) => {
            world.programming = Some(programming);
            world.error = Some(error);
        }
    }
}

#[given("a running programming workflow")]
async fn running_programming_workflow(world: &mut World) {
    world.programming = Some(programming());
}

#[given("a running commit-fix workflow")]
async fn running_commit_fix_workflow(world: &mut World) {
    let Ok(started) = programming().enter(&entry_call(valid_input())) else {
        panic!("entry parameters are valid");
    };
    world.workflow = Some(started.run());
}

#[when("the agent invokes the commit-fix entry tool with tldr, why, and what")]
async fn invoke_entry_tool(world: &mut World) {
    invoke_entry(world, valid_input());
}

#[when("the agent invokes the commit-fix entry tool without why")]
async fn invoke_entry_tool_without_why(world: &mut World) {
    invoke_entry(
        world,
        json!({ "tldr": "Fix parser", "what": "Return an error" }),
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
        Ok(completed) => world.completed = Some(completed),
        Err(workflow) => world.workflow = Some(workflow),
    }
}

#[then("a commit-fix workflow starts")]
async fn commit_fix_starts(world: &mut World) {
    assert!(world.started.is_some());
}

#[then("its prompt contains tldr, why, and what")]
async fn prompt_contains_parameters(world: &mut World) {
    let started = world.started.as_ref().expect("commit-fix started");
    let prompt = started.initial_prompt();
    for expected in [
        "Fix parser",
        "It panics on empty input",
        "Return an error instead of panicking",
    ] {
        assert!(prompt.as_str().contains(expected), "missing: {expected}");
    }
}

#[then("the commit-fix definition declares its identity, tool names, and tools")]
async fn definition_declares_contract(world: &mut World) {
    let started = world.started.as_ref().expect("commit-fix started");
    let definition = started.definition();

    assert_eq!(definition.name(), "commit-fix");
    assert_eq!(
        definition.entry_tool().map(Tool::name),
        Some("git_commit_with_fix")
    );
    assert_eq!(
        definition.exit_tool().map(CompletionTool::name),
        Some("git_commit")
    );
    for tool in ["git_status", "git_diff", "git_commit"] {
        assert!(definition.tools().contains(tool), "missing: {tool}");
    }
}

#[then("no transition happens")]
async fn no_transition(world: &mut World) {
    assert!(world.started.is_none());
    assert!(world.programming.is_some());
}

#[then("the failure is reported")]
async fn failure_is_reported(world: &mut World) {
    assert!(matches!(
        world.error,
        Some(EntryError::InvalidParameters(_))
    ));
}

#[then("the workflow is complete")]
async fn workflow_is_complete(world: &mut World) {
    assert!(world.completed.is_some());
    assert!(world.workflow.is_none());
}

#[then("the workflow remains active")]
async fn workflow_remains_active(world: &mut World) {
    assert!(world.completed.is_none());
    assert!(world.workflow.is_some());
}

#[then("the caller resumes with the result")]
async fn caller_resumes(world: &mut World) {
    let completed = world.completed.take().expect("workflow is complete");
    let (caller, result) = completed.resume();

    assert_eq!(
        result,
        WorkflowCompletionReason::ToolSucceeded {
            tool: Tool::new("git_commit").into(),
        }
    );
    assert_eq!(caller.prompt("next".to_owned()).as_str(), "next");
}

#[tokio::test]
async fn commit_fix_workflow() {
    World::cucumber().with_default_cli().run("tests").await;
}
