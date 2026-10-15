mod events;
mod mcp;
mod session;

pub use mcp::McpServer;
pub use session::{Active, Closed, Deleted, Session};

use crate::ToolEvent;
use crate::channel::{self, AcpEnds};
use events::Router;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CloseSessionRequest, ContentBlock, DeleteSessionRequest, InitializeRequest, NewSessionRequest,
    PromptRequest, PromptResponse, SessionId, SessionNotification, TextContent,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Client as AcpClient, ConnectTo, Error};
use anyhow::Context as _;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use std::env;

enum Command {
    NewSession {
        mcp_servers: Vec<McpServer>,
        reply: oneshot::Sender<Result<SessionId, Error>>,
    },
    Prompt {
        session_id: SessionId,
        text: String,
        reply: oneshot::Sender<Result<PromptResponse, Error>>,
    },
    Close {
        session_id: SessionId,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    Delete {
        session_id: SessionId,
        reply: oneshot::Sender<Result<(), Error>>,
    },
}

impl Command {
    async fn request<T>(
        commands: &mpsc::Sender<Command>,
        command: impl FnOnce(oneshot::Sender<Result<T, Error>>) -> Command,
    ) -> Result<T, Error> {
        let (reply, response) = oneshot::channel();
        commands
            .send(command(reply))
            .await
            .map_err(|_| Error::internal_error())?;
        response.await.map_err(|_| Error::internal_error())?
    }
}

pub struct Disconnected {
    config: AcpAgentConfig,
}

pub struct Connected {
    shutdown: oneshot::Sender<()>,
    commands: mpsc::Sender<Command>,
    task: JoinHandle<Result<(), Error>>,
    router: Router,
    config: AcpAgentConfig,
}

pub struct Client<S> {
    state: S,
}

impl Client<Disconnected> {
    pub fn new(agent: AcpAgent) -> Self {
        Self {
            state: Disconnected {
                config: agent.into_config(),
            },
        }
    }

    pub async fn connect(
        self,
        events: mpsc::Sender<SessionNotification>,
    ) -> Result<Client<Connected>, Error> {
        self.connect_observing(Some(events)).await
    }

    async fn connect_observing(
        self,
        events: Option<mpsc::Sender<SessionNotification>>,
    ) -> Result<Client<Connected>, Error> {
        let Disconnected { config } = self.state;
        let agent = AcpAgent::new(config.clone());
        Client::connect_over(agent, config, events).await
    }

    pub(crate) async fn serve(self, ends: AcpEnds) -> anyhow::Result<()> {
        let AcpEnds { prompts, events } = ends;
        let client = self
            .connect_observing(None)
            .await
            .context("failed to connect to agent")?;
        client.serve_prompts(prompts, events).await?;
        Ok(())
    }
}

impl Client<Connected> {
    async fn connect_over(
        transport: impl ConnectTo<AcpClient>,
        config: AcpAgentConfig,
        events: Option<mpsc::Sender<SessionNotification>>,
    ) -> Result<Client<Connected>, Error> {
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        let (error_tx, error_rx) = oneshot::channel();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let (commands_tx, mut commands_rx) = mpsc::channel::<Command>(1);
        let router = Router::default();
        let handler_router = router.clone();

        let task = tokio::spawn(async move {
            let result = AcpClient
                .builder()
                .on_receive_notification(
                    async move |notification: SessionNotification, _| {
                        tracing::info!(?notification.update, "agent message");
                        handler_router.dispatch(&notification).await;
                        if let Some(events) = &events {
                            let _ = events.send(notification).await;
                        }
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_with(transport, async move |connection| {
                    let initialize = connection
                        .send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;
                    ready_tx.send(()).map_err(|_| Error::internal_error())?;

                    loop {
                        tokio::select! {
                            _ = &mut shutdown_rx => {
                                break;
                            }
                            _ = connection.incoming_closed() => {
                                break;
                            }
                            command = commands_rx.recv() => {
                                let Some(command) = command else {
                                    break;
                                };

                                match command {
                                    Command::NewSession { mcp_servers, reply } => {
                                        let result = match env::current_dir() {
                                            Ok(cwd) => connection
                                                .send_request(
                                                    NewSessionRequest::new(cwd)
                                                        .mcp_servers(mcp_servers),
                                                )
                                                .block_task()
                                                .await
                                                .map(|response| response.session_id),
                                            Err(_) => Err(Error::internal_error()),
                                        };
                                        let _ = reply.send(result);
                                    }
                                    Command::Prompt { session_id, text, reply } => {
                                        tracing::info!(prompt = %text, "sending prompt");
                                        let result = connection
                                            .send_request(PromptRequest::new(
                                                session_id,
                                                vec![ContentBlock::Text(TextContent::new(text))],
                                            ))
                                            .block_task()
                                            .await;

                                        match result {
                                            Ok(response) => {
                                                tracing::info!(?response.stop_reason, "agent completed");
                                                let _ = reply.send(Ok(response));
                                            }
                                            Err(error) => {
                                                let _ = reply.send(Err(error.clone()));
                                                return Err(error);
                                            }
                                        }
                                    }
                                    Command::Close { session_id, reply } => {
                                        let result = connection
                                            .send_request(CloseSessionRequest::new(session_id))
                                            .block_task()
                                            .await
                                            .map(|_| ());
                                        let _ = reply.send(result);
                                    }
                                    Command::Delete { session_id, reply } => {
                                        let result = if initialize
                                            .agent_capabilities
                                            .session_capabilities
                                            .delete
                                            .is_some()
                                        {
                                            connection
                                                .send_request(DeleteSessionRequest::new(session_id))
                                                .block_task()
                                                .await
                                                .map(|_| ())
                                        } else {
                                            Err(Error::method_not_found())
                                        };
                                        let _ = reply.send(result);
                                    }

                                }
                            }
                        }
                    }

                    Ok(())
                })
                .await;
            if let Err(error) = &result {
                let _ = error_tx.send(error.clone());
            }

            result
        });

        match ready_rx.await {
            Ok(()) => {}
            Err(_) => return Err(error_rx.await.map_err(|_| Error::internal_error())?),
        }
        Ok(Client {
            state: Connected {
                shutdown: shutdown_tx,
                commands: commands_tx,
                task,
                router,
                config,
            },
        })
    }

    pub async fn disconnect(self) -> Result<Client<Disconnected>, Error> {
        let Connected {
            config,
            shutdown,
            task,
            ..
        } = self.state;
        drop(shutdown);
        task.await.map_err(|_| Error::internal_error())??;
        Ok(Client {
            state: Disconnected { config },
        })
    }

    pub(crate) async fn serve_prompts(
        self,
        mut prompts: mpsc::Receiver<channel::PromptRequest>,
        events: mpsc::Sender<ToolEvent>,
    ) -> Result<Client<Disconnected>, Error> {
        let session = self.new_session(Vec::new()).await?;
        self.route(&session, events);
        while let Some(channel::PromptRequest { text, reply }) = prompts.recv().await {
            let result = session.prompt(text).await;
            let failed = result.is_err();
            let _ = reply.send(result);
            if failed {
                break;
            }
        }
        drop(session);
        self.disconnect().await
    }
}
