//! Fake ACP agent that answers every prompt with Lorem Ipsum text instead
//! of calling a real LLM, so downstream code can be tested without tokens.

mod lorem;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentNotification, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionId,
    SessionNotification, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallId,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, Stdio};
use color_eyre::Result;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::time::sleep;

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

/// One step in a fake agent's simulated reasoning: either a thought, or a
/// tool call that goes `InProgress` then `Completed`.
enum Step {
    Thought(&'static str),
    ToolCall(&'static str),
}

/// The thought/tool-call plan for the `prompt_index`-th prompt in a
/// session. Cycles every three requests: a first request keeps the
/// original single thought + single tool call; the second and third show
/// multiple tool calls clustering together and multiple thoughts breaking
/// up separate clusters, so the inline tool cluster UI has something
/// non-trivial to render.
fn plan_for(prompt_index: usize) -> Vec<Step> {
    match prompt_index % 3 {
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
            Step::ToolCall("read_file"),
            Step::Thought("Verifying edge cases"),
            Step::ToolCall("run_tests"),
            Step::Thought("Refining the implementation plan"),
            Step::ToolCall("write_file"),
            Step::ToolCall("run_tests"),
        ],
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt().with_writer(io::stderr).init();

    Agent
        .builder()
        .on_receive_request(
            async |_request: InitializeRequest, responder, _cx| {
                responder.respond(InitializeResponse::new(ProtocolVersion::V1))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |_request: NewSessionRequest, responder, _cx| {
                let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
                let session_id: SessionId = format!("lorem-session-{id}").into();
                responder.respond(NewSessionResponse::new(session_id))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: PromptRequest, responder, cx| {
                let prompt_index = NEXT_PROMPT_INDEX.fetch_add(1, Ordering::Relaxed);
                let seed = NEXT_PROMPT_SEED.fetch_add(REPLY_WORD_COUNT, Ordering::Relaxed);
                let text = lorem::generate(seed, REPLY_WORD_COUNT);

                // Walk the plan for this request so downstream code
                // exercising SessionUpdate::AgentThoughtChunk/ToolCall/
                // ToolCallUpdate has something real, and varied, to
                // observe. Delays are deliberate: without them every
                // status flashes by within the same tick and never
                // renders as actually running.
                for (step_index, step) in plan_for(prompt_index).into_iter().enumerate() {
                    match step {
                        Step::Thought(thought) => {
                            sleep(THINKING_DELAY).await;
                            cx.send_notification(AgentNotification::SessionNotification(
                                SessionNotification::new(
                                    request.session_id.clone(),
                                    SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                                        ContentBlock::Text(TextContent::new(thought.to_owned())),
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
                                            .status(ToolCallStatus::InProgress),
                                    ),
                                ),
                            ))?;

                            sleep(TOOL_CALL_DELAY).await;
                            cx.send_notification(AgentNotification::SessionNotification(
                                SessionNotification::new(
                                    request.session_id.clone(),
                                    SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                                        tool_call_id,
                                        ToolCallUpdateFields::new()
                                            .status(ToolCallStatus::Completed),
                                    )),
                                ),
                            ))?;
                        }
                    }
                }

                cx.send_notification(AgentNotification::SessionNotification(
                    SessionNotification::new(
                        request.session_id.clone(),
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                            TextContent::new(text),
                        ))),
                    ),
                ))?;

                responder.respond(PromptResponse::new(StopReason::EndTurn))
            },
            agent_client_protocol::on_receive_request!(),
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
        let steps = plan_for(0);
        assert_eq!(tool_call_names(&steps), vec!["generate_lorem_ipsum"]);
        assert!(matches!(steps[0], Step::Thought(_)));
    }

    #[test]
    fn second_request_has_two_consecutive_tool_calls() {
        let steps = plan_for(1);
        assert_eq!(
            tool_call_names(&steps),
            vec!["search_files", "read_file", "list_directory"]
        );
        // search_files and read_file must be adjacent so they join one
        // cluster; list_directory follows a thought, starting a new one.
        assert!(matches!(steps[1], Step::ToolCall("search_files")));
        assert!(matches!(steps[2], Step::ToolCall("read_file")));
    }

    #[test]
    fn third_request_has_multiple_clusters() {
        let steps = plan_for(2);
        assert_eq!(tool_call_names(&steps).len(), 6);
        let thought_count = steps
            .iter()
            .filter(|step| matches!(step, Step::Thought(_)))
            .count();
        assert_eq!(thought_count, 3);
    }

    #[test]
    fn plan_cycles_every_three_requests() {
        assert_eq!(tool_call_names(&plan_for(0)), tool_call_names(&plan_for(3)));
        assert_eq!(tool_call_names(&plan_for(1)), tool_call_names(&plan_for(4)));
    }
}

