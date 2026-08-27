//! Contract tests for `BubbleLayout`.

use kid_agentic_coding::{Alignment, BubbleLayout, ChatLog, Status};
use ratatui::widgets::Borders;

#[test]
fn short_message_yields_single_row_of_text_plus_border() {
    let mut log = ChatLog::new();
    log.push_user("hi");

    let layout = BubbleLayout::new(&log, 80, 24);

    assert_eq!(layout.bubbles().len(), 1);
    // top border + one text line + bottom border
    assert_eq!(layout.bubbles()[0].rect.height, 3);
}

#[test]
fn long_message_wraps_and_increases_height() {
    let mut log = ChatLog::new();
    log.push_agent("word ".repeat(50).trim().to_owned());

    let layout = BubbleLayout::new(&log, 40, 24);

    assert!(layout.bubbles()[0].rect.height > 3);
}

#[test]
fn user_messages_align_right_agent_messages_align_left() {
    let mut log = ChatLog::new();
    log.push_user("hi");
    log.push_agent("ho");

    let layout = BubbleLayout::new(&log, 80, 24);

    assert_eq!(layout.bubbles()[0].alignment, Alignment::Right);
    assert_eq!(layout.bubbles()[1].alignment, Alignment::Left);
}

#[test]
fn every_bubble_has_full_borders() {
    let mut log = ChatLog::new();
    log.push_user("one");
    log.push_agent("two");
    log.push_user("three");

    let layout = BubbleLayout::new(&log, 80, 24);

    for bubble in layout.bubbles() {
        assert_eq!(bubble.borders, Borders::ALL);
    }
}

#[test]
fn total_height_is_sum_of_bubble_heights() {
    let mut log = ChatLog::new();
    log.push_user("one");
    log.push_agent("two");

    let layout = BubbleLayout::new(&log, 80, 24);

    let expected: u16 = layout.bubbles().iter().map(|b| b.rect.height).sum();
    assert_eq!(layout.total_height(), expected);
}

#[test]
fn scroll_is_clamped_to_zero_when_content_fits_viewport() {
    let mut log = ChatLog::new();
    log.push_user("hi");

    let mut layout = BubbleLayout::new(&log, 80, 24);
    layout.scroll(100);

    assert_eq!(layout.scroll_offset(), 0);
}

#[test]
fn scroll_is_clamped_to_max_offset_when_content_overflows() {
    let mut log = ChatLog::new();
    for i in 0..50 {
        log.push_user(format!("message {i}"));
    }

    let mut layout = BubbleLayout::new(&log, 80, 10);
    layout.scroll(10_000);

    let max_offset = layout.total_height().saturating_sub(10);
    assert_eq!(layout.scroll_offset(), max_offset);
}

#[test]
fn scroll_does_not_go_negative() {
    let mut log = ChatLog::new();
    log.push_user("hi");

    let mut layout = BubbleLayout::new(&log, 80, 24);
    layout.scroll(-100);

    assert_eq!(layout.scroll_offset(), 0);
}

#[test]
fn fully_visible_bubble_keeps_full_borders_and_no_text_skip() {
    let mut log = ChatLog::new();
    log.push_user("hi");

    let layout = BubbleLayout::new(&log, 80, 24);
    let visible = layout.visible_bubbles();

    let bubble = visible[0].expect("bubble is within the viewport");
    assert_eq!(bubble.screen_rect.y, 0);
    assert_eq!(bubble.screen_rect.height, layout.bubbles()[0].rect.height);
    assert_eq!(bubble.borders, Borders::ALL);
    assert_eq!(bubble.text_line_skip, 0);
}

#[test]
fn bubble_scrolled_fully_out_of_view_is_hidden() {
    let mut log = ChatLog::new();
    log.push_user("one");
    log.push_agent("two");
    log.push_user("three");

    // Three single-line bubbles are 3 rows each (total 9). A 5-row
    // viewport scrolled to its max offset (4) only reaches rows [4,9),
    // so the first bubble (rows [0,3)) is fully out of view.
    let mut layout = BubbleLayout::new(&log, 80, 5);
    layout.scroll(1_000);

    let visible = layout.visible_bubbles();
    assert!(visible[0].is_none());
    assert!(visible[1].is_some());
    assert!(visible[2].is_some());
}

#[test]
fn scrolling_into_a_bubble_from_the_top_drops_its_top_border_and_does_not_overlap_the_next() {
    let mut log = ChatLog::new();
    log.push_user("one");
    log.push_agent("two");
    log.push_user("three");

    // Each bubble is 3 rows. Scrolling 1 row in cuts through the first
    // bubble's top border row, leaving 2 visible rows of it, followed
    // immediately (no gap, no overlap) by the second bubble.
    let mut layout = BubbleLayout::new(&log, 80, 5);
    layout.scroll(1);

    let visible = layout.visible_bubbles();
    let first = visible[0].expect("first bubble is partially visible");
    let second = visible[1].expect("second bubble is fully visible");

    assert_eq!(first.screen_rect.y, 0);
    assert_eq!(first.screen_rect.height, 2);
    assert!(!first.borders.contains(Borders::TOP));

    assert_eq!(
        second.screen_rect.y,
        first.screen_rect.y + first.screen_rect.height,
        "second bubble must start exactly where the first one's visible area ends"
    );
}

