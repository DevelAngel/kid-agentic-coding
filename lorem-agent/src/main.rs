//! Fake ACP agent that answers every prompt with Lorem Ipsum text instead
//! of calling a real LLM, so downstream code can be tested without tokens.

mod lorem;

use clap::Parser;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentNotification, ConnectMcpRequest, ContentBlock, ContentChunk, InitializeRequest,
    InitializeResponse, McpServer, McpServerAcpId, MessageMcpNotification, MessageMcpRequest,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionId,
    SessionNotification, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallId,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{
    Agent, Client, ConnectionTo, Error, ErrorCode, Stdio, on_receive_request,
};
use color_eyre::Result;
use std::io;
use std::process;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::time::sleep;

use serde_json::{Map, json};
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
const REPLY_WORD_COUNT: usize = 12;

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
static CONFETTI_SERVER_ID: OnceLock<McpServerAcpId> = OnceLock::new();

/// One step in a fake agent's simulated reasoning: either a thought, or a
/// tool call that goes `InProgress` then `Completed`.
enum Step {
    Thought(&'static str),
    ToolCall(&'static str),
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
            Step::Thought("Generating lorem ipsum"),
            Step::ToolCall("generate_lorem_ipsum"),
        ],
        1 => vec![
            Step::Thought("Breaking the request into steps"),
            Step::ToolCall("search_files"),
            Step::ToolCall("read_file"),
            Step::Thought("Cross-checking the findings"),
            Step::ToolCall("list_directory"),
        ],
        _ => vec![
            Step::Thought("Exploring possible approaches"),
            Step::ToolCall("grep_codebase"),
            Step::ToolCall("read_file"),
            Step::Thought("Narrowing down the relevant files"),
            Step::ToolCall("read_file"),
            Step::ToolCall("run_tests"),
            Step::Thought("Verifying edge cases"),
            Step::ToolCall("write_file"),
            Step::ToolCall("run_tests"),
        ],
    };
    if supports_mcp && prompt_index % 3 == 2 {
        steps.push(Step::ToolCall("confetti"));
    }
    steps
}

async fn invoke_confetti(
    connection: ConnectionTo<Client>,
    server_id: McpServerAcpId,
) -> Result<()> {
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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    color_eyre::install()?;
    tracing_subscriber::fmt().with_writer(io::stderr).init();

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

                responder.respond(NewSessionResponse::new(session_id))
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async |request: PromptRequest, responder, cx| {
                let prompt_index = NEXT_PROMPT_INDEX.fetch_add(1, Ordering::Relaxed);

                if args.fail_session {
                    return Err(Error::from(ErrorCode::InternalError));
                }
                if args.crash {
                    process::exit(1);
                }
                let supports_mcp = !args.disable_mcp;
                let fail_tool = args.fail_tool;

                let _ = cx.clone().spawn(async move {
                    let seed = NEXT_PROMPT_SEED.fetch_add(REPLY_WORD_COUNT, Ordering::Relaxed);
                    let text = lorem::generate(seed, REPLY_WORD_COUNT);
                    for (step_index, step) in
                        plan_for(prompt_index, supports_mcp).into_iter().enumerate()
                    {
                        match step {
                            Step::Thought(thought) => {
                                sleep(THINKING_DELAY).await;
                                cx.send_notification(AgentNotification::SessionNotification(
                                    SessionNotification::new(
                                        request.session_id.clone(),
                                        SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                                            ContentBlock::Text(TextContent::new(
                                                thought.to_owned(),
                                            )),
                                        )),
                                    ),
                                ))?;
                            }
                            Step::ToolCall(name) => {
                                let tool_call_id =
                                    ToolCallId::new(format!("lorem-tool-{seed}-{step_index}"));
                                cx.send_notification(AgentNotification::SessionNotification(
                                    SessionNotification::new(
                                        request.session_id.clone(),
                                        SessionUpdate::ToolCall(
                                            ToolCall::new(tool_call_id.clone(), name)
                                                .status(ToolCallStatus::InProgress)
                                                .raw_input(serde_json::json!({
                                                    "tool": name,
                                                    "prompt_seed": seed,
                                                    "step_index": step_index,
                                                })),
                                        ),
                                    ),
                                ))?;

                                if name == "confetti" {
                                    sleep(TOOL_CALL_DELAY).await;
                                    let (status, result_text) =
                                        match CONFETTI_SERVER_ID.get().cloned() {
                                            Some(server_id) => {
                                                match invoke_confetti(cx.clone(), server_id).await {
                                                    Ok(()) => (
                                                        ToolCallStatus::Completed,
                                                        "confetti: ok".to_owned(),
                                                    ),
                                                    Err(error) => {
                                                        tracing::debug!(
                                                            ?error,
                                                            "confetti MCP invocation failed"
                                                        );
                                                        (
                                                            ToolCallStatus::Failed,
                                                            "confetti: failed".to_owned(),
                                                        )
                                                    }
                                                }
                                            }
                                            None => (
                                                ToolCallStatus::Failed,
                                                "confetti: MCP server unavailable".to_owned(),
                                            ),
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
                                } else {
                                    sleep(TOOL_CALL_DELAY).await;
                                    let (status, result_text) = if fail_tool && step_index == 0 {
                                        (
                                            ToolCallStatus::Failed,
                                            format!("{name}: simulated failure"),
                                        )
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
                            }
                        }
                    }

                    cx.send_notification(AgentNotification::SessionNotification(
                        SessionNotification::new(
                            request.session_id.clone(),
                            SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                ContentBlock::Text(TextContent::new(text)),
                            )),
                        ),
                    ))?;

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
    use super::{Step, plan_for};

    fn tool_call_names(steps: &[Step]) -> Vec<&'static str> {
        steps
            .iter()
            .filter_map(|step| match step {
                Step::ToolCall(name) => Some(*name),
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
    fn third_request_with_mcp_ends_with_confetti() {
        let steps = plan_for(2, true);
        assert!(matches!(steps.last(), Some(Step::ToolCall("confetti"))));
    }

    #[test]
    fn second_request_has_two_consecutive_tool_calls() {
        let steps = plan_for(1, false);
        assert_eq!(
            tool_call_names(&steps),
            vec!["search_files", "read_file", "list_directory"]
        );
        // search_files and read_file are adjacent tool calls; since a
        // thought no longer ends the cluster, both this pair and
        // list_directory (after a thought) end up in the same cluster.
        assert!(matches!(steps[1], Step::ToolCall("search_files")));
        assert!(matches!(steps[2], Step::ToolCall("read_file")));
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
