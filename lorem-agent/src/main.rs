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
                let seed = NEXT_PROMPT_SEED.fetch_add(REPLY_WORD_COUNT, Ordering::Relaxed);
                let text = lorem::generate(seed, REPLY_WORD_COUNT);

                // Emit a thought and a tool call round-trip so downstream
                // code exercising SessionUpdate::AgentThoughtChunk/ToolCall/
                // ToolCallUpdate has something real to observe. Delays are
                // deliberate: without them every status flashes by within
                // the same tick and never renders as actually running.
                sleep(THINKING_DELAY).await;
                cx.send_notification(AgentNotification::SessionNotification(
                    SessionNotification::new(
                        request.session_id.clone(),
                        SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(
                            TextContent::new("Generating lorem ipsum".to_owned()),
                        ))),
                    ),
                ))?;

                let tool_call_id = ToolCallId::new(format!("lorem-tool-{seed}"));
                cx.send_notification(AgentNotification::SessionNotification(
                    SessionNotification::new(
                        request.session_id.clone(),
                        SessionUpdate::ToolCall(
                            ToolCall::new(tool_call_id.clone(), "generate_lorem_ipsum")
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
                            ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
                        )),
                    ),
                ))?;

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
