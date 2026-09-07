//! Fake ACP agent that answers every prompt with Lorem Ipsum text instead
//! of calling a real LLM, so downstream code can be tested without tokens.

mod lorem;

use clap::Parser;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentNotification, CancelNotification, ConnectMcpRequest, ContentBlock, ContentChunk,
    InitializeRequest, InitializeResponse, McpServer, McpServerAcpId, McpServerStdio,
    MessageMcpNotification, MessageMcpRequest, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, SessionId, SessionNotification, SessionUpdate, StopReason,
    TextContent, ToolCall, ToolCallId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{
    Agent, Client, ConnectionTo, Error, ErrorCode, Stdio, on_receive_notification,
    on_receive_request,
};
use color_eyre::eyre::eyre;
use color_eyre::{Report, Result};
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
use serde_json::json;
use serde_json::{Map, Value};
use tokio::process::Command;
use tokio::time::sleep;

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Parser)]
struct Args {
    /// Fails the prompt request to exercise the session error path.
    #[arg(long)]
    fail_session: bool,

    /// Exits the agent process to exercise the connection loss path.
    #[arg(long)]
    crash: bool,

    /// Completes the first tool call of each prompt as `Failed` with a
    /// failure message, to exercise the failure-result display path.
    #[arg(long)]
    fail_tool: bool,

    /// Advertises MCP-over-ACP support in the initialize response, so a
    /// connecting client attempts confetti MCP tool registration. Active
    /// by default; pass this flag to opt out.
    #[arg(long = "disable-mcp")]
    disable_mcp: bool,
}

/// Number of words the fake agent replies with per prompt.
const REPLY_WORD_COUNT: usize = 48;

/// How long the fake agent "thinks" before its first chunk, so a client
/// polling for a running state actually has something to observe.
const THINKING_DELAY: Duration = Duration::from_millis(500);

/// How long a fake tool call stays `InProgress` before completing.
const TOOL_CALL_DELAY: Duration = Duration::from_millis(700);

/// Assigns increasing session ids; a single fake-agent process may serve
/// several `session/new` calls over its lifetime.
static NEXT_SESSION_ID: AtomicUsize = AtomicUsize::new(0);

/// Assigns increasing seeds so consecutive prompts don't all echo the same
/// Lorem Ipsum sentence.
static NEXT_PROMPT_SEED: AtomicUsize = AtomicUsize::new(0);

/// Assigns increasing indices so the fake agent can vary its thought/tool
/// call plan per request instead of repeating the same simple round-trip.
static NEXT_PROMPT_INDEX: AtomicUsize = AtomicUsize::new(0);
static COMMIT_WORKFLOW_SERVER: OnceLock<McpServerStdio> = OnceLock::new();

static CONFETTI_SERVER_ID: OnceLock<McpServerAcpId> = OnceLock::new();

static CANCELLED: AtomicBool = AtomicBool::new(false);
static SESSION_MCP_SERVERS: OnceLock<Mutex<HashMap<SessionId, SessionState>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionKind {
    Main,
    Fix,
}

struct SessionState {
    kind: SessionKind,
    servers: Vec<McpServer>,
}

fn session_kind(servers: &[McpServer]) -> SessionKind {
    if servers.iter().any(|server| {
        matches!(
            server,
            McpServer::Stdio(server) if server.name == "git-commit-fix-close-tools"
        )
    }) {
        SessionKind::Fix
    } else {
        SessionKind::Main
    }
}

type McpToolInvoker = fn(
    McpToolContext,
    Option<&'static str>,
) -> Pin<Box<dyn Future<Output = Result<String>> + Send>>;

struct McpToolCall {
    name: &'static str,
    argument: Option<&'static str>,
    invoke: McpToolInvoker,
}

struct McpToolContext {
    connection: ConnectionTo<Client>,
    rust_server: Option<McpServerStdio>,
    close_server: Option<McpServerStdio>,
}

