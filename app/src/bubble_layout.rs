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

        for message in log.messages() {
            let (text, alignment) = match message {
                Message::User(m) => (m.text.as_str(), Alignment::Right),
                Message::Agent(m) => (m.text.as_str(), Alignment::Left),
            };

            let text_lines = wrapped_line_count(text, text_width);
            let bubble_height = 2 + text_lines;

            let x = match alignment {
                Alignment::Right => width.saturating_sub(bubble_width),
                Alignment::Left => 0,
            };

            bubbles.push(Bubble {
                rect: Rect {
                    x,
                    y,
                    width: bubble_width,
                    height: bubble_height,
                },
                borders: Borders::ALL,
                alignment,
            });

            y = y.saturating_add(bubble_height);
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
        self.bubbles.iter().map(|b| self.visible_bubble(b)).collect()
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
