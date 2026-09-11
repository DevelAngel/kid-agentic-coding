//! Interactive, multi-turn ACP session management.
//!
//! Protocol-facing logic only; the channel plumbing consumers see lives in
//! [`crate::bridge`].

use crate::bridge::{SessionEvent, SessionHandle, next_session_id};
use crate::mcp;
use crate::mcp::SocketFileGuard;
use crate::prompt::PromptRunner;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, InitializeRequest, NewSessionRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory, SessionNotification,
    SessionUpdate, ToolCall, ToolCallContent, ToolCallUpdate, ToolKind,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{Agent, Client, ConnectTo, ConnectionTo, Error, SessionMessage};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use std::fs;
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
    workflow_name: Option<String>,
    fs_socket_dir: Option<PathBuf>,
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
        workflow_name.clone(),
        fs_socket_dir,
    ));

    SessionHandle {
        prompt_tx,
        event_rx,
        cancel_tx,
        session_id: next_session_id(),
        workflow_name,
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
    workflow_name: Option<String>,
    fs_socket_dir: Option<PathBuf>,
) {
    // A sandboxed agent may have the fallback directory mounted into its
    // mount namespace when it starts, so the directory must exist before
    // the agent process is spawned, not when the sockets are bound.
    if let Some(directory) = fs_socket_dir.as_ref()
        && let Err(err) = fs::create_dir_all(directory)
    {
        let _ = event_tx.send(SessionEvent::Error(format!(
            "failed to create bridge socket directory '{}': {err}",
            directory.display()
        )));
        tracing::error!(?err, dir = %directory.display(), "failed to create bridge socket directory");
        return;
    }
    let session_event_tx = event_tx.clone();
    let result = Client
        .builder()
        .connect_with(component, |cx: ConnectionTo<Agent>| async move {
            let init_response = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let mut confetti_listener: Option<UnixListener> = None;
            #[allow(unused_assignments)]
            let mut workflow_listener: Option<UnixListener> = None;
            let mut confetti_socket_guard = SocketFileGuard::new(None);
            let mut workflow_socket_guard = SocketFileGuard::new(None);

            // Linux abstract-namespace sockets cannot cross a sandbox
            // boundary, so when a fallback directory is set the bridge
            // sockets live on filesystem paths inside it instead.
            let workflow_socket = match fs_socket_dir.as_ref() {
                Some(directory) => {
                    let socket_name = mcp::workflow_socket_name();
                    let path = mcp::fs_socket_path(directory, &socket_name);
                    let identifier = path.display().to_string();
                    workflow_socket_guard = SocketFileGuard::new(Some(path));
                    identifier
                }
                None => mcp::workflow_socket_name(),
            };
            if let Some(directory) = fs_socket_dir.as_ref() {
                tracing::info!(
                    dir = %directory.display(),
                    "bridge sockets use the filesystem fallback; a sandboxed agent can only reach them if this directory is mounted writable into the sandbox"
                );
            }
            let mut session = if workflow_name.is_some() {
                workflow_listener = Some(UnixListener::from_std(
                    mcp::bind_workflow_socket(&workflow_socket).map_err(Error::into_internal_error)?
                ).map_err(Error::into_internal_error)?);
                match mcp::stdio_mcp_servers_for_fix_session(&workflow_socket) {

                    Ok(servers) => cx
                        .build_session_from(
                            NewSessionRequest::new(PathBuf::from(SESSION_ROOT))
                                .mcp_servers(servers),
                        )
                        .block_task()
                        .start_session()
                        .await?,
                    Err(err) => {
                        tracing::error!(?err, "fix-session tool registration failed");
                        cx.build_session(PathBuf::from(SESSION_ROOT))
                            .block_task()
                            .start_session()
                            .await?
                    }
                }
            } else if disable_confetti {
                workflow_listener = Some(UnixListener::from_std(
                    mcp::bind_workflow_socket(&workflow_socket).map_err(Error::into_internal_error)?
                ).map_err(Error::into_internal_error)?);
                let servers = mcp::stdio_mcp_servers_without_confetti(&workflow_socket).map_err(Error::into_internal_error)?;
                cx.build_session_from(
                    NewSessionRequest::new(PathBuf::from(SESSION_ROOT)).mcp_servers(servers),
                )
                .block_task()
                .start_session()
                .await?
            } else if mcp::supports_mcp(&init_response) {
                workflow_listener = Some(UnixListener::from_std(
                    mcp::bind_workflow_socket(&workflow_socket).map_err(Error::into_internal_error)?
                ).map_err(Error::into_internal_error)?);
                let servers = mcp::stdio_mcp_servers_without_confetti(&workflow_socket).map_err(Error::into_internal_error)?;
                match cx
                    .build_session_from(
                        NewSessionRequest::new(PathBuf::from(SESSION_ROOT)).mcp_servers(servers),
                    )
                    .with_mcp_server(mcp::confetti_mcp_server(session_event_tx.clone()))
                {
                    Ok(builder) => builder.block_task().start_session().await?,
                    Err(err) => {
                        tracing::error!(?err, "confetti MCP tool registration via ACP failed");
                        cx.build_session(PathBuf::from(SESSION_ROOT))
                            .with_mcp_server(mcp::confetti_mcp_server(session_event_tx.clone()))
                            .map_err(Error::into_internal_error)?
                            .block_task()
                            .start_session()
                            .await?
                    }
                }
            } else {
                let confetti_socket = match fs_socket_dir.as_ref() {
                    Some(directory) => {
                        let socket_name = mcp::confetti_socket_name();
                        let path = mcp::fs_socket_path(directory, &socket_name);
                        let identifier = path.display().to_string();
                        confetti_socket_guard = SocketFileGuard::new(Some(path));
                        identifier
                    }
                    None => mcp::confetti_socket_name(),
                };
                confetti_listener = Some(UnixListener::from_std(
                    mcp::bind_confetti_socket(&confetti_socket).map_err(Error::into_internal_error)?
                ).map_err(Error::into_internal_error)?);
                workflow_listener = Some(UnixListener::from_std(
                    mcp::bind_workflow_socket(&workflow_socket).map_err(Error::into_internal_error)?
                ).map_err(Error::into_internal_error)?);
                let servers = mcp::stdio_mcp_servers(
                    &confetti_socket,
                    &workflow_socket,
                ) .map_err(Error::into_internal_error)?;
                cx.build_session_from(
                    NewSessionRequest::new(PathBuf::from(SESSION_ROOT)).mcp_servers(servers),
                )
                .block_task()
                .start_session()
                .await?
            };

            let mut turn_active = false;

            let mut pending_commit_fix_done: Option<String> = None;

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
                    workflow = async {
                        match workflow_listener.as_ref() {
                            Some(listener) => Some(listener.accept().await),
                            None => future::pending().await,
                        }
                    } => {
                        if let Some(Ok((mut stream, _))) = workflow {
                            tracing::debug!("commit-fix workflow event received");
                            let mut message = Vec::new();
                            if stream.read_to_end(&mut message).await.is_ok()
                                && let Ok(value) = serde_json::from_slice::<Value>(&message)
                            {
                                let event_name = value.get("event").and_then(Value::as_str);
                                let commit_message = value
                                    .get("commit_message")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned();
                                if event_name == Some(mcp::COMMIT_FIX_EVENT) {
                                    let instructions = value
                                        .get("instructions")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_owned();
                                    tracing::info!(%commit_message, "commit-fix session event received");
                                    let _ = session_event_tx.send(SessionEvent::CommitFix {
                                        instructions,
                                        commit_message,
                                    });
                                } else if event_name == Some(mcp::COMMIT_FIX_DONE_EVENT) {
                                    tracing::info!(%commit_message, "commit-fix-done session event received");
                                    if turn_active {
                                        pending_commit_fix_done = Some(commit_message);
                                    } else {
                                        let _ = session_event_tx.send(SessionEvent::CommitFixDone {
                                            commit_message,
                                        });
                                    }
                                }
                            }
                        }

                    }

                    update = session.read_update() => {
                        let update = update?;
                        let turn_stopped = matches!(&update, SessionMessage::StopReason(_));
                        if turn_stopped {
                            turn_active = false;
                            while cancel_rx.try_recv().is_ok() {}
                        }
                        handle_update(update, &session_event_tx).await?;
                        if turn_stopped && let Some(commit_message) = pending_commit_fix_done.take() {
                            let _ = session_event_tx.send(SessionEvent::CommitFixDone {
                                commit_message,
                            });
                        }
                    }
                }
            }

            // Removes the filesystem bridge socket files of the fallback
            // directory, if the session used one.
            drop(confetti_socket_guard);
            drop(workflow_socket_guard);

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
                            let _ = event_tx.send(SessionEvent::ToolCallUpdate {
                                id: tool_call_id,
                                status: fields.status,
                                parameters: fields.raw_input.map(|value| value.to_string()),
                                result,
                            });
                        }
                        SessionUpdate::ConfigOptionUpdate(update) => {
                            if let Some(model) = model_from_config_options(&update.config_options) {
                                let _ = event_tx.send(SessionEvent::ModelChanged(model));
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

/// Extracts the current value of the model selector, if the agent reports one.
fn model_from_config_options(config_options: &[SessionConfigOption]) -> Option<String> {
    config_options.iter().find_map(|option| {
        if option.category != Some(SessionConfigOptionCategory::Model) {
            return None;
        }
        match &option.kind {
            SessionConfigKind::Select(select) => Some(select.current_value.to_string()),
            _ => None,
        }
    })
}

/// Renders a tool call's result content into a display string, joining
/// standard content blocks, summarizing diffs and terminal embeds, and
/// falling back to pretty-printed `raw_output` when no content blocks were
/// provided. Returns `None` when the tool call carries no result yet.
fn tool_call_result(content: &[ToolCallContent], raw_output: Option<&Value>) -> Option<String> {
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
