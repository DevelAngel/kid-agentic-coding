//! Interactive, multi-turn ACP session management.
//!
//! Protocol-facing logic only; the channel plumbing consumers see lives in
//! [`crate::bridge`].

use crate::bridge::{SessionEvent, SessionHandle};
use crate::mcp;
use crate::prompt::PromptRunner;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, InitializeRequest, NewSessionRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionNotification, SessionUpdate, ToolCall, ToolCallContent, ToolCallStatus, ToolCallUpdate,
    ToolKind,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{Agent, Client, ConnectTo, ConnectionTo, Error, SessionMessage};
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use std::future;
use std::path::PathBuf;

/// Working directory the agent session operates in. `.` ties the session to
/// the current process's working directory.
const SESSION_ROOT: &str = ".";

/// Starts an interactive ACP session that stays open across multiple prompts.
///
/// Unlike [`crate::prompt_with_callback`], which runs a single turn and returns,
/// this spawns the agent connection as a background task and returns a
/// [`SessionHandle`] immediately. Send prompts and read [`SessionEvent`]s through
/// the handle for as long as needed; dropping the handle shuts the session down.
pub fn start_interactive_session(
    component: impl ConnectTo<Client> + 'static,
    disable_confetti: bool,
) -> SessionHandle {
    let (prompt_tx, prompt_rx) = mpsc::unbounded_channel::<String>();
    let (cancel_tx, cancel_rx) = mpsc::unbounded_channel::<()>();
    let (event_tx, event_rx) = mpsc::unbounded_channel::<SessionEvent>();

    tokio::spawn(run_session(
        component,
        cancel_rx,
        prompt_rx,
        event_tx,
        disable_confetti,
    ));

    SessionHandle {
        prompt_tx,
        event_rx,
        cancel_tx,
    }
}

/// Connects to the agent, initializes it, and relays prompts and updates
/// between the ACP session and the event channel until the handle is dropped.
async fn run_session(
    component: impl ConnectTo<Client> + 'static,
    mut cancel_rx: UnboundedReceiver<()>,
    mut prompt_rx: UnboundedReceiver<String>,
    event_tx: UnboundedSender<SessionEvent>,
    disable_confetti: bool,
) {
    let session_event_tx = event_tx.clone();
    let result = Client
        .builder()
        .connect_with(component, |cx: ConnectionTo<Agent>| async move {
            let init_response = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let mut confetti_listener: Option<UnixListener> = None;
            let mut session = if disable_confetti {
                tracing::info!("confetti MCP tool registration disabled");
                match mcp::stdio_mcp_servers_without_confetti() {
                    Ok(servers) => {
                        tracing::info!("registered rust-mcp and commit-workflow tools via stdio");
                        cx.build_session_from(
                            NewSessionRequest::new(PathBuf::from(SESSION_ROOT))
                                .mcp_servers(servers),
                        )
                        .block_task()
                        .start_session()
                        .await?
                    }
                    Err(err) => {
                        tracing::error!(
                            ?err,
                            "rust-mcp or commit-workflow tool registration via stdio failed"
                        );
                        cx.build_session(PathBuf::from(SESSION_ROOT))
                            .block_task()
                            .start_session()
                            .await?
                    }
                }
            } else if mcp::supports_mcp(&init_response) {
                match mcp::stdio_mcp_servers_without_confetti() {
                    Ok(servers) => match cx
                        .build_session_from(
                            NewSessionRequest::new(PathBuf::from(SESSION_ROOT))
                                .mcp_servers(servers),
                        )
                        .with_mcp_server(mcp::confetti_mcp_server(session_event_tx.clone()))
                    {
                        Ok(builder) => {
                            tracing::info!(
                                "registered confetti MCP tool via ACP and rust-mcp/commit-workflow tools via stdio"
                            );
                            builder.block_task().start_session().await?
                        }
                        Err(err) => {
                            tracing::error!(?err, "confetti MCP tool registration via ACP failed");
                            let servers =
                                mcp::stdio_mcp_servers_without_confetti().unwrap_or_default();
                            cx.build_session_from(
                                NewSessionRequest::new(PathBuf::from(SESSION_ROOT))
                                    .mcp_servers(servers),
                            )
                            .block_task()
                            .start_session()
                            .await?
                        }
                    },
                    Err(err) => {
                        tracing::error!(
                            ?err,
                            "rust-mcp or commit-workflow tool registration via stdio failed"
                        );
                        match cx
                            .build_session(PathBuf::from(SESSION_ROOT))
                            .with_mcp_server(mcp::confetti_mcp_server(session_event_tx.clone()))
                        {
                            Ok(builder) => {
                                tracing::info!("registered confetti MCP tool via ACP");
                                builder.block_task().start_session().await?
                            }
                            Err(err) => {
                                tracing::error!(
                                    ?err,
                                    "confetti MCP tool registration via ACP failed"
                                );
                                cx.build_session(PathBuf::from(SESSION_ROOT))
                                    .block_task()
                                    .start_session()
                                    .await?
                            }
                        }
                    }
                }
            } else {
                tracing::warn!("agent lacks MCP-over-ACP support");
                let socket_name = mcp::confetti_socket_name();
                match (
                    mcp::bind_confetti_socket(&socket_name),
                    mcp::stdio_mcp_servers(&socket_name),
                ) {
                    (Ok(listener), Ok(servers)) => {
                        tracing::info!(
                            "registered confetti, rust-mcp, and commit-workflow tools via stdio"
                        );
                        confetti_listener = Some(
                            UnixListener::from_std(listener).map_err(Error::into_internal_error)?,
                        );
                        cx.build_session_from(
                            NewSessionRequest::new(PathBuf::from(SESSION_ROOT))
                                .mcp_servers(servers),
                        )
                        .block_task()
                        .start_session()
                        .await?
                    }
                    (listener_result, servers_result) => {
                        tracing::error!(
                            ?listener_result,
                            ?servers_result,
                            "confetti, rust-mcp, or commit-workflow tool registration via stdio failed"
                        );
                        cx.build_session(PathBuf::from(SESSION_ROOT))
                            .block_task()
                            .start_session()
                            .await?
                    }
                }
            };
            let mut turn_active = false;

            loop {
                tokio::select! {
                    _ = cancel_rx.recv(), if turn_active => {
                        session
                            .connection()
                            .send_notification_to(
                                Agent,
                                CancelNotification::new(session.session_id().clone()),
                            )?;
                    }
                    prompt = prompt_rx.recv() => {
                        match prompt {
                            Some(text) => {
                                while cancel_rx.try_recv().is_ok() {}
                                session.send_prompt(text)?;
                                turn_active = true;
                            }
                            None => break,
                        }
                    }
                    bridge = async {
                        match confetti_listener.as_ref() {
                            Some(listener) => Some(listener.accept().await),
                            None => future::pending().await,
                        }
                    } => {
                        if let Some(Ok((mut stream, _))) = bridge {
                            let mut message = Vec::new();
                            if stream.read_to_end(&mut message).await.is_ok()
                                && message == b"confetti\n"
                            {
                                let _ = session_event_tx.send(SessionEvent::Confetti);
                            }
                        }
                    }
                    update = session.read_update() => {
                        let update = update?;
                        if matches!(&update, SessionMessage::StopReason(_)) {
                            turn_active = false;
                            while cancel_rx.try_recv().is_ok() {}
                        }
                        handle_update(update, &session_event_tx).await?;
                    }
                }
            }

            Ok(())
        })
        .await;

    if let Err(err) = result {
        let _ = event_tx.send(SessionEvent::Error(err.to_string()));
        tracing::error!(?err, "interactive session task ended with error");
    }
}

