//! Interactive, multi-turn ACP session management.
//!
//! Protocol-facing logic only; the channel plumbing consumers see lives in
//! [`crate::bridge`].

use crate::bridge::{CommitFixVerdict, SessionEvent, SessionHandle, next_session_id};
use crate::mcp;
use crate::mcp::{BridgeSockets, FsSocketDir, SocketFileGuard};
use crate::prompt::PromptRunner;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, InitializeRequest, NewSessionRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory, SessionNotification,
    SessionUpdate, ToolCall, ToolCallContent, ToolCallId, ToolCallLocation, ToolCallStatus,
    ToolCallUpdate, ToolKind,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{Agent, Client, ConnectTo, ConnectionTo, Error, SessionMessage};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use wire::{CommitFixDone, Confetti, VERDICT_TIMEOUT, WorkflowEvent};

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::future;
use std::path::{Path, PathBuf};

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
    fs_socket_dir: FsSocketDir,
    session_root: Option<PathBuf>,
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
        session_root,
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
#[allow(clippy::too_many_arguments)]
async fn run_session(
    component: impl ConnectTo<Client> + 'static,
    mut cancel_rx: UnboundedReceiver<()>,
    mut prompt_rx: UnboundedReceiver<String>,
    event_tx: UnboundedSender<SessionEvent>,
    disable_confetti: bool,
    workflow_name: Option<String>,
    fs_socket_dir: FsSocketDir,
    session_root: Option<PathBuf>,
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
            let supports_mcp = mcp::supports_mcp(&init_response);
            let mut confetti_listener: Option<UnixListener> = None;
            #[allow(unused_assignments)]
            let mut workflow_listener: Option<UnixListener> = None;
            let mut confetti_socket_guard = SocketFileGuard::new(None);
            let mut workflow_socket_guard = SocketFileGuard::new(None);

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

            let confetti_socket = if workflow_name.is_none() && !disable_confetti && !supports_mcp {
                match fs_socket_dir.as_ref() {
                    Some(directory) => {
                        let socket_name = mcp::confetti_socket_name();
                        let path = mcp::fs_socket_path(directory, &socket_name);
                        let identifier = path.display().to_string();
                        confetti_socket_guard = SocketFileGuard::new(Some(path));
                        Some(identifier)
                    }
                    None => Some(mcp::confetti_socket_name()),
                }
            } else {
                None
            };

            if let Some(directory) = fs_socket_dir.as_ref() {
                tracing::info!(
                    dir = %directory.display(),
                    "bridge sockets use the filesystem fallback; a sandboxed agent can only reach them if this directory is mounted writable into the sandbox"
                );
            }
            let session_root = session_root
                .clone()
                .unwrap_or_else(|| PathBuf::from(SESSION_ROOT));

            let mut session = if workflow_name.is_some() {
                let sockets = BridgeSockets::new(workflow_socket, None)
                    .bind_workflow()
                    .map_err(Error::into_internal_error)?;
                let servers = sockets
                    .stdio_mcp_servers_for_fix_session(&session_root)
                    .map_err(Error::into_internal_error)?;
                let (workflow_std_listener, _) = sockets
                    .into_listeners()
                    .map_err(Error::into_internal_error)?;
                workflow_listener = Some(
                    UnixListener::from_std(workflow_std_listener)
                        .map_err(Error::into_internal_error)?,
                );
                match cx.build_session_from(
                    NewSessionRequest::new(session_root.clone()).mcp_servers(servers),
                )
                .block_task()
                .start_session()
                .await
                {
                    Ok(session) => session,
                    Err(err) => {
                        tracing::error!(?err, "fix-session tool registration failed");
                        cx.build_session(session_root.clone())
                            .block_task()
                            .start_session()
                            .await?
                    }
                }
            } else if disable_confetti {
                let sockets = BridgeSockets::new(workflow_socket, None)
                    .bind_workflow()
                    .map_err(Error::into_internal_error)?;
                let servers = sockets
                    .stdio_mcp_servers_without_confetti(Path::new(SESSION_ROOT))
                    .map_err(Error::into_internal_error)?;
                let (workflow_std_listener, _) = sockets
                    .into_listeners()
                    .map_err(Error::into_internal_error)?;
                workflow_listener = Some(
                    UnixListener::from_std(workflow_std_listener)
                        .map_err(Error::into_internal_error)?,
                );
                cx.build_session_from(
                    NewSessionRequest::new(PathBuf::from(SESSION_ROOT)).mcp_servers(servers),
                )
                .block_task()
                .start_session()
                .await?
            } else if supports_mcp {
                let sockets = BridgeSockets::new(workflow_socket, None)
                    .bind_workflow()
                    .map_err(Error::into_internal_error)?;
                let servers = sockets
                    .stdio_mcp_servers_without_confetti(Path::new(SESSION_ROOT))
                    .map_err(Error::into_internal_error)?;
                let (workflow_std_listener, _) = sockets
                    .into_listeners()
                    .map_err(Error::into_internal_error)?;
                workflow_listener = Some(
                    UnixListener::from_std(workflow_std_listener)
                        .map_err(Error::into_internal_error)?,
                );
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
                let sockets = BridgeSockets::new(workflow_socket, confetti_socket)
                    .bind_workflow()
                    .map_err(Error::into_internal_error)?
                    .bind_confetti()
                    .map_err(Error::into_internal_error)?;
                let servers = sockets
                    .stdio_mcp_servers(Path::new(SESSION_ROOT))
                    .map_err(Error::into_internal_error)?;
                let (workflow_std_listener, confetti_std_listener) = sockets
                    .into_listeners()
                    .map_err(Error::into_internal_error)?;
                workflow_listener = Some(
                    UnixListener::from_std(workflow_std_listener)
                        .map_err(Error::into_internal_error)?,
                );
                confetti_listener = Some(
                    UnixListener::from_std(confetti_std_listener)
                        .map_err(Error::into_internal_error)?,
                );
                cx.build_session_from(
                    NewSessionRequest::new(PathBuf::from(SESSION_ROOT)).mcp_servers(servers),
                )
                .block_task()
                .start_session()
                .await?
            };

            let mut turn_active = false;

            let mut pending_commit_fix_done: Option<String> = None;

            // Kinds of announced but not yet settled tool calls, so
            // `tool_call_result` keeps scoping file-read output when an
            // update doesn't repeat the kind.
            let mut tool_call_kinds: HashMap<ToolCallId, ToolKind> = HashMap::new();

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
                                && Confetti::parse(&message).is_some()
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
                            if stream.read_to_end(&mut message).await.is_ok() {
                                match WorkflowEvent::parse(&message) {
                                    Ok(WorkflowEvent::CommitFix(request)) => {
                                        tracing::info!(
                                            amend = request.amend,
                                            tldr = %request.tldr,
                                            why = %request.why,
                                            what = %request.what,
                                            "commit-fix session event received"
                                        );
                                        let (verdict_tx, verdict_rx) = oneshot::channel();
                                        let event = SessionEvent::CommitFix {
                                            request,
                                            verdict: verdict_tx,
                                        };
                                        if session_event_tx.send(event).is_ok() {
                                            tokio::spawn(dispatch_commit_fix(stream, verdict_rx));
                                        } else {
                                            let _ = send_workflow_ack(
                                                &mut stream,
                                                &CommitFixVerdict::Rejected {
                                                    reason: "session is shutting down".to_owned(),
                                                },
                                            )
                                            .await;
                                        }
                                    }
                                    Ok(WorkflowEvent::CommitFixDone(CommitFixDone { commit_message })) => {
                                        tracing::info!(%commit_message, "commit-fix-done session event received");
                                        if turn_active {
                                            pending_commit_fix_done = Some(commit_message);
                                        } else {
                                            let _ = session_event_tx
                                                .send(SessionEvent::CommitFixDone { commit_message });
                                        }
                                    }
                                    Err(reason) => {
                                        tracing::warn!(%reason, "rejecting workflow event");
                                        let _ = send_workflow_ack(
                                            &mut stream,
                                            &CommitFixVerdict::Rejected { reason },
                                        )
                                        .await;
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
                        handle_update(update, &session_event_tx, &mut tool_call_kinds).await?;
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

async fn send_workflow_ack(
    stream: &mut tokio::net::UnixStream,
    verdict: &CommitFixVerdict,
) -> std::io::Result<()> {
    let line = verdict.to_line().map_err(std::io::Error::other)?;
    stream.write_all(format!("{line}\n").as_bytes()).await
}

/// Waits for the UI's verdict on a commit-fix request and writes it back to
/// the requesting bridge connection. Runs in its own task because the
/// verdict is decided by the UI loop, which would be blocked while the
/// session loop's select waits here.
async fn dispatch_commit_fix(
    mut stream: tokio::net::UnixStream,
    verdict_rx: oneshot::Receiver<CommitFixVerdict>,
) {
    let verdict = match tokio::time::timeout(VERDICT_TIMEOUT, verdict_rx).await {
        Ok(Ok(verdict)) => verdict,
        Ok(Err(_)) => CommitFixVerdict::Rejected {
            reason: "session is shutting down".to_owned(),
        },
        Err(_) => CommitFixVerdict::Rejected {
            reason: "the session did not answer the commit-fix request in time".to_owned(),
        },
    };
    let _ = send_workflow_ack(&mut stream, &verdict).await;
    // The requesting bridge connection reads until EOF; dropping the stream
    // sends it.
    drop(stream);
}

/// Dispatches one session update: forwards message chunks and permission
/// requests to the event channel, and reports stop reasons without ending
/// the session.
async fn handle_update(
    update: SessionMessage,
    event_tx: &UnboundedSender<SessionEvent>,
    tool_call_kinds: &mut HashMap<ToolCallId, ToolKind>,
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
                            locations,
                            ..
                        }) => {
                            tool_call_kinds.insert(tool_call_id.clone(), kind);
                            let target = tool_call_target(kind, &locations, raw_input.as_ref());
                            let _ = event_tx.send(SessionEvent::ToolCall {
                                id: tool_call_id,
                                title: tool_call_title(kind, title, target.as_deref()),
                                status,
                                parameters: raw_input.map(|value| value.to_string()),
                                result: tool_call_result(
                                    Some(kind),
                                    Some(status),
                                    &content,
                                    raw_output.as_ref(),
                                ),
                            });
                        }

                        SessionUpdate::ToolCallUpdate(ToolCallUpdate {
                            tool_call_id,
                            fields,
                            ..
                        }) => {
                            let kind = fields
                                .kind
                                .or_else(|| tool_call_kinds.get(&tool_call_id).copied());
                            let result = tool_call_result(
                                kind,
                                fields.status,
                                fields.content.as_deref().unwrap_or(&[]),
                                fields.raw_output.as_ref(),
                            );
                            if matches!(
                                fields.status,
                                Some(ToolCallStatus::Completed | ToolCallStatus::Failed)
                            ) {
                                tool_call_kinds.remove(&tool_call_id);
                            }
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
                    let (title, parameters) = permission_request_details(&request.tool_call);
                    let _ = event_tx.send(SessionEvent::PermissionRequest {
                        tool_call_id: request.tool_call.tool_call_id,
                        title,
                        parameters,
                        options: request.options,
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

/// Labels command executions while preserving the command in the title, and
/// appends the target file when a file operation's title doesn't name the
/// file it operates on.
fn tool_call_title(kind: ToolKind, title: String, target: Option<&str>) -> String {
    let title = if kind == ToolKind::Execute {
        format!("Shell command: {title}")
    } else {
        title
    };
    let Some(target) = target else {
        return title;
    };
    let file_name = Path::new(target).file_name().and_then(OsStr::to_str);
    if title.contains(target) || file_name.is_some_and(|name| title.contains(name)) {
        return title;
    }
    format!("{title} ({target})")
}

/// The file a file operation operates on: the first ACP location, else the
/// `path`/`file_path` field of the raw input. `None` for other tool kinds.
fn tool_call_target(
    kind: ToolKind,
    locations: &[ToolCallLocation],
    raw_input: Option<&Value>,
) -> Option<String> {
    let is_file_operation = matches!(
        kind,
        ToolKind::Read | ToolKind::Edit | ToolKind::Delete | ToolKind::Move | ToolKind::Search
    );
    if !is_file_operation {
        return None;
    }
    if let Some(location) = locations.first() {
        return Some(location.path.display().to_string());
    }
    let raw_input = raw_input?;
    ["path", "file_path"]
        .iter()
        .find_map(|key| raw_input.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

/// Extracts the display title and raw input parameters of the tool call a
/// permission request refers to.
fn permission_request_details(tool_call: &ToolCallUpdate) -> (String, Option<String>) {
    let fields = &tool_call.fields;
    let title = fields
        .title
        .clone()
        .unwrap_or_else(|| "Tool call".to_owned());
    let parameters = fields.raw_input.as_ref().map(|value| value.to_string());
    (title, parameters)
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
/// provided. Returns `None` when the tool call carries no result yet. A
/// successful file read is reduced to its line count so file contents
/// don't dump into the chat; other results are shown verbatim.
fn tool_call_result(
    kind: Option<ToolKind>,
    status: Option<ToolCallStatus>,
    content: &[ToolCallContent],
    raw_output: Option<&Value>,
) -> Option<String> {
    if kind == Some(ToolKind::Read)
        && matches!(status, Some(ToolCallStatus::Completed))
        && !content.is_empty()
        && content
            .iter()
            .all(|item| matches!(item, ToolCallContent::Content(_)))
    {
        let text = content
            .iter()
            .filter_map(|item| match item {
                ToolCallContent::Content(content) => {
                    Some(PromptRunner::content_block_to_string(&content.content))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let lines = text.lines().count();
        return Some(match lines {
            1 => "1 line".to_owned(),
            _ => format!("{lines} lines"),
        });
    }

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
    use super::{permission_request_details, tool_call_result, tool_call_target, tool_call_title};
    use agent_client_protocol::schema::v1::{
        ContentBlock, TextContent, ToolCallContent, ToolCallLocation, ToolCallStatus,
        ToolCallUpdate, ToolCallUpdateFields, ToolKind,
    };

    fn text_content(text: &str) -> ToolCallContent {
        ToolCallContent::from(ContentBlock::Text(TextContent::new(text)))
    }

    #[test]
    fn shell_commands_include_the_command_in_the_title() {
        assert_eq!(
            tool_call_title(ToolKind::Execute, "git log --oneline".to_owned(), None),
            "Shell command: git log --oneline"
        );
    }

    #[test]
    fn non_shell_tool_titles_are_preserved() {
        assert_eq!(
            tool_call_title(ToolKind::Read, "Read app/src/ui.rs".to_owned(), None),
            "Read app/src/ui.rs"
        );
    }

    #[test]
    fn file_operation_titles_get_the_target_file_appended() {
        assert_eq!(
            tool_call_title(
                ToolKind::Edit,
                "Edit file".to_owned(),
                Some("app/src/ui.rs")
            ),
            "Edit file (app/src/ui.rs)"
        );
    }

    #[test]
    fn file_operation_titles_are_not_duplicated_when_the_file_is_named() {
        assert_eq!(
            tool_call_title(
                ToolKind::Read,
                "Read app/src/ui.rs".to_owned(),
                Some("app/src/ui.rs"),
            ),
            "Read app/src/ui.rs"
        );
        assert_eq!(
            tool_call_title(
                ToolKind::Read,
                "Edit ui.rs".to_owned(),
                Some("app/src/ui.rs")
            ),
            "Edit ui.rs"
        );
    }

    #[test]
    fn tool_target_prefers_locations_over_raw_input() {
        let locations = [ToolCallLocation::new("app/src/ui.rs")];
        assert_eq!(
            tool_call_target(
                ToolKind::Read,
                &locations,
                Some(&serde_json::json!({"path": "other.rs"})),
            ),
            Some("app/src/ui.rs".to_owned())
        );
    }

    #[test]
    fn tool_target_falls_back_to_raw_input() {
        assert_eq!(
            tool_call_target(
                ToolKind::Edit,
                &[],
                Some(&serde_json::json!({"file_path": "app/src/main.rs"})),
            ),
            Some("app/src/main.rs".to_owned())
        );
    }

    #[test]
    fn tool_target_is_none_outside_file_operations() {
        assert_eq!(
            tool_call_target(
                ToolKind::Execute,
                &[],
                Some(&serde_json::json!({"path": "app/src/ui.rs"})),
            ),
            None
        );
        assert_eq!(tool_call_target(ToolKind::Read, &[], None), None);
    }

    #[test]
    fn successful_read_results_are_reduced_to_line_counts() {
        assert_eq!(
            tool_call_result(
                Some(ToolKind::Read),
                Some(ToolCallStatus::Completed),
                &[text_content("fn main() {}")],
                None,
            ),
            Some("1 line".to_owned())
        );
        assert_eq!(
            tool_call_result(
                Some(ToolKind::Read),
                Some(ToolCallStatus::Completed),
                &[text_content("a\nb\nc")],
                None,
            ),
            Some("3 lines".to_owned())
        );
    }

    #[test]
    fn failed_read_results_keep_the_full_output() {
        assert_eq!(
            tool_call_result(
                Some(ToolKind::Read),
                Some(ToolCallStatus::Failed),
                &[text_content("error: no such file")],
                None,
            ),
            Some("error: no such file".to_owned())
        );
    }

    #[test]
    fn non_read_results_are_shown_verbatim() {
        assert_eq!(
            tool_call_result(
                Some(ToolKind::Execute),
                Some(ToolCallStatus::Completed),
                &[text_content("done")],
                None,
            ),
            Some("done".to_owned())
        );
    }

    #[test]
    fn results_fall_back_to_pretty_printed_raw_output() {
        assert_eq!(
            tool_call_result(
                Some(ToolKind::Execute),
                Some(ToolCallStatus::Completed),
                &[],
                Some(&serde_json::json!({"ok": true})),
            ),
            Some("{\n  \"ok\": true\n}".to_owned())
        );
    }

    #[test]
    fn permission_details_extract_title_and_raw_input() {
        let tool_call = ToolCallUpdate::new(
            "tool-call-1",
            ToolCallUpdateFields::new()
                .title("Read app/src/ui.rs")
                .raw_input(serde_json::json!({"path": "app/src/ui.rs"})),
        );

        assert_eq!(
            permission_request_details(&tool_call),
            (
                "Read app/src/ui.rs".to_owned(),
                Some(r#"{"path":"app/src/ui.rs"}"#.to_owned())
            )
        );
    }

    #[test]
    fn permission_details_fall_back_for_bare_tool_calls() {
        let tool_call = ToolCallUpdate::new("tool-call-2", ToolCallUpdateFields::new());

        assert_eq!(
            permission_request_details(&tool_call),
            ("Tool call".to_owned(), None)
        );
    }
}
