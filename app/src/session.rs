//! Interactive, multi-turn ACP session management.
//!
//! Protocol-facing logic only; the channel plumbing consumers see lives in
//! [`crate::bridge`].

use crate::bridge::{SessionEvent, SessionHandle};
use crate::prompt::PromptRunner;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    InitializeRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, SessionUpdate,
    ToolCall, ToolCallContent, ToolCallUpdate,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{
    Agent, Client, ConnectTo, ConnectionTo, Dispatch, Handled, SessionMessage, UntypedMessage,
};
use std::path::PathBuf;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

/// Working directory the agent session operates in. `.` ties the session to
/// the current process's working directory.
const SESSION_ROOT: &str = ".";

/// Starts an interactive ACP session that stays open across multiple prompts.
///
/// Unlike [`crate::prompt_with_callback`], which runs a single turn and returns,
/// this spawns the agent connection as a background task and returns a
/// [`SessionHandle`] immediately. Send prompts and read [`SessionEvent`]s through
/// the handle for as long as needed; dropping the handle shuts the session down.
pub fn start_interactive_session(component: impl ConnectTo<Client> + 'static) -> SessionHandle {
    let (prompt_tx, prompt_rx) = unbounded_channel::<String>();
    let (event_tx, event_rx) = unbounded_channel::<SessionEvent>();

    tokio::spawn(run_session(component, prompt_rx, event_tx));

    SessionHandle {
        prompt_tx,
        event_rx,
    }
}

/// Connects to the agent, initializes it, and relays prompts and updates
/// between the ACP session and the event channel until the handle is dropped.
async fn run_session(
    component: impl ConnectTo<Client> + 'static,
    mut prompt_rx: UnboundedReceiver<String>,
    event_tx: UnboundedSender<SessionEvent>,
) {
    let session_event_tx = event_tx.clone();
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
        .connect_with(component, |cx: ConnectionTo<Agent>| async move {
            let _init_response = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;

            let mut session = cx
                .build_session(PathBuf::from(SESSION_ROOT))
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
                        handle_update(update?, &session_event_tx).await?;
                    }
                }
            }

            Ok(())
        })
        .await;

    if let Err(err) = result {
        let _ = event_tx.send(SessionEvent::Error(err.to_string()));
        tracing::warn!(?err, "interactive session task ended with error");
    }
}

/// Dispatches one session update: forwards message chunks and permission
/// requests to the event channel, and reports stop reasons without ending
/// the session.
async fn handle_update(
    update: SessionMessage,
    event_tx: &UnboundedSender<SessionEvent>,
) -> Result<(), agent_client_protocol::Error> {
    match update {
        SessionMessage::SessionMessage(message) => {
            MatchDispatch::new(message)
                .if_notification(async |notification: SessionNotification| {
                    match notification.update {
                        SessionUpdate::AgentMessageChunk(content_chunk) => {
                            let _ =
                                event_tx.send(SessionEvent::Chunk(Box::new(content_chunk.content)));
                        }
                        SessionUpdate::AgentThoughtChunk(content_chunk) => {
                            let _ = event_tx
                                .send(SessionEvent::Thought(Box::new(content_chunk.content)));
                        }
                        SessionUpdate::ToolCall(ToolCall {
                            tool_call_id,
                            title,
                            status,
                            raw_input,
                            content,
                            raw_output,
                            ..
                        }) => {
                            let _ = event_tx.send(SessionEvent::ToolCall {
                                id: tool_call_id,
                                title,
                                status,
                                parameters: raw_input.map(|value| value.to_string()),
                                result: tool_call_result(&content, raw_output.as_ref()),
                            });
                        }

                        SessionUpdate::ToolCallUpdate(ToolCallUpdate {
                            tool_call_id,
                            fields,
                            ..
                        }) => {
                            let result = tool_call_result(
                                fields.content.as_deref().unwrap_or(&[]),
                                fields.raw_output.as_ref(),
                            );
                            let _ = event_tx.send(SessionEvent::ToolCallUpdate {
                                id: tool_call_id,
                                status: fields.status,
                                parameters: fields.raw_input.map(|value| value.to_string()),
                                result,
                            });
                        }
                        sn => {
                            tracing::debug!("{:?} dropped", sn);
                        }
                    }
                    Ok(())
                })
                .await
                .if_request(async |request: RequestPermissionRequest, responder| {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    let _ = event_tx.send(SessionEvent::PermissionRequest {
                        options: request.options.clone(),
                        reply: reply_tx,
                    });

                    let outcome = match reply_rx.await {
                        Ok(Some(option_id)) => RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new(option_id),
                        ),
                        _ => RequestPermissionOutcome::Cancelled,
                    };

                    responder.respond(RequestPermissionResponse::new(outcome))?;
                    Ok(())
                })
                .await
                .otherwise(async |_msg| Ok(()))
                .await?;
        }
        SessionMessage::StopReason(stop_reason) => {
            let _ = event_tx.send(SessionEvent::Stopped(stop_reason));
        }
        _ => {}
    }

    Ok(())
}

/// Renders a tool call's result content into a display string, joining
/// standard content blocks, summarizing diffs and terminal embeds, and
/// falling back to pretty-printed `raw_output` when no content blocks were
/// provided. Returns `None` when the tool call carries no result yet.
fn tool_call_result(
    content: &[ToolCallContent],
    raw_output: Option<&serde_json::Value>,
) -> Option<String> {
    let rendered: Vec<String> = content
        .iter()
        .map(|item| match item {
            ToolCallContent::Content(content) => {
                PromptRunner::content_block_to_string(&content.content)
            }
            ToolCallContent::Diff(diff) => format!("[diff: {}]", diff.path.display()),
            ToolCallContent::Terminal(terminal) => {
                format!("[terminal: {}]", terminal.terminal_id)
            }
            _ => "[unsupported content type]".to_owned(),
        })
        .collect();

    if !rendered.is_empty() {
        return Some(rendered.join("\n"));
    }

    raw_output
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()))
}