#[test]
fn visible_bubbles_never_overlap_at_any_scroll_offset() {
    let mut log = ChatLog::new();
    for i in 0..20 {
        log.push_user(format!("message number {i}"));
    }

    let viewport_height = 7;
    let mut layout = BubbleLayout::new(&log, 80, viewport_height);
    let max_offset = layout.total_height().saturating_sub(viewport_height);

    for scroll in 0..=max_offset {
        layout.scroll(-(max_offset as i32) as i16); // reset to 0, clamped
        layout.scroll(scroll as i16);

        let mut previous_bottom: Option<u16> = None;
        for bubble in layout.visible_bubbles().into_iter().flatten() {
            if let Some(bottom) = previous_bottom {
                assert!(
                    bubble.screen_rect.y >= bottom,
                    "bubble at y={} overlaps previous bubble ending at y={} (scroll={scroll})",
                    bubble.screen_rect.y,
                    bottom
                );
            }
            previous_bottom = Some(bubble.screen_rect.y + bubble.screen_rect.height);
        }
    }
}

#[test]
fn collapsed_settled_tool_cluster_is_a_single_unframed_row() {
    let mut log = ChatLog::new();
    let a = log.push_tool_call("git_status");
    let b = log.push_tool_call("git_switch_branch");
    log.update_tool_call_status(a, Status::Done);
    log.update_tool_call_status(b, Status::Done);
    // A settled cluster only collapses once it's no longer the newest
    // message (see keep_live in `ToolCluster::visible_steps`).
    log.push_user("hi");

    let layout = BubbleLayout::new(&log, 80, 24);

    assert_eq!(layout.bubbles().len(), 2);
    assert_eq!(layout.bubbles()[0].rect.height, 1);
    assert_eq!(layout.bubbles()[0].borders, Borders::NONE);
    assert_eq!(layout.bubbles()[0].alignment, Alignment::Left);
}

#[test]
fn settled_tool_cluster_stays_live_while_it_is_still_the_last_message() {
    let mut log = ChatLog::new();
    let a = log.push_tool_call("git_status");
    let b = log.push_tool_call("git_switch_branch");
    log.update_tool_call_status(a, Status::Done);
    log.update_tool_call_status(b, Status::Done);

    let layout = BubbleLayout::new(&log, 80, 24);

    // summary + 2 steps, no truncation marker since nothing is hidden
    assert_eq!(layout.bubbles()[0].rect.height, 3);
}

#[test]
fn still_running_tool_cluster_shows_a_live_tail() {
    let mut log = ChatLog::new();
    log.push_tool_call("git_status");
    log.push_tool_call("git_switch_branch");
    let running = log.push_tool_call("git_pull");
    log.update_tool_call_status(running, Status::Running);

    let layout = BubbleLayout::new(&log, 80, 24);

    // summary + 3 steps, no truncation marker since all 3 fit
    assert_eq!(layout.bubbles()[0].rect.height, 4);
}

#[test]
fn expanded_tool_cluster_reserves_one_row_per_step_plus_summary() {
    let mut log = ChatLog::new();
    let a = log.push_tool_call("git_status");
    let b = log.push_tool_call("git_switch_branch");
    let c = log.push_tool_call("git_pull");
    log.update_tool_call_status(a, Status::Done);
    log.update_tool_call_status(b, Status::Done);
    log.update_tool_call_status(c, Status::Done);
    log.toggle_cluster(0);

    let layout = BubbleLayout::new(&log, 80, 24);

    assert_eq!(layout.bubbles()[0].rect.height, 4);
}

#[test]
fn thought_row_is_unframed() {
    let mut log = ChatLog::new();
    log.push_thought("checking existing error handling");

    let layout = BubbleLayout::new(&log, 80, 24);

    assert_eq!(layout.bubbles()[0].borders, Borders::NONE);
    assert_eq!(layout.bubbles()[0].alignment, Alignment::Left);
}

#[test]
fn scroll_to_bottom_jumps_to_max_offset() {
    let mut log = ChatLog::new();
    for i in 0..50 {
        log.push_user(format!("message {i}"));
    }

    let mut layout = BubbleLayout::new(&log, 80, 10);
    layout.scroll_to_bottom();

    let max_offset = layout.total_height().saturating_sub(10);
    assert_eq!(layout.scroll_offset(), max_offset);
}

#[test]
fn scroll_to_bottom_stays_at_zero_when_content_fits_viewport() {
    let mut log = ChatLog::new();
    log.push_user("hi");

    let mut layout = BubbleLayout::new(&log, 80, 24);
    layout.scroll_to_bottom();

    assert_eq!(layout.scroll_offset(), 0);
}

#[test]
fn anchor_round_trips_when_layout_is_unchanged() {
    let mut log = ChatLog::new();
    for i in 0..20 {
        log.push_user(format!("message {i}"));
    }

    let mut layout = BubbleLayout::new(&log, 80, 7);
    layout.scroll(5);
    let anchor = layout.anchor().expect("log is not empty");

    let mut rebuilt = BubbleLayout::new(&log, 80, 7);
    rebuilt.scroll_to_anchor(anchor);

    assert_eq!(rebuilt.scroll_offset(), layout.scroll_offset());
}

