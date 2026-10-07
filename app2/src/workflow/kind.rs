use super::{Defined, Running, Started, Workflow, WorkflowDefinition, WorkflowPrompt};
use crate::{Tool, ToolSet};

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

pub trait WorkflowPromptProvider {
    fn prompt(&self, prompt: String) -> WorkflowPrompt;
}

pub(super) trait TransientWorkflow {
    fn transient() -> Self;
}

impl WorkflowPromptProvider for Workflow<Programming, Started> {
    fn prompt(&self, prompt: String) -> WorkflowPrompt {
        WorkflowPrompt::new(format!(
            "Handle programming tasks within the Programming workflow.\n\n{prompt}"
        ))
    }
}

impl WorkflowPromptProvider for Workflow<Programming, Running> {
    fn prompt(&self, prompt: String) -> WorkflowPrompt {
        WorkflowPrompt::new(prompt)
    }
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
        let status = Tool::new("git_status");
        let diff = Tool::new("git_diff");
        let commit = Tool::new("git_commit");

        let tools = ToolSet::default()
            .with(status)
            .with(diff)
            .with(commit.clone());

        Self::from_definition(
            WorkflowDefinition::default()
                .with_tools(tools)
                .completes_on_successful_tool(commit),
        )
    }
}
