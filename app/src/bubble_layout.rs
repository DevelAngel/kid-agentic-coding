//! Pure layout computation for chat bubbles: bounding rects, border sets,
//! and scroll bounds. No terminal or I/O access.

use crate::chat_log::{ChatLog, Message};
use ratatui::layout::Rect;
use ratatui::widgets::Borders;

/// Horizontal alignment of a bubble within the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Right,
}

/// Computed placement, border set, and alignment for a single bubble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bubble {
    pub rect: Rect,
    pub borders: Borders,
    pub alignment: Alignment,
}

/// Computes bubble rects, border sets, and scroll bounds for a [`ChatLog`]
/// rendered into a viewport of a given width and height.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BubbleLayout {
    bubbles: Vec<Bubble>,
    total_height: u16,
    viewport_height: u16,
    scroll_offset: u16,
}

impl BubbleLayout {
    /// Computes the layout for `log` within a viewport of `width` x
    /// `height` columns/rows. Bubbles take up to 70% of `width`.
    pub fn new(log: &ChatLog, width: u16, height: u16) -> Self {
        let bubble_width = bubble_width(width);
        let text_width = bubble_width.saturating_sub(2).max(1);

        let mut bubbles = Vec::with_capacity(log.len());
        let mut y: u16 = 0;
        let last_index = log.len().saturating_sub(1);

        for (index, message) in log.messages().iter().enumerate() {
            let (rect, borders, alignment) = match message {
                Message::User(m) => {
                    let text_lines = wrapped_line_count(&m.text, text_width);
                    framed_rect(bubble_width, width, 2 + text_lines, Alignment::Right)
                }
                Message::Agent(m) => {
                    let text_lines = wrapped_line_count(&m.text, text_width);
                    framed_rect(bubble_width, width, 2 + text_lines, Alignment::Left)
                }
                Message::ToolCluster(cluster) => {
                    let keep_live = index == last_index;
                    unframed_rect(bubble_width, cluster.visible_row_count(keep_live) as u16)
                }
                Message::SessionNotice(m) => {
                    let text_lines = wrapped_line_count(&m.text, text_width);
                    unframed_rect(bubble_width, 1 + text_lines)
                }
            };

            bubbles.push(Bubble {
                rect: Rect { y, ..rect },
                borders,
                alignment,
            });

            y = y.saturating_add(rect.height);
        }

        Self {
            bubbles,
            total_height: y,
            viewport_height: height,
            scroll_offset: 0,
        }
    }

    /// Bounding rects, border sets, and alignment, one per message, in
    /// the same order as the source [`ChatLog`].
    pub fn bubbles(&self) -> &[Bubble] {
        &self.bubbles
    }

    /// Total height in rows of all bubbles stacked vertically.
    pub fn total_height(&self) -> u16 {
        self.total_height
    }

    /// Current scroll offset in rows from the top of the content.
    pub fn scroll_offset(&self) -> u16 {
        self.scroll_offset
    }

    /// Moves the scroll offset by `delta` rows, clamped to
    /// `0..=(total_height - viewport_height)` (or `0` if content fits).
    pub fn scroll(&mut self, delta: i16) {
        let max_offset = self.total_height.saturating_sub(self.viewport_height);
        let current = i32::from(self.scroll_offset) + i32::from(delta);
        self.scroll_offset = current.clamp(0, i32::from(max_offset)) as u16;
    }

