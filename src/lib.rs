//! A library for running prompts against ACP components.
//!
//! Provides [`PromptRunner`] for one-shot prompts and [`start_interactive_session`]
//! for multi-turn interactive sessions.

mod bridge;
mod prompt;
mod session;

pub use bridge::{SessionClosed, SessionEvent, SessionHandle};
pub use prompt::{PromptError, PromptRunner};
pub use session::start_interactive_session;
