//! Pure layout computation for the vertical timeline: bounding rects and
//! scroll bounds for tool call and thought entries. No terminal or I/O
//! access. Presentation (prefix glyphs, colors) is left to the caller,
//! which has `is_last` and `status` to decide with.

use crate::timeline::{Status, TimelineLog};
use ratatui::layout::Rect;

/// Computed placement for a single timeline entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineBlock {
    pub rect: Rect,
    pub is_last: bool,
    pub status: Status,
}

/// Computes block rects and scroll bounds for a [`TimelineLog`] rendered
/// into a viewport of a given width and height.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineLayout {
    blocks: Vec<TimelineBlock>,
    total_height: u16,
    viewport_height: u16,
    scroll_offset: u16,
}

/// Width in columns reserved for the prefix column (`├─ `, `╰─ `, `│  `).
const PREFIX_WIDTH: u16 = 3;

impl TimelineLayout {
    /// Computes the layout for `log` within a viewport of `width` x
    /// `height` columns/rows. Each entry is a header row (kind/status,
    /// rendered by the caller) followed by its content lines, wrapped to
    /// fit next to the prefix column.
    pub fn new(log: &TimelineLog, width: u16, height: u16) -> Self {
        let text_width = width.saturating_sub(PREFIX_WIDTH).max(1);
        let entry_count = log.entries().len();

        let mut blocks = Vec::with_capacity(entry_count);
        let mut y: u16 = 0;

        for (index, entry) in log.entries().iter().enumerate() {
            let content_lines: u16 = entry
                .lines
                .iter()
                .map(|line| wrapped_line_count(line, text_width))
                .sum();
            let block_height = 1 + content_lines;

            blocks.push(TimelineBlock {
                rect: Rect {
                    x: 0,
                    y,
                    width,
                    height: block_height,
                },
                is_last: index + 1 == entry_count,
                status: entry.status,
            });

            y = y.saturating_add(block_height);
        }

        Self {
            blocks,
            total_height: y,
            viewport_height: height,
            scroll_offset: 0,
        }
    }

    /// Bounding rects, one per entry, in the same order as the source
    /// [`TimelineLog`].
    pub fn blocks(&self) -> &[TimelineBlock] {
        &self.blocks
    }

    /// Total height in rows of all blocks stacked vertically.
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

    /// Blocks currently within the viewport at the current scroll offset,
    /// one slot per entry in [`blocks`](Self::blocks) order (`None` where
    /// a block is scrolled fully out of view).
    ///
    /// `screen_rect` is relative to the top-left of the viewport (not the
    /// terminal), already reduced to only the visible rows. `line_skip`
    /// is how many rows of the entry (header and/or content) are hidden
    /// above the visible window.
    pub fn visible_blocks(&self) -> Vec<Option<VisibleBlock>> {
        self.blocks.iter().map(|b| self.visible_block(b)).collect()
    }

    fn visible_block(&self, block: &TimelineBlock) -> Option<VisibleBlock> {
        let block_top = block.rect.y;
        let block_bottom = block_top.saturating_add(block.rect.height);
        let viewport_bottom = self.scroll_offset.saturating_add(self.viewport_height);

        let visible_top = block_top.max(self.scroll_offset);
        let visible_bottom = block_bottom.min(viewport_bottom);
        if visible_top >= visible_bottom {
            return None;
        }

        let line_skip = visible_top - block_top;
        let visible_height = visible_bottom - visible_top;

        Some(VisibleBlock {
            screen_rect: Rect {
                x: block.rect.x,
                y: visible_top - self.scroll_offset,
                width: block.rect.width,
                height: visible_height,
            },
            line_skip,
            is_last: block.is_last,
            status: block.status,
        })
    }
}

/// The visible slice of a [`TimelineBlock`] at the current scroll offset.
/// See [`TimelineLayout::visible_blocks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleBlock {
    pub screen_rect: Rect,
    pub line_skip: u16,
    pub is_last: bool,
    pub status: Status,
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
