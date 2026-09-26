use crate::Tool;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionTool(Tool);

impl CompletionTool {
    pub fn name(&self) -> &str {
        self.0.name()
    }
}

impl From<Tool> for CompletionTool {
    fn from(tool: Tool) -> Self {
        Self(tool)
    }
}
