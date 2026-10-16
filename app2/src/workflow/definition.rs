use crate::{CompletionTool, Tool, ToolSet};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkflowDefinition {
    name: &'static str,
    entry_tool: Option<Tool>,
    tools: ToolSet,
    pub(super) completion_tool: Option<CompletionTool>,
}

impl WorkflowDefinition {
    pub fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    pub fn entered_by(mut self, tool: Tool) -> Self {
        self.entry_tool = Some(tool);
        self
    }

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

    pub fn name(&self) -> &str {
        self.name
    }

    pub fn entry_tool(&self) -> Option<&Tool> {
        self.entry_tool.as_ref()
    }

    pub fn exit_tool(&self) -> Option<&CompletionTool> {
        self.completion_tool.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowPrompt(String);

impl WorkflowPrompt {
    pub(super) fn new(prompt: String) -> Self {
        Self(prompt)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowCompletionReason {
    ToolSucceeded { tool: CompletionTool },
}
