use crate::{CompletionTool, Tool, ToolSet};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkflowDefinition {
    tools: ToolSet,
    pub(super) completion_tool: Option<CompletionTool>,
}

impl WorkflowDefinition {
    pub fn with_tools(mut self, tools: ToolSet) -> Self {
        self.tools = tools;
        self
    }

    pub fn completes_on_successful_tool(mut self, tool: Tool) -> Self {
        self.completion_tool = Some(tool.into());
        self
    }

    pub fn tools(&self) -> &ToolSet {
        &self.tools
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowCompletionReason {
    ToolSucceeded { tool: CompletionTool },
}
