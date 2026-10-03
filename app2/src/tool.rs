#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpServer {
    name: String,
}

impl McpServer {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tool {
    name: String,
}

impl Tool {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolResult {
    tool: Tool,
    success: bool,
}

impl ToolResult {
    pub fn success(tool: Tool) -> Self {
        Self {
            tool,
            success: true,
        }
    }

    pub fn failure(tool: Tool) -> Self {
        Self {
            tool,
            success: false,
        }
    }

    pub fn tool(&self) -> &Tool {
        &self.tool
    }

    pub fn is_success(&self) -> bool {
        self.success
    }
}