/// Dispatches one session update: forwards message chunks and permission
/// requests to the event channel, and reports stop reasons without ending
/// the session.
async fn handle_update(
    update: SessionMessage,
    event_tx: &UnboundedSender<SessionEvent>,
) -> Result<(), Error> {
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
                            kind,
                            status,
                            raw_input,
                            content,
                            raw_output,
                            ..
                        }) => {
                            let _ = event_tx.send(SessionEvent::ToolCall {
                                id: tool_call_id,
                                title: tool_call_title(kind, title),
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

                            // Check if this is a failed git_commit_with_check call
                            let is_fix_session_needed = fields.status
                                == Some(ToolCallStatus::Failed)
                                && extract_fix_session_data(
                                    fields.raw_input.as_ref(),
                                    fields.raw_output.as_ref(),
                                )
                                .is_some();

                            if is_fix_session_needed {
                                if let Some((commit_message, failed_step, stdout, stderr)) =
                                    extract_fix_session_data(
                                        fields.raw_input.as_ref(),
                                        fields.raw_output.as_ref(),
                                    )
                                {
                                    let _ = event_tx.send(SessionEvent::FixSessionNeeded {
                                        commit_message,
                                        failed_step,
                                        stdout,
                                        stderr,
                                    });
                                }
                            } else {
                                let _ = event_tx.send(SessionEvent::ToolCallUpdate {
                                    id: tool_call_id,
                                    status: fields.status,
                                    parameters: fields.raw_input.map(|value| value.to_string()),
                                    result,
                                });
                            }
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

/// Labels command executions while preserving the command in the title.
fn tool_call_title(kind: ToolKind, title: String) -> String {
    if kind == ToolKind::Execute {
        format!("Shell command: {title}")
    } else {
        title
    }
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

/// Checks if a tool call is a failed git_commit_with_check and extracts all needed data.
/// Returns Some((commit_message, failed_step, stdout, stderr)) if it is, None otherwise.
fn extract_fix_session_data(
    raw_input: Option<&serde_json::Value>,
    raw_output: Option<&serde_json::Value>,
) -> Option<(String, String, String, String)> {
    // Extract commit_message from parameters
    let input_obj = raw_input?.as_object()?;
    let commit_message = input_obj.get("message")?.as_str()?.to_string();

    // Extract failure details from output
    let output_obj = raw_output?.as_object()?;
    let failed_step = output_obj.get("failed_step")?.as_str()?.to_string();
    let stdout = output_obj.get("stdout")?.as_str()?.to_string();
    let stderr = output_obj.get("stderr")?.as_str()?.to_string();

    Some((commit_message, failed_step, stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::tool_call_title;
    use agent_client_protocol::schema::v1::ToolKind;

    #[test]
    fn shell_commands_include_the_command_in_the_title() {
        assert_eq!(
            tool_call_title(ToolKind::Execute, "git log --oneline".to_owned()),
            "Shell command: git log --oneline"
        );
    }

    #[test]
    fn non_shell_tool_titles_are_preserved() {
        assert_eq!(
            tool_call_title(ToolKind::Read, "Read app/src/ui.rs".to_owned()),
            "Read app/src/ui.rs"
        );
    }
}
