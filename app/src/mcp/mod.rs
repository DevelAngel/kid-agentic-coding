//! MCP tools registered with the agent when it advertises MCP-over-ACP support.

mod confetti;
mod milestone;

pub use confetti::confetti_mcp_server;
pub use milestone::{
    GhAvailability, MilestoneCli, SystemGhCli, ToolUnavailableChoice,
    ask_user_about_unavailable_tool, milestone_mcp_server,
};

use agent_client_protocol::schema::v1::InitializeResponse;

/// Whether the connected agent advertises MCP-over-ACP support.
pub fn supports_mcp(init_response: &InitializeResponse) -> bool {
    init_response.agent_capabilities.mcp_capabilities.acp
}

#[cfg(test)]
mod supports_mcp_tests {
    use super::supports_mcp;
    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::schema::v1::InitializeResponse;

    #[test]
    fn true_when_agent_advertises_mcp_acp_capability() {
        let mut response = InitializeResponse::new(ProtocolVersion::V1);
        response.agent_capabilities.mcp_capabilities.acp = true;

        assert!(supports_mcp(&response));
    }

    #[test]
    fn false_when_agent_does_not_advertise_mcp_acp_capability() {
        let response = InitializeResponse::new(ProtocolVersion::V1);

        assert!(!supports_mcp(&response));
    }
}
