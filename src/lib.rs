//! YOPO (You Only Prompt Once) - A simple library for testing ACP agents
//!
//! Provides a convenient API for running one-shot prompts against ACP components.

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AudioContent, ContentBlock, EmbeddedResourceResource, ImageContent, InitializeRequest,
    PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    PermissionOption,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, SessionUpdate,
    StopReason, TextContent,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{Agent, Client, ConnectTo, Dispatch, Handled, UntypedMessage};
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

/// Converts a `ContentBlock` to its string representation.
///
/// This function provides standard string conversions for different content types:
/// - `Text`: Returns the text content
/// - `Image`: Returns a placeholder like `[Image: image/png]`
/// - `Audio`: Returns a placeholder like `[Audio: audio/wav]`
/// - `ResourceLink`: Returns the URI
/// - `Resource`: Returns the URI
///
/// # Example
///
/// ```no_run
/// use kid_agentic_coding::content_block_to_string;
/// use agent_client_protocol::schema::v1::{ContentBlock, TextContent};
///
/// let block = ContentBlock::Text(TextContent::new("Hello".to_string()));
/// assert_eq!(content_block_to_string(&block), "Hello");
/// ```
#[must_use]
pub fn content_block_to_string(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(TextContent { text, .. }) => text.clone(),
        ContentBlock::Image(ImageContent { mime_type, .. }) => {
            format!("[Image: {mime_type}]")
        }
        ContentBlock::Audio(AudioContent { mime_type, .. }) => {
            format!("[Audio: {mime_type}]")
        }
        ContentBlock::ResourceLink(link) => link.uri.clone(),
        ContentBlock::Resource(resource) => match &resource.resource {
            EmbeddedResourceResource::TextResourceContents(text) => text.uri.clone(),
            EmbeddedResourceResource::BlobResourceContents(blob) => blob.uri.clone(),
            _ => "[Unknown resource type]".to_string(),
        },
        _ => "[Unknown content type]".to_string(),
    }
}

