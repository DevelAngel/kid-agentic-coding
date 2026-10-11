mod commit_fix;

mod mcp;
mod session;

pub use commit_fix::CommitFixRequest;
pub use mcp::McpServer;

pub use session::{Active, Closed, Deleted, Session};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest,
    PromptRequest, PromptResponse, SessionNotification, SessionUpdate, TextContent, ToolCallStatus,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Client as AcpClient, Error};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

pub struct Disconnected {
    config: AcpAgentConfig,
}

struct PromptCommand {
    prompt: String,
    cwd: Option<std::path::PathBuf>,
    new_session: bool,
    response_tx: oneshot::Sender<Result<PromptResponse, Error>>,
}

pub struct Connected {
    commit_fix_rx: mpsc::UnboundedReceiver<CommitFixRequest>,
    shutdown: oneshot::Sender<()>,
    prompt_tx: mpsc::Sender<PromptCommand>,
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

    pub async fn connect(self) -> Result<Client<Connected>, Error> {
        let Disconnected { config } = self.state;
        let agent = AcpAgent::new(config.clone());

        let (ready_tx, ready_rx) = oneshot::channel::<Result<InitializeResponse, Error>>();
        let (commit_fix_tx, commit_fix_rx) = mpsc::unbounded_channel();
        let pending_commit_fix = Arc::new(Mutex::new(HashMap::<String, CommitFixRequest>::new()));
        let transition_started = Arc::new(AtomicBool::new(false));
        let transition_started_handler = Arc::clone(&transition_started);
        let pending_commit_fix_handler = Arc::clone(&pending_commit_fix);
        let (error_tx, error_rx) = oneshot::channel();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let (prompt_tx, mut prompt_rx) = mpsc::channel::<PromptCommand>(1);
        let task = tokio::spawn(async move {
            let result = AcpClient

                .builder()
                .on_receive_notification(
                    async move |notification: SessionNotification, connection| {
                        match &notification.update {
                            SessionUpdate::ToolCall(call)
                                if CommitFixRequest::matches_tool_title(&call.title) && !transition_started_handler.load(Ordering::Relaxed) =>
                            {
                                if let Some(input) = call.raw_input.as_ref() {
                                    match CommitFixRequest::try_from(input) {
                                        Ok(request) => match call.status {

                                            ToolCallStatus::Completed if !transition_started_handler.swap(true, Ordering::Relaxed) => {
                                                commit_fix_tx
                                                    .send(request)
                                                    .map_err(|_| Error::internal_error())?;
                                                connection.send_notification(CancelNotification::new(
                                                    notification.session_id.clone(),
                                                ))?;
                                            }
                                            ToolCallStatus::Completed => {}
                                            ToolCallStatus::Failed => {}
                                            _ => {
                                                if let Ok(mut pending) = pending_commit_fix_handler.lock() {
                                                    pending.insert(call.tool_call_id.to_string(), request);
                                                }
                                            }
                                        },
                                        Err(error) => {
                                            tracing::warn!(%error, "invalid commit-fix tool input");
                                        }
                                    }
                                }
                            }
                            SessionUpdate::ToolCallUpdate(update) => {
                                match update.fields.status {
                                    Some(ToolCallStatus::Completed) => {
                                        let request = pending_commit_fix_handler
                                            .lock()
                                            .ok()
                                            .and_then(|mut pending| {
                                                pending.remove(&update.tool_call_id.to_string())
                                            });
                                        if let Some(request) = request.filter(|_| !transition_started_handler.swap(true, Ordering::Relaxed)) {
                                            commit_fix_tx
                                                .send(request)
                                                .map_err(|_| Error::internal_error())?;
                                            connection.send_notification(CancelNotification::new(
                                                notification.session_id.clone(),
                                            ))?;
                                        }
                                    }
                                    Some(ToolCallStatus::Failed) => {
                                        if let Ok(mut pending) = pending_commit_fix_handler.lock() {
                                            pending.remove(&update.tool_call_id.to_string());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                        tracing::info!(?notification.update, "agent message");
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
                                let Some(PromptCommand { prompt, cwd, new_session, response_tx }) = prompt else {
                                    break;
                                };

                                tracing::info!(%prompt, "sending prompt");
                                let target_session_id = if new_session {
                                    let cwd = cwd.unwrap_or(
                                        std::env::current_dir().map_err(|_| Error::internal_error())?,
                                    );
                                    connection
                                        .send_request(NewSessionRequest::new(cwd))
                                        .block_task()
                                        .await?
                                        .session_id
                                } else {
                                    session.session_id.clone()
                                };
                                let result = connection
                                    .send_request(PromptRequest::new(
                                        target_session_id,
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
            // The initialization response was received.
            Err(_) => return Err(error_rx.await.map_err(|_| Error::internal_error())?),
        }
        Ok(Client {
            state: Connected {
                shutdown: shutdown_tx,

                commit_fix_rx,
                prompt_tx,
                task,
                config,
            },
        })
    }
}

impl Client<Connected> {
    pub async fn prompt(&self, prompt: String) -> Result<PromptResponse, Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.state
            .prompt_tx
            .send(PromptCommand {
                prompt,
                cwd: None,
                new_session: false,
                response_tx,
            })
            .await
            .map_err(|_| Error::internal_error())?;
        response_rx.await.map_err(|_| Error::internal_error())?
    }

    pub async fn prompt_in_new_session(
        &self,
        prompt: String,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<PromptResponse, Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.state
            .prompt_tx
            .send(PromptCommand {
                prompt,
                cwd,
                new_session: true,
                response_tx,
            })
            .await
            .map_err(|_| Error::internal_error())?;
        response_rx.await.map_err(|_| Error::internal_error())?
    }

    pub fn take_commit_fix_request(&mut self) -> Option<CommitFixRequest> {
        self.state.commit_fix_rx.try_recv().ok()
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

    #[cfg(test)]
    pub(super) fn connected_for_test() -> Client<Connected> {
        let (shutdown, _receiver) = oneshot::channel();
        let (prompt_tx, _receiver) = mpsc::channel(1);
        let (_commit_fix_tx, commit_fix_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async { Ok(()) });

        Client {
            state: Connected {
                commit_fix_rx,
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
    fn recognizes_commit_fix_tool_names_and_titles() {
        assert!(CommitFixRequest::matches_tool_title("git_commit_with_fix"));
        assert!(CommitFixRequest::matches_tool_title("Git Commit With Fix"));
        assert!(!CommitFixRequest::matches_tool_title("git_commit"));
    }

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
}
