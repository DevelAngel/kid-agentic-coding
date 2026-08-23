//! Contract tests for `TimelineLayout`.

use kid_agentic_coding::{Status, TimelineLayout, TimelineLog};

#[test]
fn tool_call_with_no_extra_lines_is_a_single_row() {
    let mut log = TimelineLog::new();
    log.push_tool_call("read_file", vec![]);

    let layout = TimelineLayout::new(&log, 80, 24);

    assert_eq!(layout.blocks().len(), 1);
    assert_eq!(layout.blocks()[0].rect.height, 1);
}

#[test]
fn extra_lines_increase_height_beyond_the_header_row() {
    let mut log = TimelineLog::new();
    log.push_tool_call("read_file", vec!["path: src/main.rs".to_string()]);

    let layout = TimelineLayout::new(&log, 80, 24);

    // header row + one content line
    assert_eq!(layout.blocks()[0].rect.height, 2);
}

#[test]
fn long_line_wraps_and_increases_height() {
    let mut log = TimelineLog::new();
    log.push_thought(vec!["word ".repeat(50).trim().to_owned()]);

    let layout = TimelineLayout::new(&log, 40, 24);

    // header row + several wrapped content rows
    assert!(layout.blocks()[0].rect.height > 2);
}

#[test]
fn last_entry_is_flagged_as_last() {
    let mut log = TimelineLog::new();
    log.push_tool_call("read_file", vec![]);
    log.push_thought(vec![]);

    let layout = TimelineLayout::new(&log, 80, 24);

    assert!(!layout.blocks()[0].is_last);
    assert!(layout.blocks()[1].is_last);
}

#[test]
fn total_height_is_sum_of_block_heights() {
    let mut log = TimelineLog::new();
    log.push_tool_call("read_file", vec!["a".to_string()]);
    log.push_thought(vec!["b".to_string(), "c".to_string()]);

    let layout = TimelineLayout::new(&log, 80, 24);

    let expected: u16 = layout.blocks().iter().map(|b| b.rect.height).sum();
    assert_eq!(layout.total_height(), expected);
}

#[test]
fn scroll_is_clamped_to_zero_when_content_fits_viewport() {
    let mut log = TimelineLog::new();
    log.push_tool_call("read_file", vec![]);

    let mut layout = TimelineLayout::new(&log, 80, 24);
    layout.scroll(100);

    assert_eq!(layout.scroll_offset(), 0);
}

#[test]
fn scroll_is_clamped_to_max_offset_when_content_overflows() {
    let mut log = TimelineLog::new();
    for i in 0..50 {
        log.push_tool_call(format!("tool_{i}"), vec![]);
    }

    let mut layout = TimelineLayout::new(&log, 80, 10);
    layout.scroll(10_000);

    let max_offset = layout.total_height().saturating_sub(10);
    assert_eq!(layout.scroll_offset(), max_offset);
}

#[test]
fn scroll_does_not_go_negative() {
    let mut log = TimelineLog::new();
    log.push_tool_call("read_file", vec![]);

    let mut layout = TimelineLayout::new(&log, 80, 24);
    layout.scroll(-100);

    assert_eq!(layout.scroll_offset(), 0);
}

#[test]
fn fully_visible_block_has_no_line_skip() {
    let mut log = TimelineLog::new();
    log.push_tool_call("read_file", vec![]);

    let layout = TimelineLayout::new(&log, 80, 24);
    let visible = layout.visible_blocks();

    let block = visible[0].expect("block is within the viewport");
    assert_eq!(block.screen_rect.y, 0);
    assert_eq!(block.screen_rect.height, layout.blocks()[0].rect.height);
    assert_eq!(block.line_skip, 0);
}

#[test]
fn block_scrolled_fully_out_of_view_is_hidden() {
    let mut log = TimelineLog::new();
    log.push_tool_call("one", vec![]);
    log.push_tool_call("two", vec![]);
    log.push_tool_call("three", vec![]);

    // Three single-row blocks (total 3). A 2-row viewport scrolled to
    // its max offset (1) only reaches rows [1,3), so the first block
    // (row [0,1)) is fully out of view.
    let mut layout = TimelineLayout::new(&log, 80, 2);
    layout.scroll(1_000);

    let visible = layout.visible_blocks();
    assert!(visible[0].is_none());
    assert!(visible[1].is_some());
    assert!(visible[2].is_some());
}

#[test]
fn scrolling_into_a_block_skips_its_hidden_top_lines() {
    let mut log = TimelineLog::new();
    log.push_tool_call("one", vec!["a".to_string(), "b".to_string()]);
    log.push_tool_call("two", vec![]);

    // First block is 3 rows (header + 2 lines). Scrolling 2 rows in
    // leaves only its last row visible, with 2 hidden rows above.
    let mut layout = TimelineLayout::new(&log, 80, 2);
    layout.scroll(2);

    let visible = layout.visible_blocks();
    let first = visible[0].expect("first block is partially visible");
    assert_eq!(first.screen_rect.y, 0);
    assert_eq!(first.screen_rect.height, 1);
    assert_eq!(first.line_skip, 2);
}

#[test]
fn visible_blocks_never_overlap_at_any_scroll_offset() {
    let mut log = TimelineLog::new();
    for i in 0..20 {
        log.push_tool_call(format!("tool_{i}"), vec!["detail".to_string()]);
    }

    let viewport_height = 7;
    let mut layout = TimelineLayout::new(&log, 80, viewport_height);
    let max_offset = layout.total_height().saturating_sub(viewport_height);

    for scroll in 0..=max_offset {
        layout.scroll(-(max_offset as i32) as i16); // reset to 0, clamped
        layout.scroll(scroll as i16);

        let mut previous_bottom: Option<u16> = None;
        for block in layout.visible_blocks().into_iter().flatten() {
            if let Some(bottom) = previous_bottom {
                assert!(
                    block.screen_rect.y >= bottom,
                    "block at y={} overlaps previous block ending at y={} (scroll={scroll})",
                    block.screen_rect.y,
                    bottom
                );
            }
            previous_bottom = Some(block.screen_rect.y + block.screen_rect.height);
        }
    }
}

#[test]
fn status_is_exposed_per_block() {
    let mut log = TimelineLog::new();
    let id = log.push_tool_call("read_file", vec![]);
    log.update_tool_call_status(id, Status::Running);

    let layout = TimelineLayout::new(&log, 80, 24);

    assert_eq!(layout.blocks()[0].status, Status::Running);
}