/// Runs a single prompt against a component with a callback for each content block.
///
/// This function:
/// - Spawns the component
/// - Initializes the agent
/// - Creates a new session
/// - Sends the prompt
/// - Auto-approves all permission requests
/// - Calls the callback with each `ContentBlock` from agent messages
/// - Returns when the prompt completes
///
/// The callback receives each `ContentBlock` as it arrives and can process it
/// asynchronously (e.g., print it, accumulate it, etc.).
///
/// # Example
///
/// ```ignore
/// use yopo::{prompt_with_callback, content_block_to_string};
/// use agent_client_protocol::AcpAgent;
/// use std::str::FromStr;
///
/// # async fn example() -> Result<(), agent_client_protocol::Error> {
/// let agent = AcpAgent::from_str("python agent.py")?;
/// prompt_with_callback(agent, "What is 2+2?", async |block| {
///     print!("{}", content_block_to_string(&block));
/// }).await?;
/// # Ok(())
/// # }
/// ```
pub async fn prompt_with_callback(
    component: impl ConnectTo<Client>,
    prompt_text: impl ToString,
    mut callback: impl AsyncFnMut(ContentBlock) + Send,
) -> Result<(), agent_client_protocol::Error> {
    // Convert prompt to String
    let prompt_text = prompt_text.to_string();

    // Run the client
    Client
        .builder()
        .on_receive_dispatch(
            async |message: Dispatch<UntypedMessage, UntypedMessage>, _cx| {
                tracing::trace!("received: {:?}", message.message());
                Ok(Handled::No {
                    message,
                    retry: false,
                })
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_with(
            component,
            |cx: agent_client_protocol::ConnectionTo<Agent>| async move {
                // Initialize the agent
                let _init_response = cx
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;

                let mut session = cx
                    .build_session(PathBuf::from("."))
                    .block_task()
                    .start_session()
                    .await?;

                session.send_prompt(prompt_text)?;

                loop {
                    let update = session.read_update().await?;
                    match update {
                        agent_client_protocol::SessionMessage::SessionMessage(message) => {
                            MatchDispatch::new(message)
                                .if_notification(async |notification: SessionNotification| {
                                    tracing::debug!(
                                        ?notification,
                                        "yopo: received SessionNotification"
                                    );
                                    // Call the callback for each agent message chunk
                                    if let SessionUpdate::AgentMessageChunk(content_chunk) =
                                        notification.update
                                    {
                                        callback(content_chunk.content).await;
                                    }
                                    Ok(())
                                })
                                .await
                                .if_request(async |request: RequestPermissionRequest, responder| {
                                    // Auto-approve all permission requests by selecting the first option
                                    // that looks "allow-ish"
                                    let outcome = request
                                        .options
                                        .iter()
                                        .find(|option| match option.kind {
                                            PermissionOptionKind::AllowOnce
                                            | PermissionOptionKind::AllowAlways => true,
                                            PermissionOptionKind::RejectOnce
                                            | PermissionOptionKind::RejectAlways
                                            | _ => false,
                                        })
                                        .map_or(RequestPermissionOutcome::Cancelled, |option| {
                                            RequestPermissionOutcome::Selected(
                                                SelectedPermissionOutcome::new(
                                                    option.option_id.clone(),
                                                ),
                                            )
                                        });

                                    responder.respond(RequestPermissionResponse::new(outcome))?;

                                    Ok(())
                                })
                                .await
                                .otherwise(async |_msg| Ok(()))
                                .await?;
                        }
                        agent_client_protocol::SessionMessage::StopReason(stop_reason) => {
                            match stop_reason {
                                StopReason::EndTurn => break,
                                StopReason::MaxTokens => {
                                    tracing::debug!("Agent hit max tokens limit");
                                    break;
                                }
                                StopReason::MaxTurnRequests => {
                                    tracing::debug!("Agent hit max turn requests limit");
                                    break;
                                }
                                StopReason::Refusal => {
                                    tracing::warn!("Agent refused to continue");
                                    break;
                                }
                                StopReason::Cancelled => {
                                    tracing::debug!("Session was cancelled");
                                    break;
                                }
                                other => {
                                    tracing::warn!("Unknown stop reason: {:?}", other);
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                }

                Ok(())
            },
        )
        .await?;

    Ok(())
}

/// Runs a single prompt against a component and returns the accumulated text response.
///
/// This function:
/// - Spawns the component
/// - Initializes the agent
/// - Creates a new session
/// - Sends the prompt
/// - Auto-approves all permission requests
/// - Accumulates all content from agent messages using [`content_block_to_string`]
/// - Returns the complete response as a String
///
/// This is a convenience wrapper around [`prompt_with_callback`] that accumulates
/// all content blocks into a single string.
///
/// # Example
///
/// ```ignore
/// use yopo::prompt;
/// use agent_client_protocol::AcpAgent;
/// use std::str::FromStr;
///
/// # async fn example() -> Result<(), agent_client_protocol::Error> {
/// let agent = AcpAgent::from_str("python agent.py")?;
/// let response = prompt(agent, "What is 2+2?").await?;
/// assert!(response.contains("4"));
/// # Ok(())
/// # }
/// ```
pub async fn prompt(
    component: impl ConnectTo<Client>,
    prompt_text: impl ToString,
) -> Result<String, agent_client_protocol::Error> {
    let mut accumulated_text = String::new();
    prompt_with_callback(component, prompt_text, async |block| {
        let text = content_block_to_string(&block);
        accumulated_text.push_str(&text);
    })
    .await?;
    Ok(accumulated_text)
}

/// Events emitted from an interactive session to a UI layer.
#[derive(Debug)]
pub enum SessionEvent {
    /// A chunk of agent message content.
    Chunk(Box<ContentBlock>),
    /// The agent requests permission to proceed.
    ///
    /// Reply with `Some(option_id)` to select an option, or `None` to cancel.
    PermissionRequest {
        options: Vec<PermissionOption>,
        reply: oneshot::Sender<Option<String>>,
    },
    /// The current turn ended with the given reason. The session stays open
    /// for further prompts.
    Stopped(StopReason),
}

/// Returned by [`SessionHandle::send_prompt`] when the session task has already ended.
#[derive(Debug)]
pub struct SessionClosed;

impl std::fmt::Display for SessionClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "interactive session task has ended")
    }
}

impl std::error::Error for SessionClosed {}

/// Handle to a running interactive ACP session.
///
/// Prompts are sent via [`SessionHandle::send_prompt`], updates are consumed via
/// [`SessionHandle::recv_event`]. The underlying agent connection stays open across
/// multiple turns until the handle is dropped.
pub struct SessionHandle {
    prompt_tx: mpsc::UnboundedSender<String>,
    event_rx: mpsc::UnboundedReceiver<SessionEvent>,
}

impl SessionHandle {
    /// Sends a new prompt into the running session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionClosed`] if the session task has already ended.
    pub fn send_prompt(&self, prompt_text: impl ToString) -> Result<(), SessionClosed> {
        self.prompt_tx
            .send(prompt_text.to_string())
            .map_err(|_| SessionClosed)
    }

    /// Awaits the next event from the session.
    ///
    /// Returns `None` once the session task has ended (e.g. startup failed,
    /// or the agent connection closed).
    pub async fn recv_event(&mut self) -> Option<SessionEvent> {
        self.event_rx.recv().await
    }
}

/// Starts an interactive ACP session that stays open across multiple prompts.
///
/// Unlike [`prompt_with_callback`], which runs a single turn and returns, this spawns
/// the agent connection as a background task and returns a [`SessionHandle`] immediately.
/// Send prompts and read [`SessionEvent`]s through the handle for as long as needed;
/// dropping the handle shuts the session down.
pub fn start_interactive_session(component: impl ConnectTo<Client> + 'static) -> SessionHandle {
    let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel::<String>();
    let (event_tx, event_rx) = mpsc::unbounded_channel::<SessionEvent>();

    tokio::spawn(async move {
        let result = Client
            .builder()
            .on_receive_dispatch(
                async |message: Dispatch<UntypedMessage, UntypedMessage>, _cx| {
                    tracing::trace!("received: {:?}", message.message());
                    Ok(Handled::No {
                        message,
                        retry: false,
                    })
                },
                agent_client_protocol::on_receive_dispatch!(),
            )
            .connect_with(
                component,
                |cx: agent_client_protocol::ConnectionTo<Agent>| async move {
                    let _init_response = cx
                        .send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;

                    let mut session = cx
                        .build_session(PathBuf::from("."))
                        .block_task()
                        .start_session()
                        .await?;

                    loop {
                        tokio::select! {
                            prompt = prompt_rx.recv() => {
                                match prompt {
                                    Some(text) => session.send_prompt(text)?,
                                    None => break,
                                }
                            }
                            update = session.read_update() => {
                                match update? {
                                    agent_client_protocol::SessionMessage::SessionMessage(message) => {
                                        MatchDispatch::new(message)
                                            .if_notification(async |notification: SessionNotification| {
                                                if let SessionUpdate::AgentMessageChunk(content_chunk) =
                                                    notification.update
                                                {
                                                    let _ = event_tx
                                                        .send(SessionEvent::Chunk(Box::new(content_chunk.content)));
                                                }
                                                Ok(())
                                            })
                                            .await
                                            .if_request(
                                                async |request: RequestPermissionRequest, responder| {
                                                    let (reply_tx, reply_rx) = oneshot::channel();
                                                    let _ = event_tx.send(SessionEvent::PermissionRequest {
                                                        options: request.options.clone(),
                                                        reply: reply_tx,
                                                    });

                                                    let outcome = match reply_rx.await {
                                                        Ok(Some(option_id)) => {
                                                            RequestPermissionOutcome::Selected(
                                                                SelectedPermissionOutcome::new(option_id),
                                                            )
                                                        }
                                                        _ => RequestPermissionOutcome::Cancelled,
                                                    };

                                                    responder
                                                        .respond(RequestPermissionResponse::new(outcome))?;
                                                    Ok(())
                                                },
                                            )
                                            .await
                                            .otherwise(async |_msg| Ok(()))
                                            .await?;
                                    }
                                    agent_client_protocol::SessionMessage::StopReason(stop_reason) => {
                                        let _ = event_tx.send(SessionEvent::Stopped(stop_reason));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    Ok(())
                },
            )
            .await;

        if let Err(err) = result {
            tracing::warn!(?err, "interactive session task ended with error");
        }
    });

    SessionHandle {
        prompt_tx,
        event_rx,
    }
}