enum Step {
    Thought(&'static str),
    SimulatedToolCall(&'static str),
    McpToolCall(McpToolCall),
}

/// The thought/tool-call plan for the `prompt_index`-th prompt in a
/// session. Cycles every three requests: a first request keeps the
/// original single thought + single tool call; the second interleaves a
/// couple of tool calls with thoughts; the third runs more than three
/// tool calls so the inline UI's live tail (last three steps) and its
/// truncation marker actually have something to show while the turn is
/// still in progress. Since a thought no longer ends the cluster it
/// belongs to (only a following user/agent message does), every step in
/// one plan renders as a single growing tool cluster.
fn plan_for(prompt_index: usize, supports_mcp: bool) -> Vec<Step> {
    let mut steps = match prompt_index % 3 {
        0 => vec![
            Step::Thought(
                "Generating a deliberately long lorem ipsum response so the live UI has enough content to demonstrate line wrapping clearly",
            ),
            Step::SimulatedToolCall("generate_lorem_ipsum"),
        ],
        1 => vec![
            Step::Thought(
                "Breaking the request into several carefully chosen steps while keeping enough descriptive detail to exercise the live text wrapping behavior",
            ),
            Step::SimulatedToolCall("search_files"),
            Step::SimulatedToolCall("read_file"),
            Step::Thought(
                "Cross-checking the findings against the surrounding context to make sure the rendered cluster remains readable while new chunks continue arriving",
            ),
            Step::SimulatedToolCall("list_directory"),
            Step::McpToolCall(McpToolCall {
                name: "Git Commit With Fix",
                argument: Some("feat(lorem-agent): demonstrate commit fix workflow"),
                invoke: invoke_commit_workflow,
            }),
        ],
        _ => vec![
            Step::Thought(
                "Exploring several possible approaches and intentionally producing enough reasoning text to make incremental rendering and wrapping visible in a narrow terminal",
            ),
            Step::SimulatedToolCall("grep_codebase"),
            Step::SimulatedToolCall("read_file"),
            Step::Thought(
                "Narrowing down the relevant files while preserving a long enough live thought stream to expose wrapping, indentation, and cluster growth",
            ),
            Step::SimulatedToolCall("read_file"),
            Step::Thought(
                "Verifying edge cases and checking that the final rendered output stays legible even when long messages and tool details arrive in many small chunks",
            ),
            Step::SimulatedToolCall("write_file"),
        ],
    };
    if supports_mcp && prompt_index % 3 == 2 {
        steps.push(Step::SimulatedToolCall("bash"));
        steps.push(Step::McpToolCall(McpToolCall {
            name: "confetti",
            argument: None,
            invoke: invoke_confetti_tool,
        }));
    }
    steps
}

/// The fixed thought/tool-call plan for a commit-fix session: investigate,
/// re-run the Rust checks, then call the close server's git_commit, which
/// both commits and closes the session. Unlike `plan_for`, this never
/// cycles - a fix session only ever gets a single seed prompt.
fn commit_fix_session_plan() -> Vec<Step> {
    vec![
        Step::Thought(
            "Investigating the failing checks reported by the main session and applying a fix before committing",
        ),
        Step::McpToolCall(McpToolCall {
            name: "Rust Check",
            argument: Some("rust_check"),
            invoke: invoke_rust_tool,
        }),
        Step::McpToolCall(McpToolCall {
            name: "Rust Lint",
            argument: Some("rust_lint"),
            invoke: invoke_rust_tool,
        }),
        Step::McpToolCall(McpToolCall {
            name: "Rust Test",
            argument: Some("rust_test"),
            invoke: invoke_rust_tool,
        }),
        Step::McpToolCall(McpToolCall {
            name: "Git Commit",
            argument: Some("feat(lorem-agent): demonstrate commit fix workflow"),
            invoke: invoke_git_commit_tool,
        }),
    ]
}

async fn run_confetti(connection: ConnectionTo<Client>, server_id: McpServerAcpId) -> Result<()> {
    let response = connection
        .send_request(ConnectMcpRequest::new(server_id))
        .block_task()
        .await?;
    let connection_id = response.connection_id;
    let mut initialize_params = Map::new();
    initialize_params.insert("protocolVersion".into(), json!("2025-11-25"));
    initialize_params.insert("capabilities".into(), json!({}));
    initialize_params.insert(
        "clientInfo".into(),
        json!({"name": "lorem-agent", "version": env!("CARGO_PKG_VERSION")}),
    );
    connection
        .send_request(
            MessageMcpRequest::new(connection_id.clone(), "initialize").params(initialize_params),
        )
        .block_task()
        .await?;
    connection.send_notification(MessageMcpNotification::new(
        connection_id.clone(),
        "notifications/initialized",
    ))?;

    let mut params = Map::new();
    params.insert("name".into(), json!("confetti"));
    params.insert("arguments".into(), json!({}));
    tracing::debug!("calling confetti MCP tool");
    connection
        .send_request(MessageMcpRequest::new(connection_id, "tools/call").params(params))
        .block_task()
        .await?;

    Ok(())
}

async fn run_commit_workflow(message: &str) -> Result<()> {
    let server = COMMIT_WORKFLOW_SERVER
        .get()
        .ok_or_else(|| eyre!("commit-workflow MCP server unavailable"))?;
    let mut command = Command::new(&server.command);
    tracing::debug!(%message, "invoking commit-workflow MCP tool");
    command.args(&server.args);

    let client = ().serve(TokioChildProcess::new(command)?).await?;
    client
        .call_tool(
            CallToolRequestParams::new("git_commit_with_fix").with_arguments(
                serde_json::json!({"message": message})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            ),
        )
        .await?;
    tracing::debug!("commit-workflow MCP tool completed");
    client.cancel().await?;
    Ok(())
}
async fn run_git_commit_fix_close(server: &McpServerStdio, message: &str) -> Result<()> {
    let mut command = Command::new(&server.command);
    tracing::debug!(%message, "invoking git-commit-fix-close MCP tool");
    command.args(&server.args);

    let client = ().serve(TokioChildProcess::new(command)?).await?;
    client
        .call_tool(
            CallToolRequestParams::new("git_commit").with_arguments(
                serde_json::json!({"message": message})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            ),
        )
        .await?;
    tracing::debug!("git-commit-fix-close MCP tool completed");
    client.cancel().await?;
    Ok(())
}

async fn run_rust_tool(server: &McpServerStdio, tool_name: &str) -> Result<()> {
    let mut command = Command::new(&server.command);
    tracing::debug!(%tool_name, "invoking rust-tools MCP tool");
    command.args(&server.args);

    let client = ().serve(TokioChildProcess::new(command)?).await?;
    client
        .call_tool(CallToolRequestParams::new(tool_name.to_owned()))
        .await?;
    tracing::debug!("rust-tools MCP tool completed");
    client.cancel().await?;
    Ok(())
}

fn invoke_rust_tool(
    context: McpToolContext,
    tool_name: Option<&'static str>,
) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> {
    Box::pin(async move {
        let server = context
            .rust_server
            .ok_or_else(|| eyre!("rust-tools MCP server unavailable"))?;
        let Some(tool_name) = tool_name else {
            return Err(eyre!("rust tool name unavailable"));
        };
        run_rust_tool(&server, tool_name).await?;
        Ok(format!("{tool_name}: ok"))
    })
}

fn invoke_commit_workflow(
    _context: McpToolContext,
    message: Option<&'static str>,
) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> {
    Box::pin(async move {
        let Some(message) = message else {
            return Err(eyre!("commit message unavailable"));
        };
        run_commit_workflow(message).await?;
        Ok("git_commit_with_fix: ok".to_owned())
    })
}

fn invoke_git_commit_tool(
    context: McpToolContext,
    message: Option<&'static str>,
) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> {
    Box::pin(async move {
        let server = context
            .close_server
            .ok_or_else(|| eyre!("git-commit-fix-close MCP server unavailable"))?;
        let Some(message) = message else {
            return Err(eyre!("commit message unavailable"));
        };
        run_git_commit_fix_close(&server, message).await?;
        Ok("git_commit: ok".to_owned())
    })
}

fn invoke_confetti_tool(
    context: McpToolContext,
    _argument: Option<&'static str>,
) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> {
    Box::pin(async move {
        let server_id = CONFETTI_SERVER_ID
            .get()
            .cloned()
            .ok_or_else(|| eyre!("confetti MCP server unavailable"))?;
        run_confetti(context.connection, server_id).await?;
        Ok("confetti: ok".to_owned())
    })
}

async fn list_acp_tools(
    connection: ConnectionTo<Client>,
    server_id: McpServerAcpId,
) -> Result<Vec<String>> {
    let response = connection
        .send_request(ConnectMcpRequest::new(server_id))
        .block_task()
        .await?;
    let connection_id = response.connection_id;

    let mut initialize_params = Map::new();
    initialize_params.insert("protocolVersion".into(), json!("2025-11-25"));
    initialize_params.insert("capabilities".into(), json!({}));
    initialize_params.insert(
        "clientInfo".into(),
        json!({"name": "lorem-agent", "version": env!("CARGO_PKG_VERSION")}),
    );
    connection
        .send_request(
            MessageMcpRequest::new(connection_id.clone(), "initialize").params(initialize_params),
        )
        .block_task()
        .await?;
    connection.send_notification(MessageMcpNotification::new(
        connection_id.clone(),
        "notifications/initialized",
    ))?;

    let response = connection
        .send_request(MessageMcpRequest::new(connection_id, "tools/list"))
        .block_task()
        .await?;
    let response: Value = serde_json::from_str(response.0.get())?;
    Ok(response["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect())
}

async fn list_registered_tools(
    connection: ConnectionTo<Client>,
    servers: Vec<McpServer>,
) -> Result<String> {
    let mut lines = vec!["Registered MCP tools:".to_owned()];
    for server in servers {
        match server {
            McpServer::Acp(server) => {
                let server_id = server.server_id.clone();
                match list_acp_tools(connection.clone(), server_id.clone()).await {
                    Ok(tools) => lines.push(format!("- {}: {}", server_id, tools.join(", "))),
                    Err(error) => {
                        lines.push(format!("- {}: <failed to list tools: {error}>", server_id))
                    }
                }
            }
            McpServer::Stdio(server) => {
                let mut command = Command::new(&server.command);
                command.args(&server.args);
                match async {
                    let client = ().serve(TokioChildProcess::new(command)?).await?;
                    let tools = client.peer().list_all_tools().await?;
                    let names = tools
                        .into_iter()
                        .map(|tool| tool.name.to_string())
                        .collect::<Vec<_>>();
                    client.cancel().await?;
                    Ok::<_, Report>(names)
                }
                .await
                {
                    Ok(tools) => lines.push(format!("- {}: {}", server.name, tools.join(", "))),
                    Err(error) => lines.push(format!(
                        "- {}: <failed to list tools: {error}>",
                        server.name
                    )),
                }
            }
            _ => {}
        }
    }
    Ok(lines.join("\n"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    color_eyre::install()?;
    tracing_subscriber::fmt().with_writer(io::stderr).init();
    tracing::info!("agent logging initialized");

    Agent
        .builder()
        .on_receive_request(
            async |_request: InitializeRequest, responder, _cx| {
                let mut response = InitializeResponse::new(ProtocolVersion::V1);
                if !args.disable_mcp {
                    response.agent_capabilities.mcp_capabilities.acp = true;
                }
                responder.respond(response)
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async |request: NewSessionRequest, responder, _cx| {
                let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
                let session_id: SessionId = format!("lorem-session-{id}").into();

                let server_id = request.mcp_servers.iter().find_map(|server| match server {
                    McpServer::Acp(server) => Some(server.server_id.clone()),
                    _ => None,
                });
                if let (true, Some(server_id)) = (!args.disable_mcp, server_id) {
                    let _ = CONFETTI_SERVER_ID.set(server_id);
                }
                if let Some(server) = request.mcp_servers.iter().find_map(|server| match server {
                    McpServer::Stdio(server) if server.name == "git-commit-fix-open-tools" => {
                        Some(server.clone())
                    }
                    _ => None,
                }) {
                    let _ = COMMIT_WORKFLOW_SERVER.set(server);
                }

                SESSION_MCP_SERVERS
                    .get_or_init(|| Mutex::new(HashMap::new()))
                    .lock()
                    .map_err(|_| Error::from(ErrorCode::InternalError))?
                    .insert(
                        session_id.clone(),
                        SessionState {
                            kind: session_kind(&request.mcp_servers),
                            servers: request.mcp_servers.clone(),
                        },
                    );

                responder.respond(NewSessionResponse::new(session_id))
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            async |notification: CancelNotification, _cx| {
                CANCELLED.store(true, Ordering::Relaxed);
                tracing::debug!(
                    session_id = ?notification.session_id,
                    "session cancellation requested"
                );
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async |request: PromptRequest, responder, cx| {
                let prompt_index = NEXT_PROMPT_INDEX.fetch_add(1, Ordering::Relaxed);
                CANCELLED.store(false, Ordering::Relaxed);

                if args.fail_session {
                    return Err(Error::from(ErrorCode::InternalError));
                }
                if args.crash {
                    process::exit(1);
                }
                let supports_mcp = !args.disable_mcp;
                let fail_tool = args.fail_tool;

                let session_state = SESSION_MCP_SERVERS
                    .get()
                    .and_then(|sessions| sessions.lock().ok())
                    .and_then(|mut sessions| sessions.remove(&request.session_id));
                let session_kind = session_state
                    .as_ref()
                    .map_or(SessionKind::Main, |state| state.kind);
                let first_prompt_servers = session_state.map(|state| state.servers);

                let close_server = first_prompt_servers.as_ref().and_then(|servers| {
                    servers.iter().find_map(|server| match server {
                        McpServer::Stdio(server) if server.name == "git-commit-fix-close-tools" => {
                            Some(server.clone())
                        }
                        _ => None,
                    })
                });
                let rust_server = first_prompt_servers.as_ref().and_then(|servers| {
                    servers.iter().find_map(|server| match server {
                        McpServer::Stdio(server) if server.name == "rust-tools" => {
                            Some(server.clone())
                        }
                        _ => None,
                    })
                });

                let _ = cx.clone().spawn(async move {
                    if let Some(servers) = first_prompt_servers {
                        let tools = match list_registered_tools(cx.clone(), servers).await {
                            Ok(tools) => tools,
                            Err(error) => {
                                format!("Registered MCP tools: <failed to list tools: {error}>")
                            }
                        };
                        cx.send_notification(AgentNotification::SessionNotification(
                            SessionNotification::new(
                                request.session_id.clone(),
                                SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                    ContentBlock::Text(TextContent::new(tools)),
                                )),
                            ),
                        ))?;
                    }

                    let seed = NEXT_PROMPT_SEED.fetch_add(REPLY_WORD_COUNT, Ordering::Relaxed);
                    let text = lorem::generate(seed, REPLY_WORD_COUNT);
                    let steps = match session_kind {
                        SessionKind::Fix => commit_fix_session_plan(),
                        SessionKind::Main => plan_for(prompt_index, supports_mcp),
                    };
                    for (step_index, step) in steps.into_iter().enumerate() {
                        if CANCELLED.load(Ordering::Relaxed) {
                            responder.respond(PromptResponse::new(StopReason::Cancelled))?;
                            return Ok(());
                        }
                        match step {
                            Step::Thought(thought) => {
                                sleep(THINKING_DELAY).await;
                                // Emit thought in small word chunks to show live-append
                                let words: Vec<&str> = thought.split_whitespace().collect();
                                for chunk in words.chunks(2) {
                                    let chunk_text = chunk.join(" ");
                                    cx.send_notification(AgentNotification::SessionNotification(
                                        SessionNotification::new(
                                            request.session_id.clone(),
                                            SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                                                ContentBlock::Text(TextContent::new(chunk_text)),
                                            )),
                                        ),
                                    ))?;
                                    sleep(Duration::from_millis(100)).await;
                                }
                            }
                            Step::SimulatedToolCall(name) => {
                                let tool_call_id =
                                    ToolCallId::new(format!("lorem-tool-{seed}-{step_index}"));
                                cx.send_notification(AgentNotification::SessionNotification(
                                    SessionNotification::new(
                                        request.session_id.clone(),
                                        SessionUpdate::ToolCall(
                                            ToolCall::new(tool_call_id.clone(), name)
                                                .status(ToolCallStatus::InProgress)
                                                .raw_input(json!({
                                                    "tool": name,
                                                    "prompt_seed": seed,
                                                    "step_index": step_index
                                                })),
                                        ),
                                    ),
                                ))?;
                                sleep(TOOL_CALL_DELAY).await;
                                let (status, result_text) = if fail_tool && step_index == 0 {
                                    (ToolCallStatus::Failed, format!("{name}: simulated failure"))
                                } else {
                                    (
                                        ToolCallStatus::Completed,
                                        format!("{name}: ok ({} words)", REPLY_WORD_COUNT),
                                    )
                                };
                                cx.send_notification(AgentNotification::SessionNotification(
                                    SessionNotification::new(
                                        request.session_id.clone(),
                                        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                                            tool_call_id,
                                            ToolCallUpdateFields::new().status(status).content(
                                                vec![
                                                    ContentBlock::Text(TextContent::new(
                                                        result_text,
                                                    ))
                                                    .into(),
                                                ],
                                            ),
                                        )),
                                    ),
                                ))?;
                            }
                            Step::McpToolCall(call) => {
                                let tool_call_id =
                                    ToolCallId::new(format!("lorem-tool-{seed}-{step_index}"));
                                cx.send_notification(AgentNotification::SessionNotification(
                                    SessionNotification::new(
                                        request.session_id.clone(),
                                        SessionUpdate::ToolCall(
                                            ToolCall::new(tool_call_id.clone(), call.name)
                                                .status(ToolCallStatus::InProgress)
                                                .raw_input(
                                                    call.argument.map(
                                                        |argument| json!({"argument": argument}),
                                                    ),
                                                ),
                                        ),
                                    ),
                                ))?;
                                sleep(TOOL_CALL_DELAY).await;

                                let context = McpToolContext {
                                    connection: cx.clone(),
                                    rust_server: rust_server.clone(),
                                    close_server: close_server.clone(),
                                };
                                let (status, result_text) =
                                    match (call.invoke)(context, call.argument).await {
                                        Ok(result) => (ToolCallStatus::Completed, result),
                                        Err(error) => {
                                            tracing::debug!(
                                                ?error,
                                                tool = call.name,
                                                "MCP tool invocation failed"
                                            );
                                            (
                                                ToolCallStatus::Failed,
                                                format!("{}: failed", call.name),
                                            )
                                        }
                                    };
                                cx.send_notification(AgentNotification::SessionNotification(
                                    SessionNotification::new(
                                        request.session_id.clone(),
                                        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                                            tool_call_id,
                                            ToolCallUpdateFields::new().status(status).content(
                                                vec![
                                                    ContentBlock::Text(TextContent::new(
                                                        result_text,
                                                    ))
                                                    .into(),
                                                ],
                                            ),
                                        )),
                                    ),
                                ))?;
                            }
                        }
                    }

                    if CANCELLED.load(Ordering::Relaxed) {
                        responder.respond(PromptResponse::new(StopReason::Cancelled))?;
                        return Ok(());
                    }

                    // Emit speech in small word chunks to show live-append
                    let words: Vec<&str> = text.split_whitespace().collect();
                    for chunk in words.chunks(2) {
                        let chunk_text = chunk.join(" ");
                        cx.send_notification(AgentNotification::SessionNotification(
                            SessionNotification::new(
                                request.session_id.clone(),
                                SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                    ContentBlock::Text(TextContent::new(chunk_text)),
                                )),
                            ),
                        ))?;
                        sleep(Duration::from_millis(50)).await;
                    }

                    responder.respond(PromptResponse::new(StopReason::EndTurn))
                });

                Ok(())
            },
            on_receive_request!(),
        )
        .connect_to(Stdio::new())
        .await?;

    Ok(())
}