#[test]
fn anchor_keeps_a_later_bubble_pinned_when_an_earlier_cluster_shrinks() {
    let mut log = ChatLog::new();
    let a = log.push_tool_call("git_status");
    let b = log.push_tool_call("git_switch_branch");
    let running = log.push_tool_call("git_pull");
    log.update_tool_call_status(a, Status::Done);
    log.update_tool_call_status(b, Status::Done);
    log.update_tool_call_status(running, Status::Running);
    log.push_user("hi");

    // Cluster is 4 rows while running, user bubble is 3 rows: total 7.
    // A 3-row viewport scrolled to the bottom lands exactly on the user
    // bubble.
    let mut layout = BubbleLayout::new(&log, 80, 3);
    layout.scroll_to_bottom();
    let anchor = layout.anchor().expect("log is not empty");
    assert_eq!(anchor.message_index, 1);
    assert_eq!(anchor.row_offset, 0);

    // Cluster settles and collapses to a single summary row.
    log.update_tool_call_status(running, Status::Done);
    let mut settled = BubbleLayout::new(&log, 80, 3);
    settled.scroll_to_anchor(anchor);

    let visible = settled.visible_bubbles();
    let user_bubble = visible[1].expect("user bubble stays visible");
    assert_eq!(
        user_bubble.screen_rect.y, 0,
        "anchored bubble stays pinned to the same screen row"
    );
}

#[test]
fn anchor_clamps_when_its_own_bubble_shrinks_past_the_row_offset() {
    let mut log = ChatLog::new();
    let a = log.push_tool_call("git_status");
    let b = log.push_tool_call("git_switch_branch");
    let running = log.push_tool_call("git_pull");
    log.update_tool_call_status(a, Status::Done);
    log.update_tool_call_status(b, Status::Done);
    log.update_tool_call_status(running, Status::Running);
    log.push_user("hi"); // pushes the cluster out of the "still last" slot

    // A 1-row viewport scrolled all the way down lands on the cluster's
    // last step row (row 3 of the 4-row running cluster).
    let mut layout = BubbleLayout::new(&log, 80, 1);
    layout.scroll(3);
    let anchor = layout.anchor().expect("log is not empty");
    assert_eq!(anchor.message_index, 0);
    assert_eq!(anchor.row_offset, 3);

    // Cluster settles and collapses to a single summary row (height 1),
    // which no longer has a row 3 to anchor to.
    log.update_tool_call_status(running, Status::Done);
    let mut settled = BubbleLayout::new(&log, 80, 1);
    settled.scroll_to_anchor(anchor);

    assert_eq!(
        settled.scroll_offset(),
        0,
        "clamped to the shrunk bubble's last row instead of an unrelated bubble"
    );
}

#[test]
fn extend_to_bottom_follows_the_message_that_pushes_a_settled_cluster_off_the_bottom() {
    let mut log = ChatLog::new();
    log.push_user("one");
    log.push_user("two");
    log.push_user("three");
    let a = log.push_tool_call("git_status");
    let b = log.push_tool_call("git_switch_branch");
    let running = log.push_tool_call("git_pull");
    log.update_tool_call_status(a, Status::Done);
    log.update_tool_call_status(b, Status::Done);
    log.update_tool_call_status(running, Status::Running);

    // 3 padding bubbles (3 rows each = 9) + a running cluster (4 rows) = 13.
    let mut layout = BubbleLayout::new(&log, 80, 2);
    layout.scroll_to_bottom();
    let anchor = layout.anchor().expect("log is not empty");

    // Cluster settles (collapsing to 1 row once it's no longer last, see
    // `keep_live`) and a new message arrives: total is 9 + 1 + 3 = 13
    // again. extend_to_bottom must follow down to reveal the new
    // message instead of getting stuck at the anchor's old position.
    log.update_tool_call_status(running, Status::Done);
    log.push_user("done");
    let mut settled = BubbleLayout::new(&log, 80, 2);
    settled.scroll_to_anchor(anchor);
    settled.extend_to_bottom();

    assert_eq!(
        settled.scroll_offset(),
        settled.total_height() - 2,
        "follows the new message to the bottom instead of staying at the anchor"
    );
}

#[test]
fn extend_to_bottom_follows_growth_of_the_last_bubble() {
    let mut log = ChatLog::new();
    log.push_user("one");

    let mut layout = BubbleLayout::new(&log, 80, 2);
    layout.scroll_to_bottom();
    let anchor = layout.anchor().expect("log is not empty");

    log.push_agent("word ".repeat(20).trim().to_owned());
    let mut grown = BubbleLayout::new(&log, 80, 2);
    grown.scroll_to_anchor(anchor);
    grown.extend_to_bottom();

    let max_offset = grown.total_height() - 2;
    assert_eq!(
        grown.scroll_offset(),
        max_offset,
        "advances to reveal the newly appended message"
    );
}
