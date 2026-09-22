//! A library for running prompts against ACP components.
//!
//! Provides interactive ACP session management for the terminal UI.

mod bubble_layout;
mod markdown;

pub use bubble_layout::{Alignment, Bubble, BubbleLayout, ScrollAnchor, VisibleBubble};
pub use kid_agentic_coding_chat::{
    AgentMessage, AutoMessage, ChatLog, EntryId, Message, SessionNotice, SessionNoticeKind,
    SessionTransition, Status, Step, ToolCallEntry, ToolCluster, UserMessage, strip_redundant_name,
};
pub use kid_agentic_coding_session::{
    AgentLauncher, CommitFixVerdict, FsSocketDir, PermissionOption, SessionClosed, SessionEvent,
    SessionHandle, StopReason, ToolStatus, parse_agent_args, start_interactive_session,
};
pub use markdown::render as render_markdown;