#[cfg(test)]
mod plan_for_tests {
    use super::{
        McpServer, McpServerStdio, McpToolCall, SessionKind, Step, commit_fix_session_plan,
        invoke_git_commit_tool, plan_for, session_kind,
    };

    #[test]
    fn session_kind_identifies_fix_session_from_close_server() {
        let servers = vec![McpServer::Stdio(McpServerStdio::new(
            "git-commit-fix-close-tools",
            std::path::PathBuf::from("git-commit-fix-close"),
        ))];
        assert_eq!(session_kind(&servers), SessionKind::Fix);
    }

    #[test]
    fn session_kind_keeps_main_session_without_close_server() {
        let servers = vec![McpServer::Stdio(McpServerStdio::new(
            "git-commit-fix-open-tools",
            std::path::PathBuf::from("git-commit-fix-open"),
        ))];
        assert_eq!(session_kind(&servers), SessionKind::Main);
    }

    #[test]
    fn fix_session_commit_uses_close_server_git_commit() {
        let steps = commit_fix_session_plan();
        let Some(Step::McpToolCall(call)) = steps.last() else {
            panic!("fix session must end with Git Commit");
        };
        let expected: super::McpToolInvoker = invoke_git_commit_tool;

        assert_eq!(call.name, "Git Commit");
        assert_eq!(
            call.argument,
            Some("feat(lorem-agent): demonstrate commit fix workflow")
        );
        assert!(std::ptr::fn_addr_eq(call.invoke, expected));
    }

