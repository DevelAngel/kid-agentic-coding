mod mcp;
mod session;

pub use mcp::McpServer;
pub use session::{Active, Closed, Deleted, Session};

use agent_client_protocol::schema::{ProtocolVersion, v1};
use agent_client_protocol::{AcpAgent, Client as AcpClient, Error};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub struct Disconnected {
    agent: AcpAgent,
}

pub struct Connected {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<Result<(), Error>>,
    response: v1::InitializeResponse,
}

pub struct Initialized {
    _shutdown: oneshot::Sender<()>,
    task: JoinHandle<Result<(), Error>>,
    response: v1::InitializeResponse,
}

pub struct Client<S> {
    state: S,
}

impl Client<Disconnected> {
    pub fn new(agent: AcpAgent) -> Self {
        Self {
            state: Disconnected { agent },
        }
    }

    pub async fn connect(self) -> Result<Client<Connected>, Error> {
        let Disconnected { agent } = self.state;
        let (ready_tx, ready_rx) = oneshot::channel::<Result<v1::InitializeResponse, Error>>();
        let (error_tx, error_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let task = tokio::spawn(async move {
            let result = AcpClient
                .builder()
                .on_receive_notification(
                    async |notification: v1::SessionNotification, _| {
                        tracing::info!(?notification.update, "agent message");
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_with(agent, async move |connection| {
                    let request = v1::InitializeRequest::new(ProtocolVersion::V1);
                    let response = connection.send_request(request).block_task().await?;
                    ready_tx
                        .send(Ok(response))
                        .map_err(|_| Error::internal_error())?;
                    let session = connection
                        .send_request(v1::NewSessionRequest::new(
                            std::env::current_dir().map_err(|_| Error::internal_error())?,
                        ))
                        .block_task()
                        .await?;
                    let prompt = "Hello from app2! Please reply with a short greeting.";
                    tracing::info!(%prompt, "sending prompt");
                    let response = connection
                        .send_request(v1::PromptRequest::new(
                            session.session_id,
                            vec![v1::ContentBlock::Text(v1::TextContent::new(prompt))],
                        ))
                        .block_task()
                        .await?;
                    tracing::info!(?response.stop_reason, "agent completed");

                    tokio::select! {
                        result = shutdown_rx => {
                            result.map_err(|_| Error::internal_error())?;
                        }
                        _ = connection.incoming_closed() => {}
                    }

                    Ok(())
                })
                .await;
            if let Err(error) = &result {
                let _ = error_tx.send(error.clone());
            }

            result
        });

        let response = match ready_rx.await {
            Ok(response) => response?,
            Err(_) => return Err(error_rx.await.map_err(|_| Error::internal_error())?),
        };
        Ok(Client {
            state: Connected {
                shutdown: shutdown_tx,
                task,
                response,
            },
        })
    }
}

impl Client<Connected> {
    pub fn initialize(self) -> Client<Initialized> {
        let Connected {
            shutdown,
            task,
            response,
        } = self.state;

        Client {
            state: Initialized {
                _shutdown: shutdown,
                task,
                response,
            },
        }
    }

    #[cfg(test)]
    pub(super) fn connected_for_test() -> Client<Connected> {
        let (shutdown, _receiver) = oneshot::channel();
        let task = tokio::spawn(async { Ok(()) });

        Client {
            state: Connected {
                shutdown,
                task,
                response: v1::InitializeResponse::new(ProtocolVersion::V1),
            },
        }
    }
}

impl Client<Initialized> {
    pub fn initialize_response(&self) -> &v1::InitializeResponse {
        &self.state.response
    }

    pub async fn wait(self) -> Result<(), Error> {
        self.state.task.await.map_err(|_| Error::internal_error())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_client_requires_an_agent() {
        let agent = AcpAgent::from_args(["/bin/true"]).unwrap();
        let client = Client::<Disconnected>::new(agent);
        let _: Client<Disconnected> = client;
    }
}
