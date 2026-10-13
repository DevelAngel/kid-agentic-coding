mod mcp;
mod session;

pub use mcp::McpServer;
pub use session::{Active, Closed, Deleted, Session};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest, PromptRequest,
    PromptResponse, SessionNotification, TextContent,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Client as AcpClient, Error};
use anyhow::Context as _;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::channel::{self, AcpEnds, AgentEvent};

pub struct Disconnected {
    config: AcpAgentConfig,
}

pub struct Connected {
    shutdown: oneshot::Sender<()>,
    prompt_tx: mpsc::Sender<(String, oneshot::Sender<Result<PromptResponse, Error>>)>,
    task: JoinHandle<Result<(), Error>>,
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
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<Client<Connected>, Error> {
        let Disconnected { config } = self.state;
        let agent = AcpAgent::new(config.clone());

        let (ready_tx, ready_rx) = oneshot::channel::<Result<InitializeResponse, Error>>();
        let (error_tx, error_rx) = oneshot::channel();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let (prompt_tx, mut prompt_rx) =
            mpsc::channel::<(String, oneshot::Sender<Result<PromptResponse, Error>>)>(1);

        let task = tokio::spawn(async move {
            let result = AcpClient
                .builder()
                .on_receive_notification(
                    async move |notification: SessionNotification, _| {
                        let _ = events.send(notification).await;
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_with(agent, async move |connection| {
                    let request = InitializeRequest::new(ProtocolVersion::V1);
                    let response = connection.send_request(request).block_task().await?;
                    let session = connection
                        .send_request(NewSessionRequest::new(
                            std::env::current_dir().map_err(|_| Error::internal_error())?,
                        ))
                        .block_task()
                        .await?;
                    ready_tx
                        .send(Ok(response))
                        .map_err(|_| Error::internal_error())?;

                    loop {
                        tokio::select! {
                            result = &mut shutdown_rx => {
                                result.map_err(|_| Error::internal_error())?;
                                break;
                            }
                            _ = connection.incoming_closed() => {
                                break;
                            }
                            prompt = prompt_rx.recv() => {
                                let Some((prompt, response_tx)) = prompt else {
                                    break;
                                };

                                tracing::info!(%prompt, "sending prompt");
                                let result = connection
                                    .send_request(PromptRequest::new(
                                        session.session_id.clone(),
                                        vec![ContentBlock::Text(TextContent::new(prompt))],
                                    ))
                                    .block_task()
                                    .await;

                                match result {
                                    Ok(response) => {
                                        tracing::info!(?response.stop_reason, "agent completed");
                                        let _ = response_tx.send(Ok(response));
                                    }
                                    Err(error) => {
                                        let _ = response_tx.send(Err(error.clone()));
                                        return Err(error);
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
            Ok(_) => {}
            Err(_) => return Err(error_rx.await.map_err(|_| Error::internal_error())?),
        }
        Ok(Client {
            state: Connected {
                shutdown: shutdown_tx,
                prompt_tx,
                task,
                config,
            },
        })
    }

    pub(crate) async fn serve(self, ends: AcpEnds) {
        let AcpEnds {
            prompts,
            sessions: _sessions,
            events,
            completion,
        } = ends;

        let outcome = async {
            let client = self
                .connect(events)
                .await
                .context("failed to connect to agent")?;
            client.serve_prompts(prompts).await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        let _ = completion.send(outcome).await;
    }
}

impl Client<Connected> {
    pub async fn prompt(&self, prompt: String) -> Result<PromptResponse, Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.state
            .prompt_tx
            .send((prompt, response_tx))
            .await
            .map_err(|_| Error::internal_error())?;
        response_rx.await.map_err(|_| Error::internal_error())?
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
    ) -> Result<Client<Disconnected>, Error> {
        while let Some(channel::PromptRequest { text, reply }) = prompts.recv().await {
            let result = self.prompt(text).await;
            let failed = result.is_err();
            let _ = reply.send(result);
            if failed {
                break;
            }
        }
        self.disconnect().await
    }

    #[cfg(test)]
    pub(super) fn connected_for_test() -> Client<Connected> {
        let (shutdown, _receiver) = oneshot::channel();
        let (prompt_tx, _receiver) = mpsc::channel(1);
        let task = tokio::spawn(async { Ok(()) });

        Client {
            state: Connected {
                shutdown,
                prompt_tx,
                task,
                config: AcpAgentConfig::new("/bin/true"),
            },
        }
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

    #[tokio::test]
    async fn connected_client_can_disconnect() {
        let client = Client::<Connected>::connected_for_test();
        let client = client.disconnect().await.unwrap();
        let _: Client<Disconnected> = client;
    }

    #[tokio::test]
    async fn serving_disconnects_when_the_prompt_sender_is_dropped() {
        let (prompts_tx, prompts_rx) = mpsc::channel(1);
        drop(prompts_tx);

        let client = Client::<Connected>::connected_for_test()
            .serve_prompts(prompts_rx)
            .await
            .unwrap();

        let _: Client<Disconnected> = client;
    }

    #[tokio::test]
    async fn failed_prompt_is_reported_to_the_requester() {
        let (prompts_tx, prompts_rx) = mpsc::channel(1);
        let (reply, reply_rx) = oneshot::channel();
        prompts_tx
            .send(channel::PromptRequest {
                text: "hello".to_owned(),
                reply,
            })
            .await
            .unwrap();
        drop(prompts_tx);

        let _ = Client::<Connected>::connected_for_test()
            .serve_prompts(prompts_rx)
            .await;

        assert!(reply_rx.await.unwrap().is_err());
    }
}