    fn tool_call_names(steps: &[Step]) -> Vec<&'static str> {
        steps
            .iter()
            .filter_map(|step| match step {
                Step::SimulatedToolCall(name) => Some(*name),
                Step::McpToolCall(call) => Some(call.name),
                Step::Thought(_) => None,
            })
            .collect()
    }

    #[test]
    fn first_request_has_a_single_thought_and_tool_call() {
        let steps = plan_for(0, false);
        assert_eq!(tool_call_names(&steps), vec!["generate_lorem_ipsum"]);
        assert!(matches!(steps[0], Step::Thought(_)));
    }

    #[test]
    fn commit_fix_session_plan_ends_with_git_commit() {
        let steps = commit_fix_session_plan();
        assert_eq!(
            tool_call_names(&steps),
            vec!["Rust Check", "Rust Lint", "Rust Test", "Git Commit"]
        );

        assert!(matches!(steps[0], Step::Thought(_)));
    }

    #[test]
    fn second_request_includes_commit_fix_tool_call() {
        let steps = plan_for(1, false);
        assert_eq!(
            tool_call_names(&steps),
            vec![
                "search_files",
                "read_file",
                "list_directory",
                "Git Commit With Fix"
            ]
        );
    }

    #[test]
    fn third_request_with_mcp_includes_bash_before_confetti() {
        let steps = plan_for(2, true);
        assert!(matches!(
            steps[steps.len() - 2],
            Step::SimulatedToolCall("bash")
        ));
        assert!(matches!(
            steps.last(),
            Some(Step::McpToolCall(McpToolCall {
                name: "confetti",
                ..
            }))
        ));
    }

    #[test]
    fn second_request_has_two_consecutive_tool_calls() {
        let steps = plan_for(1, false);
        assert_eq!(
            tool_call_names(&steps),
            vec![
                "search_files",
                "read_file",
                "list_directory",
                "Git Commit With Fix",
            ]
        );
        // search_files and read_file are adjacent tool calls; since a
        // thought no longer ends the cluster, both this pair and
        // list_directory (after a thought) end up in the same cluster.
        assert!(matches!(steps[1], Step::SimulatedToolCall("search_files")));
        assert!(matches!(steps[2], Step::SimulatedToolCall("read_file")));
    }

    #[test]
    fn third_request_has_more_than_three_tool_calls() {
        let steps = plan_for(2, false);
        // More than the inline UI's 3-step live tail, so this request
        // exercises the truncation marker while the turn is still running.
        assert!(tool_call_names(&steps).len() > 3);
    }

    #[test]
    fn third_request_interleaves_thoughts_between_tool_calls() {
        let steps = plan_for(2, false);
        let thought_count = steps
            .iter()
            .filter(|step| matches!(step, Step::Thought(_)))
            .count();
        assert!(thought_count >= 2);
        assert!(matches!(steps[0], Step::Thought(_)));
    }

    #[test]
    fn plan_cycles_every_three_requests() {
        assert_eq!(
            tool_call_names(&plan_for(0, false)),
            tool_call_names(&plan_for(3, false))
        );
        assert_eq!(
            tool_call_names(&plan_for(1, false)),
            tool_call_names(&plan_for(4, false))
        );
    }
}