    /// Jumps the scroll offset to the bottom of the content (or `0` if it
    /// fits within the viewport).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.total_height.saturating_sub(self.viewport_height);
    }

    /// Bubbles currently within the viewport at the current scroll
    /// offset, one slot per message in [`bubbles`](Self::bubbles) order
    /// (`None` where a bubble is scrolled fully out of view).
    ///
    /// `screen_rect` is relative to the top-left of the viewport (not
    /// the terminal), already reduced to only the visible rows, with
    /// whichever border edge got scrolled past removed so a truncated
    /// bubble never shows a border implying it's complete.
    /// `text_line_skip` is how many wrapped text lines are hidden above
    /// the visible window, for use with `Paragraph::scroll`.
    pub fn visible_bubbles(&self) -> Vec<Option<VisibleBubble>> {
        self.bubbles
            .iter()
            .map(|b| self.visible_bubble(b))
            .collect()
    }

    fn visible_bubble(&self, bubble: &Bubble) -> Option<VisibleBubble> {
        let bubble_top = bubble.rect.y;
        let bubble_bottom = bubble_top.saturating_add(bubble.rect.height);
        let viewport_bottom = self.scroll_offset.saturating_add(self.viewport_height);

        let visible_top = bubble_top.max(self.scroll_offset);
        let visible_bottom = bubble_bottom.min(viewport_bottom);
        if visible_top >= visible_bottom {
            return None;
        }

        let hidden_above = visible_top - bubble_top;
        let visible_height = visible_bottom - visible_top;

        let mut borders = bubble.borders;
        if hidden_above > 0 {
            borders.remove(Borders::TOP);
        }
        if visible_bottom < bubble_bottom {
            borders.remove(Borders::BOTTOM);
        }

        let top_border_rows = u16::from(bubble.borders.contains(Borders::TOP));
        let text_line_skip = hidden_above.saturating_sub(top_border_rows);

        Some(VisibleBubble {
            screen_rect: Rect {
                x: bubble.rect.x,
                y: visible_top - self.scroll_offset,
                width: bubble.rect.width,
                height: visible_height,
            },
            borders,
            text_line_skip,
            alignment: bubble.alignment,
        })
    }

    /// The bubble currently at the top of the viewport, and how many of
    /// its rows are scrolled past. `None` if the log is empty.
    pub fn anchor(&self) -> Option<ScrollAnchor> {
        let (message_index, bubble) = self
            .bubbles
            .iter()
            .enumerate()
            .find(|(_, b)| b.rect.y.saturating_add(b.rect.height) > self.scroll_offset)?;
        Some(ScrollAnchor {
            message_index,
            row_offset: self.scroll_offset.saturating_sub(bubble.rect.y),
        })
    }

    /// Moves the scroll offset so the row identified by `anchor` sits at
    /// the top of the viewport. If the anchor's bubble has shrunk past
    /// `row_offset`, clamps to its last row instead of jumping to an
    /// unrelated bubble; otherwise never forces the viewport to pack
    /// against the bottom, so shrinking content leaves blank space
    /// there rather than reflowing bubbles above it.
    pub fn scroll_to_anchor(&mut self, anchor: ScrollAnchor) {
        let Some(bubble) = self.bubbles.get(anchor.message_index) else {
            self.scroll_to_bottom();
            return;
        };
        let row_offset = anchor.row_offset.min(bubble.rect.height.saturating_sub(1));
        self.scroll_offset = bubble.rect.y.saturating_add(row_offset);
    }

    /// Advances the scroll offset just far enough to reveal the newest
    /// content, without ever scrolling back up. Applied after
    /// [`Self::scroll_to_anchor`] to make autoscroll follow growth (new
    /// messages, streaming text) while still tolerating shrinkage
    /// elsewhere without snapping back down to force-fill the viewport.
    pub fn extend_to_bottom(&mut self) {
        let max_offset = self.total_height.saturating_sub(self.viewport_height);
        self.scroll_offset = self.scroll_offset.max(max_offset);
    }
}

/// A row within a specific message's bubble, used to keep the viewport
/// glued to content across layout recomputation instead of an absolute
/// offset that drifts when an earlier bubble changes height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollAnchor {
    pub message_index: usize,
    pub row_offset: u16,
}

/// The visible slice of a [`Bubble`] at the current scroll offset. See
/// [`BubbleLayout::visible_bubbles`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleBubble {
    pub screen_rect: Rect,
    pub borders: Borders,
    pub text_line_skip: u16,
    pub alignment: Alignment,
}

/// Bubble width in columns: up to 70% of the viewport width, at least
/// wide enough for a left and right border column.
fn bubble_width(viewport_width: u16) -> u16 {
    let seventy_percent = (u32::from(viewport_width) * 70) / 100;
    (seventy_percent as u16).max(2).min(viewport_width.max(2))
}

/// Rect/borders/alignment for a bordered bubble of the given content
/// `height` (already including the top/bottom border rows). `y` is left
/// at `0`; the caller overwrites it once the running offset is known.
fn framed_rect(
    bubble_width: u16,
    viewport_width: u16,
    height: u16,
    alignment: Alignment,
) -> (Rect, Borders, Alignment) {
    let x = match alignment {
        Alignment::Right => viewport_width.saturating_sub(bubble_width),
        Alignment::Left => 0,
    };
    (
        Rect {
            x,
            y: 0,
            width: bubble_width,
            height,
        },
        Borders::ALL,
        alignment,
    )
}

/// Rect/borders/alignment for an unframed, left-aligned row (thoughts and
/// tool clusters) of the given `height`. `y` is left at `0`; the caller
/// overwrites it once the running offset is known.
fn unframed_rect(bubble_width: u16, height: u16) -> (Rect, Borders, Alignment) {
    (
        Rect {
            x: 0,
            y: 0,
            width: bubble_width,
            height,
        },
        Borders::NONE,
        Alignment::Left,
    )
}

/// Number of rows `text` occupies when greedily word-wrapped to `width`
/// columns. Words longer than `width` are not split further.
fn wrapped_line_count(text: &str, width: u16) -> u16 {
    let width = usize::from(width.max(1));
    let mut lines: u16 = 0;
    let mut current_width = 0usize;

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if current_width == 0 {
            lines += 1;
            current_width = word_len;
            continue;
        }

        let needed = current_width + 1 + word_len;
        if needed <= width {
            current_width = needed;
        } else {
            lines += 1;
            current_width = word_len;
        }
    }

    lines.max(1)
}
