use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, CloseSessionRequest, CloseSessionResponse, ContentBlock,
    DeleteSessionRequest, DeleteSessionResponse, InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionCapabilities,
    SessionDeleteCapabilities, SessionId, StopReason,
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
    Close(SessionId),
    Delete(SessionId),
}

#[derive(Clone, Default)]
pub(super) struct TestAgent {
    recorded: Arc<Mutex<Vec<Recorded>>>,
    fail_prompts: bool,
    delete_supported: bool,
}

impl TestAgent {
    pub(super) fn failing_prompts() -> Self {
        Self {
            fail_prompts: true,
            ..Self::default()
        }
    }

    pub(super) fn deleting() -> Self {
        Self {
            delete_supported: true,
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
        let close = self.clone();
        let delete = self.clone();
        let initialize = self.clone();

        Agent
            .builder()
            .on_receive_request(
                async move |_request: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _connection: ConnectionTo<AcpClient>| {
                    let mut response = InitializeResponse::new(ProtocolVersion::V1);
                    if initialize.delete_supported {
                        response = response.agent_capabilities(
                            AgentCapabilities::new().session_capabilities(
                                SessionCapabilities::new().delete(SessionDeleteCapabilities::new()),
                            ),
                        );
                    }
                    responder.respond(response)
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
            .on_receive_request(
                async move |request: CloseSessionRequest,
                            responder: Responder<CloseSessionResponse>,
                            _connection: ConnectionTo<AcpClient>| {
                    close
                        .recorded
                        .lock()
                        .unwrap()
                        .push(Recorded::Close(request.session_id));
                    responder.respond(CloseSessionResponse::default())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: DeleteSessionRequest,
                            responder: Responder<DeleteSessionResponse>,
                            _connection: ConnectionTo<AcpClient>| {
                    delete
                        .recorded
                        .lock()
                        .unwrap()
                        .push(Recorded::Delete(request.session_id));
                    responder.respond(DeleteSessionResponse::default())
                },
                agent_client_protocol::on_receive_request!(),
            )
    }
}
