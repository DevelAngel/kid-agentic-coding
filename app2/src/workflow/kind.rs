use super::{Defined, Workflow, WorkflowDefinition};
use crate::{McpServer, Tool};

#[derive(Default)]
pub struct Programming;

#[derive(Default)]
pub struct Planning;

#[derive(Default)]
#[cfg_attr(test, derive(Debug))]
pub struct CommitFix;

pub trait InitialWorkflow {
    fn initial() -> Self;
}

pub(super) trait TransientWorkflow {
    fn transient() -> Self;
}

impl InitialWorkflow for Workflow<Programming, Defined> {
    fn initial() -> Self {
        Workflow::from_definition(WorkflowDefinition::default())
    }
}

impl InitialWorkflow for Workflow<Planning, Defined> {
    fn initial() -> Self {
        Workflow::from_definition(WorkflowDefinition::default())
    }
}

impl TransientWorkflow for Workflow<CommitFix, Defined> {
    fn transient() -> Self {
        let commit = Tool::new("git_commit");

        Self::from_definition(
            WorkflowDefinition::default()
                .with_mcp_server(McpServer::new("git-commit-fix-close-tools"))
                .completes_on_successful_tool(commit),
        )
    }
}
