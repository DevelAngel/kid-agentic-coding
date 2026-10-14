use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, SessionId, StopReason,
};
use agent_client_protocol::{
    AcpAgentConfig, Agent, Client as AcpClient, ConnectTo, ConnectionTo, Error, Responder,
};
use tokio::sync::mpsc;

use super::{Client, Connected, McpServer};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Recorded {
    NewSession(Vec<McpServer>),
    Prompt(SessionId, String),
}

#[derive(Clone, Default)]
pub(super) struct TestAgent {
    recorded: Arc<Mutex<Vec<Recorded>>>,
    fail_prompts: bool,
}

impl TestAgent {
    pub(super) fn failing_prompts() -> Self {
        Self {
            fail_prompts: true,
            ..Self::default()
        }
    }

    pub(super) fn recorded(&self) -> Vec<Recorded> {
        self.recorded.lock().unwrap().clone()
    }

    pub(super) async fn connect(&self) -> Client<Connected> {
        let (events, _events_rx) = mpsc::channel(1);
        Client::connect_over(self.transport(), AcpAgentConfig::new("/bin/true"), events)
            .await
            .unwrap()
    }

    fn transport(&self) -> impl ConnectTo<AcpClient> {
        let new_session = self.clone();
        let prompt = self.clone();

        Agent
            .builder()
            .on_receive_request(
                async move |_request: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _connection: ConnectionTo<AcpClient>| {
                    responder.respond(InitializeResponse::new(ProtocolVersion::V1))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: NewSessionRequest,
                            responder: Responder<NewSessionResponse>,
                            _connection: ConnectionTo<AcpClient>| {
                    let mut recorded = new_session.recorded.lock().unwrap();
                    let count = recorded
                        .iter()
                        .filter(|entry| matches!(entry, Recorded::NewSession(_)))
                        .count();
                    recorded.push(Recorded::NewSession(request.mcp_servers));
                    drop(recorded);
                    responder.respond(NewSessionResponse::new(SessionId::new(format!(
                        "test-session-{count}"
                    ))))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest,
                            responder: Responder<PromptResponse>,
                            _connection: ConnectionTo<AcpClient>| {
                    let text = request
                        .prompt
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<String>();
                    prompt
                        .recorded
                        .lock()
                        .unwrap()
                        .push(Recorded::Prompt(request.session_id, text));
                    if prompt.fail_prompts {
                        return Err(Error::internal_error());
                    }
                    responder.respond(PromptResponse::new(StopReason::EndTurn))
                },
                agent_client_protocol::on_receive_request!(),
            )
    }
}
