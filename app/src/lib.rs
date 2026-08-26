//! A library for running prompts against ACP components.
//!
//! Provides [`PromptRunner`] for one-shot prompts and [`start_interactive_session`]
//! for multi-turn interactive sessions.

mod bridge;
mod bubble_layout;
mod chat_log;
mod prompt;
mod session;

pub use bridge::{SessionClosed, SessionEvent, SessionHandle};
pub use bubble_layout::{Alignment, Bubble, BubbleLayout, ScrollAnchor, VisibleBubble};
pub use chat_log::{
    AgentMessage, ChatLog, EntryId, Message, SessionNotice, SessionNoticeKind, Status, Step,
    ToolCallEntry, ToolCluster, UserMessage,
};
pub use prompt::{PromptError, PromptRunner};
pub use session::start_interactive_session;
