//! One-shot prompt execution against ACP components.

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AudioContent, ContentBlock, EmbeddedResourceResource, ImageContent, InitializeRequest,
    PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, SessionUpdate,
    StopReason, TextContent,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{
    AcpAgent, Agent, Client, ConnectTo, ConnectionTo, Dispatch, Handled, SessionMessage,
    UntypedMessage,
};
use std::path::PathBuf;
use thiserror::Error;

/// Errors returned by [`PromptRunner`] operations.
#[derive(Debug, Error)]
pub enum PromptError {
    /// The underlying ACP agent protocol returned an error.
    #[error("agent protocol error: {0}")]
    Agent(#[from] agent_client_protocol::Error),
}

/// Runs one-shot prompts against ACP components.
pub struct PromptRunner;

impl PromptRunner {
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
    /// use kid_agentic_coding::PromptRunner;
    /// use agent_client_protocol::schema::v1::{ContentBlock, TextContent};
    ///
    /// let block = ContentBlock::Text(TextContent::new("Hello".to_string()));
    /// assert_eq!(PromptRunner::content_block_to_string(&block), "Hello");
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
    /// # Errors
    ///
    /// Returns [`PromptError`] if the agent connection, initialization, or the
    /// prompt turn itself fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use kid_agentic_coding::PromptRunner;
    /// use agent_client_protocol::AcpAgent;
    /// use std::str::FromStr;
    ///
    /// # async fn example() -> Result<(), kid_agentic_coding::PromptError> {
    /// let agent = AcpAgent::from_str("python agent.py")?;
    /// PromptRunner::run_with_callback(agent, "What is 2+2?", async |block| {
    ///     print!("{}", PromptRunner::content_block_to_string(&block));
    /// }).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn run_with_callback(
        component: impl ConnectTo<Client>,
        prompt_text: impl ToString,
        mut callback: impl AsyncFnMut(ContentBlock) + Send,
    ) -> Result<(), PromptError> {
        let prompt_text = prompt_text.to_string();

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
            .connect_with(component, |cx: ConnectionTo<Agent>| async move {
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
                        SessionMessage::SessionMessage(message) => {
                            MatchDispatch::new(message)
                                .if_notification(async |notification: SessionNotification| {
                                    tracing::debug!(?notification, "received SessionNotification");
                                    if let SessionUpdate::AgentMessageChunk(content_chunk) =
                                        notification.update
                                    {
                                        callback(content_chunk.content).await;
                                    }
                                    Ok(())
                                })
                                .await
                                .if_request(async |request: RequestPermissionRequest, responder| {
                                    // Auto-approve all permission requests by selecting the first
                                    // option that looks "allow-ish"
                                    let outcome = request
                                        .options
                                        .iter()
                                        .find(|option| {
                                            matches!(
                                                option.kind,
                                                PermissionOptionKind::AllowOnce
                                                    | PermissionOptionKind::AllowAlways
                                            )
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
                        SessionMessage::StopReason(stop_reason) => match stop_reason {
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
                        },
                        _ => {}
                    }
                }

                Ok(())
            })
            .await?;

        Ok(())
    }

    /// Runs a single prompt against a component and returns the accumulated text response.
    ///
    /// This is a convenience wrapper around [`PromptRunner::run_with_callback`] that
    /// accumulates all content blocks into a single string.
    ///
    /// # Errors
    ///
    /// Returns [`PromptError`] under the same conditions as
    /// [`PromptRunner::run_with_callback`].
    ///
    /// # Example
    ///
    /// ```ignore
    /// use kid_agentic_coding::PromptRunner;
    /// use agent_client_protocol::AcpAgent;
    /// use std::str::FromStr;
    ///
    /// # async fn example() -> Result<(), kid_agentic_coding::PromptError> {
    /// let agent = AcpAgent::from_str("python agent.py")?;
    /// let response = PromptRunner::run(agent, "What is 2+2?").await?;
    /// assert!(response.contains("4"));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn run(
        component: impl ConnectTo<Client>,
        prompt_text: impl ToString,
    ) -> Result<String, PromptError> {
        let mut accumulated_text = String::new();
        Self::run_with_callback(component, prompt_text, async |block| {
            accumulated_text.push_str(&Self::content_block_to_string(&block));
        })
        .await?;
        Ok(accumulated_text)
    }

    /// Parses agent command-line arguments into an [`AcpAgent`].
    ///
    /// A single argument starting with `{` is parsed as a JSON configuration object;
    /// otherwise the arguments are treated as a command and its arguments.
    ///
    /// # Errors
    ///
    /// Returns [`PromptError`] if the arguments cannot be parsed as a valid agent
    /// configuration.
    pub fn parse_agent_args(agent_args: &[String]) -> Result<AcpAgent, PromptError> {
        let agent = match agent_args {
            [configuration] if configuration.trim_start().starts_with('{') => {
                configuration.parse()?
            }
            arguments => AcpAgent::from_args(arguments)?,
        };
        Ok(agent)
    }
}

#[cfg(test)]
mod parse_agent_args_tests {
    use super::PromptRunner;
    use std::path::Path;

    #[test]
    fn parses_json_agent_configuration() {
        let agent = PromptRunner::parse_agent_args(&[
            r#"{"command":"python","args":["agent.py"],"env":{"RUST_LOG":"debug"}}"#.to_owned(),
        ])
        .unwrap();

        assert_eq!(agent.config().command(), Path::new("python"));
        assert_eq!(agent.config().arguments(), ["agent.py"]);
        assert_eq!(
            agent
                .config()
                .environment()
                .get("RUST_LOG")
                .map(String::as_str),
            Some("debug")
        );
    }

    #[test]
    fn preserves_single_executable_path_with_spaces() {
        let agent = PromptRunner::parse_agent_args(&["/Applications/My Agent".to_owned()]).unwrap();

        assert_eq!(
            agent.config().command(),
            Path::new("/Applications/My Agent")
        );
        assert!(agent.config().arguments().is_empty());
    }
}
