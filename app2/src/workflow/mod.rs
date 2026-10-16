mod definition;
mod kind;
mod runner;
mod state;
pub use definition::{WorkflowCompletionReason, WorkflowDefinition, WorkflowPrompt};
pub use kind::{
    CommitFix, EntryError, InitialWorkflow, Planning, Programming, WorkflowPromptProvider,
};
pub(crate) use runner::Runner;
pub use state::{Completed, Defined, Running, Started};

#[cfg_attr(test, derive(Debug))]
pub struct Workflow<K, S> {
    definition: WorkflowDefinition,
    state: S,
    kind: K,
}

impl<K, S> Workflow<K, S> {
    pub fn definition(&self) -> &WorkflowDefinition {
        &self.definition
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Tool, ToolCall, ToolResult};
    use serde_json::json;
    use std::assert_matches;

    fn commit_tool() -> Tool {
        Tool::new("git_commit")
    }

    fn valid_input() -> serde_json::Value {
        json!({ "tldr": "Fix parser", "why": "It panics", "what": "Return an error" })
    }

    fn entry_call(input: serde_json::Value) -> ToolCall {
        ToolCall::new(Tool::new("git_commit_with_fix"), input)
    }

    fn commit_fix_workflow() -> Workflow<CommitFix, Running> {
        Workflow::<Programming, Defined>::initial()
            .start()
            .run()
            .enter(&entry_call(valid_input()))
            .expect("valid entry parameters")
            .run()
    }

    #[test]
    fn initial_workflow_uses_initial_entry_state() {
        let workflow = Workflow::<Programming, Defined>::initial();
        let _: Workflow<Programming, Started> = workflow.start();
    }

    #[test]
    fn programming_combines_workflow_context_with_first_prompt() {
        let workflow = Workflow::<Programming, Defined>::initial().start();

        assert_eq!(
            workflow.prompt("Fix the parser".to_owned()).as_str(),
            "Handle programming tasks within the Programming workflow.\n\nFix the parser"
        );
    }

    #[test]
    fn programming_sends_user_prompt_without_workflow_context_after_startup() {
        let workflow = Workflow::<Programming, Defined>::initial().start().run();

        assert_eq!(
            workflow.prompt("Fix the parser".to_owned()).as_str(),
            "Fix the parser"
        );
    }

    #[test]
    fn programming_enters_commit_fix_through_the_entry_tool() {
        let programming = Workflow::<Programming, Defined>::initial().start().run();

        let entered = programming.enter(&entry_call(valid_input()));

        assert_matches!(entered, Ok(_));
    }

    #[test]
    fn other_tools_do_not_enter_commit_fix() {
        let programming = Workflow::<Programming, Defined>::initial().start().run();
        let call = ToolCall::new(Tool::new("git_status"), valid_input());

        assert_matches!(programming.enter(&call), Err((_, EntryError::NotEntryTool)));
    }

    #[test]
    fn blank_parameters_do_not_enter_commit_fix() {
        let programming = Workflow::<Programming, Defined>::initial().start().run();
        let input = json!({ "tldr": "  ", "why": "why", "what": "what" });

        assert_matches!(
            programming.enter(&entry_call(input)),
            Err((_, EntryError::InvalidParameters(_)))
        );
    }

    #[test]
    fn successful_completion_transitions_to_completed() {
        let workflow = commit_fix_workflow();

        let completed = match workflow.try_complete(&ToolResult::success(commit_tool())) {
            Ok(completed) => completed,
            Err(_) => panic!("successful completion must transition to completed"),
        };

        assert_eq!(
            completed.completion_reason(),
            &WorkflowCompletionReason::ToolSucceeded {
                tool: commit_tool().into(),
            }
        );
    }

    #[test]
    fn failed_completion_keeps_workflow_running() {
        let workflow = commit_fix_workflow();

        assert_matches!(
            workflow.try_complete(&ToolResult::failure(commit_tool())),
            Err(_)
        );
    }

    #[test]
    fn unrelated_success_keeps_workflow_running() {
        let workflow = commit_fix_workflow();

        assert_matches!(
            workflow.try_complete(&ToolResult::success(Tool::new("git_status"))),
            Err(_)
        );
    }
}
