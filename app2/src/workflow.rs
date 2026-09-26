use std::marker::PhantomData;

use crate::{Tool, ToolResult, ToolSet, completion_tool::CompletionTool};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowDefinition {
    name: String,
    tools: ToolSet,
    completion_tool: Option<CompletionTool>,
}

impl WorkflowDefinition {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tools: ToolSet::default(),
            completion_tool: None,
        }
    }

    pub fn with_tools(mut self, tools: ToolSet) -> Self {
        self.tools = tools;
        self
    }

    pub fn completes_on_successful_tool(mut self, tool: Tool) -> Self {
        self.completion_tool = Some(tool.into());
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tools(&self) -> &ToolSet {
        &self.tools
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompletionReason {
    ToolSucceeded { tool: CompletionTool },
}

pub struct Defined;
pub struct Running;
pub struct Completed;

pub struct Workflow<S> {
    definition: WorkflowDefinition,
    completion_reason: Option<CompletionReason>,
    state: PhantomData<S>,
}

impl Workflow<Defined> {
    pub fn new(definition: WorkflowDefinition) -> Self {
        Self {
            definition,
            completion_reason: None,
            state: PhantomData,
        }
    }

    pub fn start(self) -> Workflow<Running> {
        Workflow {
            definition: self.definition,
            completion_reason: None,
            state: PhantomData,
        }
    }
}

impl Workflow<Running> {
    pub fn handle_tool_result(
        self,
        result: &ToolResult,
    ) -> Result<Workflow<Completed>, Workflow<Running>> {
        let should_complete = result.is_success()
            && self
                .definition
                .completion_tool
                .as_ref()
                .map(CompletionTool::name)
                == Some(result.tool().name());

        if !should_complete {
            return Err(self);
        }

        Ok(Workflow {
            definition: self.definition,
            completion_reason: Some(CompletionReason::ToolSucceeded {
                tool: result.tool().clone().into(),
            }),
            state: PhantomData,
        })
    }

    pub fn definition(&self) -> &WorkflowDefinition {
        &self.definition
    }
}

impl Workflow<Completed> {
    pub fn definition(&self) -> &WorkflowDefinition {
        &self.definition
    }

    pub fn completion_reason(&self) -> Option<&CompletionReason> {
        self.completion_reason.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit_tool() -> Tool {
        Tool::new("git_commit")
    }

    fn commit_fix_workflow() -> Workflow<Running> {
        Workflow::new(
            WorkflowDefinition::new("commit-fix")
                .with_tools(ToolSet::default().with(commit_tool()))
                .completes_on_successful_tool(commit_tool()),
        )
        .start()
    }

    #[test]
    fn successful_completion_transitions_to_completed() {
        let workflow = commit_fix_workflow();

        let completed = match workflow.handle_tool_result(&ToolResult::success(commit_tool())) {
            Ok(completed) => completed,
            Err(_) => panic!("successful completion must transition to completed"),
        };

        assert_eq!(
            completed.completion_reason(),
            Some(&CompletionReason::ToolSucceeded {
                tool: commit_tool().into(),
            })
        );
    }

    #[test]
    fn failed_completion_keeps_workflow_running() {
        let workflow = commit_fix_workflow();
        let workflow = match workflow.handle_tool_result(&ToolResult::failure(commit_tool())) {
            Err(workflow) => workflow,
            Ok(_) => panic!("failed completion must keep workflow running"),
        };

        assert_eq!(workflow.definition().name(), "commit-fix");
    }

    #[test]
    fn unrelated_success_keeps_workflow_running() {
        let workflow = commit_fix_workflow();

        let workflow =
            match workflow.handle_tool_result(&ToolResult::success(Tool::new("git_status"))) {
                Err(workflow) => workflow,
                Ok(_) => panic!("unrelated tool must keep workflow running"),
            };

        assert_eq!(workflow.definition().name(), "commit-fix");
    }
}
