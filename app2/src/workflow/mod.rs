mod definition;
mod kind;
mod state;

pub use definition::{WorkflowCompletionReason, WorkflowDefinition};
use kind::TransientWorkflow;
pub use kind::{CommitFix, InitialWorkflow, Planning, Programming};
pub use state::{Completed, Defined, Running};

use std::marker::PhantomData;

#[cfg_attr(test, derive(Debug))]
pub struct Workflow<K, S> {
    definition: WorkflowDefinition,
    state: S,
    kind: PhantomData<K>,
}

impl<K, S> Workflow<K, S> {
    pub fn definition(&self) -> &WorkflowDefinition {
        &self.definition
    }
}

impl Workflow<Programming, Running> {
    pub fn commit_fix(self) -> Workflow<CommitFix, Running> {
        let _ = self;
        Workflow::<CommitFix, Defined>::transient().start()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{McpServer, Tool, ToolResult};
    use std::assert_matches;

    fn commit_tool() -> Tool {
        Tool::new("git_commit")
    }

    fn commit_fix_workflow() -> Workflow<CommitFix, Running> {
        Workflow::<CommitFix, Defined>::transient().start()
    }

    #[test]
    fn commit_fix_defines_required_mcp_server() {
        let workflow = commit_fix_workflow();

        assert_eq!(
            workflow.definition().mcp_servers(),
            &[McpServer::new("git-commit-fix-close-tools")]
        );
    }

    #[test]
    fn initial_workflow_uses_initial_entry_state() {
        let workflow = Workflow::<Programming, Defined>::initial();
        let _: Workflow<Programming, Running> = workflow.start();
    }

    #[test]
    fn programming_can_enter_commit_fix() {
        let workflow = Workflow::<Programming, Defined>::initial().start();
        let _: Workflow<CommitFix, Running> = workflow.commit_fix();
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
                tool: commit_tool(),
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
