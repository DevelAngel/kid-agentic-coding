use crate::{McpServer, Tool};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkflowDefinition {
    mcp_servers: Vec<McpServer>,
    pub(super) completion_tool: Option<Tool>,
}

impl WorkflowDefinition {
    pub fn with_mcp_server(mut self, server: McpServer) -> Self {
        self.mcp_servers.push(server);
        self
    }

    pub fn completes_on_successful_tool(mut self, tool: Tool) -> Self {
        self.completion_tool = Some(tool);
        self
    }

    pub fn mcp_servers(&self) -> &[McpServer] {
        &self.mcp_servers
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowCompletionReason {
    ToolSucceeded { tool: Tool },
}
