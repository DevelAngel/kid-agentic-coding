//! Terminal UI components for agentic coding.

mod bubble_layout;
mod markdown;

pub use bubble_layout::{Alignment, Bubble, BubbleLayout, ScrollAnchor, VisibleBubble};
pub use markdown::render as render_markdown;
