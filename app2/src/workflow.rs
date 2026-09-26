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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowAction {
    Continue,
    Complete(CompletionReason),
}

pub struct WorkflowRuntime {
    definition: WorkflowDefinition,
    running: bool,
}

impl WorkflowRuntime {
    pub fn start(definition: WorkflowDefinition) -> Self {
        Self {
            definition,
            running: true,
        }
    }

    pub fn handle_tool_result(&mut self, result: &ToolResult) -> WorkflowAction {
        if !self.running {
            return WorkflowAction::Continue;
        }

        let should_complete = result.is_success()
            && self
                .definition
                .completion_tool
                .as_ref()
                .map(CompletionTool::name)
                == Some(result.tool().name());

        if should_complete {
            self.running = false;
            return WorkflowAction::Complete(CompletionReason::ToolSucceeded {
                tool: result.tool().clone().into(),
            });
        }

        WorkflowAction::Continue
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn definition(&self) -> &WorkflowDefinition {
        &self.definition
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn commit_tool() -> Tool {
        Tool::new("git_commit")
    }
    #[test]
    fn successful_completion_tool_stops_workflow() {
        let commit = commit_tool();
        let definition = WorkflowDefinition::new("commit-fix")
            .with_tools(ToolSet::default().with(commit.clone()))
            .completes_on_successful_tool(commit.clone());
        let mut workflow = WorkflowRuntime::start(definition);

        let action = workflow.handle_tool_result(&ToolResult::success(commit));

        assert_eq!(
            action,
            WorkflowAction::Complete(CompletionReason::ToolSucceeded {
                tool: commit_tool().into(),
            })
        );
        assert!(!workflow.is_running());
    }

    #[test]
    fn failed_completion_tool_does_not_stop_workflow() {
        let commit = commit_tool();
        let definition = WorkflowDefinition::new("commit-fix")
            .with_tools(ToolSet::default().with(commit.clone()))
            .completes_on_successful_tool(commit.clone());
        let mut workflow = WorkflowRuntime::start(definition);

        let action = workflow.handle_tool_result(&ToolResult::failure(commit));

        assert_eq!(action, WorkflowAction::Continue);
        assert!(workflow.is_running());
    }

    #[test]
    fn unrelated_successful_tool_does_not_stop_workflow() {
        let commit = commit_tool();
        let status = Tool::new("git_status");
        let definition = WorkflowDefinition::new("commit-fix")
            .with_tools(ToolSet::default().with(commit.clone()).with(status.clone()))
            .completes_on_successful_tool(commit.clone());
        let mut workflow = WorkflowRuntime::start(definition);

        let action = workflow.handle_tool_result(&ToolResult::success(status));

        assert_eq!(action, WorkflowAction::Continue);
        assert!(workflow.is_running());
    }
}
