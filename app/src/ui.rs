//! Interactive terminal UI for an ACP session.

use crate::log_buffer::LogBuffer;

use agent_client_protocol::AcpAgent;
use agent_client_protocol::schema::v1::{PermissionOption, StopReason, ToolCallId, ToolCallStatus};
use ansi_to_tui::IntoText;
use kid_agentic_coding::start_interactive_session;
use kid_agentic_coding::{
    BubbleLayout, ChatLog, EntryId, Message, PromptRunner, ScrollAnchor, SessionEvent,
    SessionHandle, SessionNoticeKind, Status, Step, ToolCluster, VisibleBubble,
};
use rand::RngExt;
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui_textarea::TextArea;
use textwrap::{self, Options};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{mpsc, oneshot};
use tokio::task;
use tokio::time::{self, MissedTickBehavior};

use std::collections::HashMap;
use std::future;
use std::io::{self, Stdout};

use std::mem;
use std::time::Duration;

/// Nerd Font glyph and accent color for the user (mage).
const USER_ICON: &str = "\u{f0d0}";
const USER_COLOR: Color = Color::Rgb(120, 170, 255);
const USER_NAME: &str = "DevelAngel";

/// Nerd Font glyph and accent color for the agent (dungeon cook).
const AGENT_ICON: &str = "\u{f0f5}";
const AGENT_COLOR: Color = Color::Rgb(255, 170, 80);
const AGENT_NAME: &str = "Senshi";

/// Rows scrolled per PageUp/PageDown press.
const SCROLL_STEP: u16 = 3;

/// Tool title annotation of `git_commit_with_check` in
/// `commit-workflow-mcp/src/main.rs`. Must match exactly; the two crates
/// aren't linked, so this is the trigger for opening a fix session.
const GIT_COMMIT_WITH_CHECK_TITLE: &str = "Git Commit With Check";

/// Workflow name for the fix session opened on a failed
/// `git_commit_with_check`. Shown in the session banner and prompt title.
const COMMIT_FIX_WORKFLOW: &str = "commit-fix-rust";

/// Fixed first prompt sent to a freshly opened commit-fix session, followed
/// by the `[AUTO: ...]` block carrying the original commit message.
const COMMIT_FIX_INSTRUCTIONS: &str = "\
The main session's `git_commit_with_check` failed. You are a fresh session \
started to fix it. Run `cargo check`, `cargo clippy`, and `cargo test` (or \
re-invoke `git_commit_with_check` directly) to see the current failure. Fix \
the underlying issue, not just the symptom. Once check, lint, and test all \
pass, invoke `git_commit_with_check` again with the same commit message to \
finish the commit.";

/// Extracts the `message` field from a `git_commit_with_check` tool call's
/// JSON parameters. Returns an empty string if parsing fails or the field
/// is absent, rather than failing the whole trigger.
fn extract_commit_message(parameters: Option<&str>) -> String {
    parameters
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A permission request awaiting the user's decision.
struct PendingPermission {
    options: Vec<PermissionOption>,
    reply: oneshot::Sender<Option<String>>,
}

struct Confetti {
    frame: u8,
    particles: Vec<(u16, u16, char)>,
}

impl Confetti {
    const DURATION: u8 = 60;
    const PARTICLES: usize = 128;

    fn new() -> Self {
        let mut rng = rand::rng();
        let particles = (0..Self::PARTICLES)
            .map(|_| {
                let x = rng.random_range(0..=1000);
                let y = rng.random_range(0..=1000);
                let symbol = match rng.random_range(0..3) {
                    0 => '*',
                    1 => '+',
                    _ => '·',
                };
                (x, y, symbol)
            })
            .collect();
        Self {
            frame: 0,
            particles,
        }
    }

    fn tick(&mut self) -> bool {
        let mut rng = rand::rng();
        for (x, _, _) in &mut self.particles {
            match rng.random_range(0..3) {
                0 => *x = x.saturating_sub(1),
                1 => *x = x.saturating_add(1).min(1000),
                _ => {}
            }
        }
        self.frame = self.frame.saturating_add(1);
        self.frame < Self::DURATION
    }
}
/// TUI application state.
struct App {
    chat_log: ChatLog,
    prompt: TextArea<'static>,
    agent_buffer: String,
    /// Where the viewport was anchored at the last redraw, resolved
    /// against the current layout so it survives bubble height changes
    /// (e.g. a tool cluster settling and collapsing) without jumping.
    scroll_anchor: Option<ScrollAnchor>,
    /// Unapplied row delta from PageUp/PageDown, consumed on the next
    /// redraw.
    pending_scroll_delta: i16,
    pending_permission: Option<PendingPermission>,
    /// Whether the view tracks new messages automatically. Disabled by
    /// navigating to older content; re-enabled via [`KeyCode::End`].
    autoscroll: bool,
    should_quit: bool,
    tool_call_ids: HashMap<ToolCallId, EntryId>,
    confetti: Option<Confetti>,
    spinner_phase: usize,
    /// Index into `chat_log.messages()` of the `ToolCluster` currently
    /// navigated via Ctrl+↑/↓, if any. `None` means the prompt has focus.
    focused_cluster: Option<usize>,
    focused_tool_call: Option<EntryId>,
    tool_call_popup: Option<EntryId>,
    log_buffer: LogBuffer,
    log_popup: bool,
    log_popup_scroll: u16,
    tool_call_popup_scroll: u16,
    /// The entry ID of the last thought step, for live appending of subsequent
    /// thought chunks. Cleared when a non-Thought event arrives.
    last_thought_entry_id: Option<EntryId>,
    /// The entry ID of the last agent message, for live appending of subsequent
    /// speech chunks. Cleared when a non-Chunk event arrives.
    last_agent_message_entry_id: Option<EntryId>,
}

impl App {
    fn new() -> Self {
        Self {
            chat_log: ChatLog::new(),
            prompt: new_prompt_textarea(None),
            agent_buffer: String::new(),
            scroll_anchor: None,
            pending_scroll_delta: 0,
            pending_permission: None,
            autoscroll: true,
            should_quit: false,
            tool_call_ids: HashMap::new(),
            confetti: None,
            spinner_phase: 0,
            focused_cluster: None,
            focused_tool_call: None,
            tool_call_popup: None,
            tool_call_popup_scroll: 0,
            log_buffer: LogBuffer::default(),
            log_popup: false,
            log_popup_scroll: 0,
            last_thought_entry_id: None,
            last_agent_message_entry_id: None,
        }
    }

    /// Like [`App::new`], but backed by an existing, externally shared
    /// [`LogBuffer`] instead of a fresh one.
    fn with_log_buffer(log_buffer: LogBuffer) -> Self {
        Self {
            log_buffer,
            ..Self::new()
        }
    }

    fn handle_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Confetti => {
                tracing::debug!("event: confetti");
                self.last_thought_entry_id = None;
                self.last_agent_message_entry_id = None;
                self.confetti = Some(Confetti::new());
            }

            SessionEvent::Chunk(block) => {
                let mut text = PromptRunner::content_block_to_string(&block);

                // Trim leading newlines only on first chunk of a message
                if self.last_agent_message_entry_id.is_none() {
                    text = text.trim_start().to_string();
                }

                if text.is_empty() {
                    return; // Skip whitespace-only chunks
                }

                tracing::debug!(
                    len = text.len(),
                    has_entry = self.last_agent_message_entry_id.is_some(),
                    "event: chunk"
                );
                self.last_thought_entry_id = None;

                if let Some(entry_id) = self.last_agent_message_entry_id {
                    // Append to existing agent message
                    self.chat_log.append_to_agent(entry_id, &text);
                } else {
                    // Create new agent message and remember its ID
                    let entry_id = self.chat_log.push_agent(text);
                    self.last_agent_message_entry_id = Some(entry_id);
                }
            }
            SessionEvent::PermissionRequest { options, reply } => {
                tracing::debug!(option_count = options.len(), "event: permission_request");
                self.last_thought_entry_id = None;
                self.last_agent_message_entry_id = None;
                self.pending_permission = Some(PendingPermission { options, reply });
            }
            SessionEvent::Stopped(reason) => {
                tracing::debug!(?reason, "event: stopped");
                self.last_thought_entry_id = None;
                self.last_agent_message_entry_id = None;
                if !self.agent_buffer.is_empty() {
                    self.chat_log.push_agent(mem::take(&mut self.agent_buffer));
                }
                if reason != StopReason::EndTurn {
                    self.chat_log
                        .push_session_notice(SessionNoticeKind::Stopped, stop_reason_text(reason));
                }
            }
            SessionEvent::Error(error) => {
                tracing::debug!(%error, "event: error");
                self.last_thought_entry_id = None;
                self.last_agent_message_entry_id = None;
                if !self.agent_buffer.is_empty() {
                    self.chat_log.push_agent(mem::take(&mut self.agent_buffer));
                }
                self.chat_log.push_session_notice(
                    SessionNoticeKind::Error,
                    format!("Session failed: {error}"),
                );
            }
            SessionEvent::Thought(block) => {
                let text = PromptRunner::content_block_to_string(&block);
                tracing::debug!(
                    len = text.len(),
                    has_entry = self.last_thought_entry_id.is_some(),
                    "event: thought"
                );
                self.last_agent_message_entry_id = None;
                self.flush_agent_buffer();

                if let Some(entry_id) = self.last_thought_entry_id {
                    // Append to existing thought step
                    self.chat_log.append_to_thought(entry_id, &text);
                } else {
                    // Create new thought step and remember its ID
                    let entry_id = self.chat_log.push_thought(text);
                    self.last_thought_entry_id = Some(entry_id);
                }
            }
            SessionEvent::ToolCall {
                id,
                title,
                status,
                parameters,
                result,
            } => {
                tracing::debug!(%title, %id, ?status, "event: tool_call");
                self.last_thought_entry_id = None;
                self.last_agent_message_entry_id = None;
                self.flush_agent_buffer();
                let entry_id = self
                    .chat_log
                    .push_tool_call_with_parameters(title, parameters);
                self.chat_log
                    .update_tool_call_status(entry_id, map_tool_call_status(status));
                if let Some(result) = result {
                    self.chat_log.update_tool_call_result(entry_id, result);
                }
                self.tool_call_ids.insert(id, entry_id);
            }
            SessionEvent::ToolCallUpdate {
                id,
                status,
                parameters,
                result,
            } => {
                tracing::debug!(%id, "event: tool_call_update");
                if let Some(&entry_id) = self.tool_call_ids.get(&id) {
                    if let Some(status) = status {
                        self.chat_log
                            .update_tool_call_status(entry_id, map_tool_call_status(status));
                    }
                    if let Some(parameters) = parameters {
                        self.chat_log
                            .update_tool_call_parameters(entry_id, parameters);
                    }
                    if let Some(result) = result {
                        self.chat_log.update_tool_call_result(entry_id, result);
                    }
                }
            }
        }
    }

    /// Commits streamed speech before a non-speech event. This preserves the
    /// original event order and prevents text on opposite sides of a thought
    /// or tool call from being concatenated into one speech bubble.
    fn flush_agent_buffer(&mut self) {
        if !self.agent_buffer.is_empty() {
            self.chat_log.push_agent(mem::take(&mut self.agent_buffer));
        }
    }

    fn handle_key(&mut self, key: KeyEvent, session: &SessionHandle) {
        if self.pending_permission.is_some() {
            handle_permission_key(key.code, &mut self.pending_permission);
            return;
        }

        if self.tool_call_popup.is_some() {
            self.handle_tool_call_popup_key(key);
            return;
        }

        if self.log_popup {
            self.handle_log_popup_key(key);
            return;
        }

        if let Some(focused) = self.focused_cluster {
            self.handle_cluster_focus_key(key, focused);
            return;
        }

        match key.code {
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.focused_cluster = last_cluster_index(&self.chat_log);
                self.autoscroll = false;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.log_popup = true;
                self.log_popup_scroll = 0;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                session.cancel();
            }
            KeyCode::Enter => {
                let prompt_text = self.prompt.lines().join(" ").trim().to_owned();
                if prompt_text.is_empty() {
                    return;
                }
                if matches!(prompt_text.as_str(), ":q" | ":quit") {
                    self.should_quit = true;
                    return;
                }
                self.prompt = new_prompt_textarea(session.workflow_name());
                self.chat_log.push_user(prompt_text.clone());
                if session.send_prompt(prompt_text).is_err() {
                    self.chat_log.push_agent("[session closed]");
                    self.should_quit = true;
                }
            }
            KeyCode::PageUp => {
                self.pending_scroll_delta =
                    self.pending_scroll_delta.saturating_sub(SCROLL_STEP as i16);
                self.autoscroll = false;
            }
            KeyCode::PageDown => {
                self.pending_scroll_delta =
                    self.pending_scroll_delta.saturating_add(SCROLL_STEP as i16);
            }
            KeyCode::End => {
                self.autoscroll = true;
            }
            KeyCode::Esc => {
                session.cancel();
                self.prompt = new_prompt_textarea(session.workflow_name());
            }
            _ => {
                self.prompt.input(key);
            }
        }
    }

    /// Applies a key press while a tool cluster has focus. Ctrl+↑/↓ moves
    /// between clusters; Enter/Space expands or collapses the selected
    /// cluster. Once expanded, ↑/↓ selects tool calls and Enter opens their
    /// audit details while Space collapses the cluster. Esc leaves cluster
    /// focus and resumes autoscroll, since Ctrl+↑/↓ disabled it to enter.
    fn handle_cluster_focus_key(&mut self, key: KeyEvent, focused: usize) {
        let expanded = matches!(
            self.chat_log.messages().get(focused),
            Some(Message::ToolCluster(cluster)) if cluster.expanded()
        );
        let tool_calls = self.chat_log.tool_call_ids(focused);

        match key.code {
            KeyCode::Esc => {
                self.focused_cluster = None;
                self.focused_tool_call = None;
                self.autoscroll = true;
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(prev) = cluster_index_before(&self.chat_log, focused) {
                    self.focused_cluster = Some(prev);
                    self.focused_tool_call = None;
                }
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(next) = cluster_index_after(&self.chat_log, focused) {
                    self.focused_cluster = Some(next);
                    self.focused_tool_call = None;
                } else {
                    self.focused_cluster = None;
                    self.focused_tool_call = None;
                }
            }
            KeyCode::Up if expanded => {
                self.focused_tool_call = previous_tool_call(&tool_calls, self.focused_tool_call);
            }
            KeyCode::Down if expanded => {
                self.focused_tool_call = next_tool_call(&tool_calls, self.focused_tool_call);
            }

            KeyCode::Up if !expanded => {
                if let Some(prev) = cluster_index_before(&self.chat_log, focused) {
                    self.focused_cluster = Some(prev);
                    self.focused_tool_call = None;
                }
            }
            KeyCode::Down if !expanded => {
                if let Some(next) = cluster_index_after(&self.chat_log, focused) {
                    self.focused_cluster = Some(next);
                    self.focused_tool_call = None;
                }
            }

            KeyCode::Enter if expanded => {
                if let Some(entry_id) = self.focused_tool_call {
                    self.tool_call_popup = Some(entry_id);
                    self.tool_call_popup_scroll = 0;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.chat_log.toggle_cluster(focused);
                self.focused_tool_call = if expanded {
                    None
                } else {
                    tool_calls.first().copied()
                };
            }
            _ => {}
        }
    }

    fn handle_tool_call_popup_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.tool_call_popup = None;
                self.tool_call_popup_scroll = 0;
            }
            KeyCode::Up => {
                self.tool_call_popup_scroll = self.tool_call_popup_scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                self.tool_call_popup_scroll = self.tool_call_popup_scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.tool_call_popup_scroll =
                    self.tool_call_popup_scroll.saturating_sub(SCROLL_STEP);
            }
            KeyCode::PageDown => {
                self.tool_call_popup_scroll =
                    self.tool_call_popup_scroll.saturating_add(SCROLL_STEP);
            }
            _ => {}
        }
    }

    fn handle_log_popup_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.log_popup = false;
                self.log_popup_scroll = 0;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.log_popup = false;
                self.log_popup_scroll = 0;
            }
            KeyCode::Up => {
                self.log_popup_scroll = self.log_popup_scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                self.log_popup_scroll = self.log_popup_scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.log_popup_scroll = self.log_popup_scroll.saturating_sub(SCROLL_STEP);
            }
            KeyCode::PageDown => {
                self.log_popup_scroll = self.log_popup_scroll.saturating_add(SCROLL_STEP);
            }
            _ => {}
        }
    }
}

/// Index of the last `ToolCluster` message in `chat_log`, if any.
fn last_cluster_index(chat_log: &ChatLog) -> Option<usize> {
    chat_log
        .messages()
        .iter()
        .rposition(|m| matches!(m, Message::ToolCluster(_)))
}

/// Index of the nearest `ToolCluster` message before `index`, if any.
fn cluster_index_before(chat_log: &ChatLog, index: usize) -> Option<usize> {
    chat_log.messages()[..index]
        .iter()
        .rposition(|m| matches!(m, Message::ToolCluster(_)))
}

/// Index of the nearest `ToolCluster` message after `index`, if any.
fn cluster_index_after(chat_log: &ChatLog, index: usize) -> Option<usize> {
    chat_log.messages()[index + 1..]
        .iter()
        .position(|m| matches!(m, Message::ToolCluster(_)))
        .map(|offset| index + 1 + offset)
}

fn previous_tool_call(tool_calls: &[EntryId], selected: Option<EntryId>) -> Option<EntryId> {
    match selected.and_then(|id| tool_calls.iter().position(|candidate| *candidate == id)) {
        Some(index) => index
            .checked_sub(1)
            .and_then(|index| tool_calls.get(index).copied()),
        None => tool_calls.last().copied(),
    }
}

fn next_tool_call(tool_calls: &[EntryId], selected: Option<EntryId>) -> Option<EntryId> {
    match selected.and_then(|id| tool_calls.iter().position(|candidate| *candidate == id)) {
        Some(index) => tool_calls
            .get(index + 1)
            .copied()
            .or_else(|| tool_calls.last().copied()),
        None => tool_calls.first().copied(),
    }
}

/// Maps an ACP tool call status onto the chat log's protocol-agnostic
/// [`Status`]. Non-exhaustive future variants default to [`Status::Pending`].
fn map_tool_call_status(status: ToolCallStatus) -> Status {
    match status {
        ToolCallStatus::Pending => Status::Pending,
        ToolCallStatus::InProgress => Status::Running,
        ToolCallStatus::Completed => Status::Done,
        ToolCallStatus::Failed => Status::Failed,
        _ => Status::Pending,
    }
}

fn stop_reason_text(reason: StopReason) -> String {
    match reason {
        StopReason::Cancelled => "Session cancelled.".to_string(),
        StopReason::MaxTokens => "Session stopped: maximum tokens reached.".to_string(),
        StopReason::MaxTurnRequests => {
            "Session stopped: maximum turn requests reached.".to_string()
        }
        StopReason::Refusal => "Session stopped: agent refused the request.".to_string(),
        StopReason::EndTurn => "".to_string(),
        _ => format!("Session stopped: {reason:?}."),
    }
}

/// Title shows `workflow_name` when set, so it's clear at a glance which
/// session Enter routes the prompt to.
fn new_prompt_textarea(workflow_name: Option<&str>) -> TextArea<'static> {
    let title = match workflow_name {
        Some(name) => format!(" {USER_ICON} Prompt \u{2192} {name} "),
        None => format!(" {USER_ICON} Prompt "),
    };
    let mut textarea = TextArea::default();
    textarea.set_block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(USER_COLOR))
            .title(Span::styled(
                title,
                Style::default().fg(USER_COLOR).add_modifier(Modifier::BOLD),
            )),
    );
    textarea.set_cursor_line_style(Style::default());
    textarea.set_placeholder_text("Type a message, or :q / :quit to exit");
    textarea.set_placeholder_style(
        Style::default()
            .fg(Color::DarkGray)
            .bg(Color::Rgb(40, 40, 50)),
    );
    textarea
}

/// Applies a key press while a permission popup is showing, resolving and
/// clearing `pending` when an option is chosen or the request is cancelled.
fn handle_permission_key(key: KeyCode, pending: &mut Option<PendingPermission>) {
    let Some(permission) = pending.take() else {
        return;
    };

    match key {
        KeyCode::Char(c) if c.is_ascii_digit() => {
            let index = c.to_digit(10).unwrap_or(0) as usize;
            if index >= 1 && index <= permission.options.len() {
                let option_id = permission.options[index - 1].option_id.clone();
                let _ = permission.reply.send(Some(option_id.to_string()));
                return;
            }
            *pending = Some(permission);
        }
        KeyCode::Esc => {
            let _ = permission.reply.send(None);
        }
        _ => {
            *pending = Some(permission);
        }
    }
}

/// Reads terminal events on a blocking task and forwards them to an
/// unbounded channel, so the async loop can `select!` on them.
fn spawn_terminal_events() -> UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    task::spawn_blocking(move || {
        while let Ok(ev) = event::read() {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
    rx
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    main_session: &mut SessionHandle,
    agent_config: &agent_client_protocol::AcpAgentConfig,
    term_events: &mut UnboundedReceiver<Event>,
) -> io::Result<()> {
    let mut spinner = time::interval(Duration::from_millis(250));
    spinner.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut confetti = time::interval(Duration::from_millis(50));
    confetti.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut fix_session: Option<SessionHandle> = None;

    while !app.should_quit {
        terminal.draw(|frame| frame.draw_app(app))?;
        let fix_recv = async {
            match fix_session.as_mut() {
                Some(session) => session.recv_event().await,
                None => future::pending().await,
            }
        };
        tokio::select! {
            _ = spinner.tick() => {
                app.spinner_phase = app.spinner_phase.wrapping_add(1);
            }
            _ = confetti.tick() => {
                if let Some(confetti) = app.confetti.as_mut() && !confetti.tick() {
                    app.confetti = None;
                }
            }
            Some(session_event) = main_session.recv_event() => {
                if fix_session.is_none()
                    && let SessionEvent::ToolCall { title, parameters, .. } = &session_event
                    && title == GIT_COMMIT_WITH_CHECK_TITLE
                {
                    let commit_message = extract_commit_message(parameters.as_deref());
                    app.handle_session_event(session_event);
                    fix_session = Some(
                        open_commit_fix_session(app, main_session, agent_config, commit_message)
                            .await,
                    );
                } else {
                    app.handle_session_event(session_event);
                }
            }
            Some(session_event) = fix_recv => {
                app.handle_session_event(session_event);
            }
            Some(term_event) = term_events.recv() => {
                if let Event::Key(key) = term_event
                    && key.kind == KeyEventKind::Press
                {
                    let active_session = fix_session.as_ref().unwrap_or(main_session);
                    app.handle_key(key, active_session);
                }
            }
        }
    }

    Ok(())
}

/// Cancels the main session's current turn, waits for it to actually stop,
/// then opens and seeds a fix session. Blocking on the cancel confirmation
/// avoids a race between the main session's own event stream and the new
/// fix session's first events.
async fn open_commit_fix_session(
    app: &mut App,
    main_session: &mut SessionHandle,
    agent_config: &agent_client_protocol::AcpAgentConfig,
    commit_message: String,
) -> SessionHandle {
    main_session.cancel();
    while let Some(event) = main_session.recv_event().await {
        let is_cancelled = matches!(event, SessionEvent::Stopped(StopReason::Cancelled));
        app.handle_session_event(event);
        if is_cancelled {
            break;
        }
    }

    let fix_component = AcpAgent::new(agent_config.clone());
    let fix_session =
        start_interactive_session(fix_component, true, Some(COMMIT_FIX_WORKFLOW.to_owned()));

    app.chat_log.push_session_transition(COMMIT_FIX_WORKFLOW);
    app.prompt = new_prompt_textarea(Some(COMMIT_FIX_WORKFLOW));

    let seed_prompt = format!(
        "{COMMIT_FIX_INSTRUCTIONS}\n\n[AUTO: Commit Message from Main Session]\n{commit_message}"
    );
    let _ = fix_session.send_prompt(seed_prompt);

    fix_session
}

/// Draws application state onto a ratatui `Frame`.
trait DrawApp {
    /// Renders the chat bubbles, prompt textarea, and permission popup
    /// (if any).
    fn draw_app(&mut self, app: &mut App);
    fn draw_confetti(&mut self, confetti: &Confetti, area: Rect);

    /// Renders the chat log as scrollable speech bubbles.
    fn draw_chat_log(&mut self, app: &mut App, area: Rect);

    /// Renders the permission popup over the given area.
    fn draw_permission_popup(&mut self, pending: &PendingPermission, area: Rect);
    /// Renders a tool call audit popup over the given area.
    fn draw_tool_call_popup(
        &mut self,
        entry: &kid_agentic_coding::ToolCallEntry,
        scroll: u16,
        area: Rect,
    );

    /// Renders the log popup over the given area.
    fn draw_log_popup(&mut self, lines: &[String], scroll: u16, area: Rect);
}

impl DrawApp for Frame<'_> {
    fn draw_confetti(&mut self, confetti: &Confetti, area: Rect) {
        for (index, &(base_x, base_y, symbol)) in confetti.particles.iter().enumerate() {
            let x = u32::from(base_x) * u32::from(area.width.max(1)) / 1001;
            let upper_height = area.height.max(1).div_ceil(2);
            let y = u32::from(base_y) * u32::from(upper_height) / 1001 + u32::from(confetti.frame);
            if y >= u32::from(area.height) {
                continue;
            }
            let color = match index % 4 {
                0 => Color::Yellow,
                1 => Color::Green,
                2 => Color::Cyan,
                _ => Color::Magenta,
            };
            let particle_area = Rect::new(area.x + x as u16, area.y + y as u16, 1, 1);
            self.render_widget(
                Paragraph::new(symbol.to_string()).style(Style::default().fg(color)),
                particle_area,
            );
        }
    }

    fn draw_app(&mut self, app: &mut App) {
        let [log_area, input_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(self.area());
        if let Some(confetti) = app.confetti.as_ref() {
            self.draw_confetti(confetti, self.area());
        }

        self.draw_chat_log(app, log_area);
        self.render_widget(&app.prompt, input_area);

        if let Some(pending) = &app.pending_permission {
            self.draw_permission_popup(pending, self.area());
        }

        if let Some(entry_id) = app.tool_call_popup
            && let Some(entry) = app.chat_log.tool_call(entry_id)
        {
            self.draw_tool_call_popup(entry, app.tool_call_popup_scroll, self.area());
        }

        if app.log_popup {
            self.draw_log_popup(&app.log_buffer.lines(), app.log_popup_scroll, self.area());
        }
    }

    fn draw_chat_log(&mut self, app: &mut App, area: Rect) {
        let (area, banner_area) = if app.autoscroll {
            (area, None)
        } else {
            let [log_area, banner_area] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
            (log_area, Some(banner_area))
        };

        let mut render_log = app.chat_log.clone();
        if !app.agent_buffer.is_empty() {
            render_log.push_agent(app.agent_buffer.clone());
        }

        let mut layout = BubbleLayout::new(&render_log, area.width, area.height);

        if app.autoscroll {
            if let Some(anchor) = app.scroll_anchor {
                layout.scroll_to_anchor(anchor);
            }
            layout.extend_to_bottom();
        } else {
            if let Some(anchor) = app.scroll_anchor {
                layout.scroll_to_anchor(anchor);
            }
            if app.pending_scroll_delta != 0 {
                layout.scroll(app.pending_scroll_delta);
                app.pending_scroll_delta = 0;
            }

            // Keep the focused cluster fully in view, so Ctrl+↑/↓
            // navigation scrolls the viewport instead of leaving the
            // selection off-screen.
            if let Some(focused) = app.focused_cluster
                && let Some(bubble) = layout.bubbles().get(focused)
            {
                let bubble_top = bubble.rect.y;
                let bubble_bottom = bubble_top.saturating_add(bubble.rect.height);
                let current = layout.scroll_offset();
                let viewport_bottom = current.saturating_add(area.height);
                let target = if bubble_top < current {
                    Some(bubble_top)
                } else if bubble_bottom > viewport_bottom {
                    Some(bubble_bottom.saturating_sub(area.height))
                } else {
                    None
                };
                if let Some(target) = target {
                    let delta = (i32::from(target) - i32::from(current))
                        .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                        as i16;
                    layout.scroll(delta);
                }
            }
        }

        app.scroll_anchor = layout.anchor();

        if let Some(banner_area) = banner_area {
            self.render_widget(
                Paragraph::new("\u{2193} New messages \u{b7} End to jump to latest")
                    .style(Style::default().fg(Color::Black).bg(Color::Yellow)),
                banner_area,
            );
        }

        let messages = render_log.messages().iter();
        let visible = layout.visible_bubbles().into_iter();
        for (index, (message, visible_bubble)) in messages.zip(visible).enumerate() {
            let Some(visible_bubble) = visible_bubble else {
                continue;
            };

            let render_rect = Rect {
                x: area.x + visible_bubble.screen_rect.x,
                y: area.y + visible_bubble.screen_rect.y,
                width: visible_bubble.screen_rect.width,
                height: visible_bubble.screen_rect.height,
            };

            match message {
                Message::User(m) => {
                    self.render_widget(
                        bubble_paragraph(
                            USER_ICON,
                            USER_NAME,
                            USER_COLOR,
                            &m.text,
                            &visible_bubble,
                        ),
                        render_rect,
                    );
                }
                Message::Agent(m) => {
                    self.render_widget(
                        bubble_paragraph(
                            AGENT_ICON,
                            AGENT_NAME,
                            AGENT_COLOR,
                            &m.text,
                            &visible_bubble,
                        ),
                        render_rect,
                    );
                }

                Message::ToolCluster(cluster) => {
                    let is_focused = app.focused_cluster == Some(index);
                    let keep_live = index + 1 == render_log.len();

                    let selected_step = app
                        .focused_tool_call
                        .and_then(|id| app.chat_log.tool_call_step_index(index, id));
                    let text = render_tool_cluster(
                        cluster,
                        is_focused,
                        keep_live,
                        selected_step,
                        app.spinner_phase,
                        render_rect.width,
                    );
                    let paragraph = Paragraph::new(text).scroll((visible_bubble.text_line_skip, 0));
                    self.render_widget(paragraph, render_rect);
                }
                Message::SessionNotice(m) => {
                    let style = match m.kind {
                        SessionNoticeKind::Error => Style::default().fg(Color::Red),
                        SessionNoticeKind::Stopped => Style::default().fg(Color::Yellow),
                    };
                    self.render_widget(Paragraph::new(m.text.as_str()).style(style), render_rect);
                }
                Message::SessionTransition(t) => {
                    let title = format!(" \u{2699} {} (main paused) ", t.workflow_name);
                    let width = render_rect.width as usize;
                    let fill = width.saturating_sub(title.chars().count());
                    let left = fill / 2;
                    let right = fill - left;
                    let line = Line::from(vec![
                        Span::styled("\u{2500}".repeat(left), Style::default().fg(Color::Yellow)),
                        Span::styled(
                            title,
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("\u{2500}".repeat(right), Style::default().fg(Color::Yellow)),
                    ]);
                    self.render_widget(Paragraph::new(line), render_rect);
                }
            }
        }

        let mut scrollbar_state = ScrollbarState::new(layout.total_height() as usize)
            .viewport_content_length(area.height as usize)
            .position(layout.scroll_offset() as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        self.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }

    fn draw_permission_popup(&mut self, pending: &PendingPermission, area: Rect) {
        let popup_area = centered_rect(60, 40, area);

        let items: Vec<ListItem> = pending
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| ListItem::new(format!("{}. {}", index + 1, option.name)))
            .collect();

        let popup = List::new(items).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title("Permission requested (Esc to cancel)")
                .style(Style::default().fg(Color::Yellow)),
        );

        self.render_widget(Clear, popup_area);
        self.render_widget(popup, popup_area);
    }

    fn draw_tool_call_popup(
        &mut self,
        entry: &kid_agentic_coding::ToolCallEntry,
        scroll: u16,
        area: Rect,
    ) {
        let popup_area = centered_rect(90, 85, area);
        let (status_icon, _, status) = status_style(entry.status);
        let parameters = entry
            .parameters
            .as_deref()
            .unwrap_or("No parameters supplied.");
        let result_label = if entry.status == Status::Failed {
            "Result (failed)"
        } else {
            "Result"
        };
        let result = entry.result.as_deref().unwrap_or("Not available yet.");
        let text = format!(
            "Status: {status_icon} {status}\n\nParameters\n{parameters}\n\n{result_label}\n{result}"
        );
        let paragraph = Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(format!(" Tool Call: {} ", entry.name)),
            );
        self.render_widget(Clear, popup_area);
        self.render_widget(paragraph, popup_area);
    }

    fn draw_log_popup(&mut self, lines: &[String], scroll: u16, area: Rect) {
        let popup_area = centered_rect(90, 85, area);
        let text = if lines.is_empty() {
            Text::raw("No log output yet.")
        } else {
            let mut text = Text::default();
            for line in lines {
                match line.as_bytes().to_vec().into_text() {
                    Ok(parsed) => text.extend(parsed),
                    Err(_) => text.push_line(Line::raw(line.clone())),
                }
            }
            text
        };

        let paragraph = Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(" Logs (Esc or Ctrl+L to close) "),
            );
        self.render_widget(Clear, popup_area);
        self.render_widget(paragraph, popup_area);
    }
}
/// Builds the framed paragraph for a User/Agent bubble.
fn bubble_paragraph<'a>(
    icon: &str,
    name: &str,
    color: Color,
    text: &'a str,
    visible_bubble: &VisibleBubble,
) -> Paragraph<'a> {
    let block = Block::default()
        .borders(visible_bubble.borders)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(
            format!(" {icon} {name} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));

    Paragraph::new(text)
        .wrap(Wrap { trim: true })
        .scroll((visible_bubble.text_line_skip, 0))
        .block(block)
}

/// Strips a leading `name` from `comment`, along with an optional `:`
/// and surrounding whitespace, so a self-labeled tool result like
/// `"run_tests: ok"` doesn't repeat the name already shown next to it.
fn strip_redundant_name<'a>(comment: &'a str, name: &str) -> &'a str {
    let Some(rest) = comment.strip_prefix(name) else {
        return comment;
    };
    let rest = rest.trim_start();
    rest.strip_prefix(':').unwrap_or(rest).trim_start()
}

/// Renders a tool cluster as a summary line followed by its visible steps.
/// `keep_live` defers the collapse of a just-settled cluster while it is
/// still the last message.
fn render_tool_cluster(
    cluster: &ToolCluster,
    is_focused: bool,
    keep_live: bool,
    selected_step: Option<usize>,
    spinner_phase: usize,
    width: u16,
) -> Text<'static> {
    let (icon, color, _) = status_style(cluster.status());
    let icon = animated_status_icon(cluster.status(), spinner_phase, icon);
    let count = cluster.tool_call_count();
    let label = match count {
        0 => "Thinking..".to_owned(),
        1 => "Calling 1 Tool..".to_owned(),
        n => format!("Calling {n} Tools.."),
    };
    let marker = if is_focused { "\u{25b8}" } else { icon };
    let mut summary_style = Style::default().fg(color);
    if is_focused {
        summary_style = summary_style.fg(Color::White).add_modifier(Modifier::BOLD);
    }
    let summary = Span::styled(format!("{marker} {label}"), summary_style);

    let shown = cluster.visible_steps(keep_live);
    if shown.is_empty() {
        return Text::from(Line::from(summary));
    }

    let hidden = cluster.steps().len().saturating_sub(shown.len());
    let mut lines = vec![Line::from(summary)];
    if hidden > 0 {
        lines.push(Line::from(Span::styled(
            "\u{22ee}",
            Style::default().fg(Color::White),
        )));
    }
    for (index, step) in shown.iter().enumerate() {
        let actual_index = hidden + index;
        let is_last = index + 1 == shown.len();
        let corner = if is_last { "\u{2570}" } else { "\u{251c}" };
        let selected = selected_step == Some(actual_index);
        let (line_color, dashes, text) = match step {
            Step::Thought(text) => (
                Color::White,
                "\u{2500}\u{2500}",
                format!("\u{1f914} {text}"),
            ),
            Step::ToolCall(entry) => {
                let (status_icon, _, _) = status_style(entry.status);
                let status_icon = animated_status_icon(entry.status, spinner_phase, status_icon);
                let comment = entry.result.as_deref().and_then(|r| r.lines().next());
                let comment = comment.map(|c| strip_redundant_name(c, &entry.name));
                let text = match comment {
                    Some(comment) if !comment.is_empty() => {
                        format!("\u{1f527} {} {status_icon} \u{2014} {comment}", entry.name)
                    }
                    _ => format!("\u{1f527} {} {status_icon}", entry.name),
                };
                (Color::White, "\u{2500}\u{2500}", text)
            }
        };
        let style = if selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(line_color)
        };
        lines.extend(
            textwrap::fill(
                &text,
                Options::new(width.max(1) as usize)
                    .initial_indent(&format!("{corner}{dashes} "))
                    .subsequent_indent("│  "),
            )
            .lines()
            .map(|line| Line::from(Span::styled(line.to_owned(), style))),
        );
    }
    Text::from(lines)
}

fn running_icon(phase: usize) -> &'static str {
    const SPINNER: [&str; 4] = ["\u{25d0}", "\u{25d3}", "\u{25d1}", "\u{25d2}"];
    SPINNER[phase % SPINNER.len()]
}

/// Uses the same spinner for every running tool status in the live view.
fn animated_status_icon(
    status: Status,
    spinner_phase: usize,
    static_icon: &'static str,
) -> &'static str {
    if status == Status::Running {
        running_icon(spinner_phase)
    } else {
        static_icon
    }
}

fn status_style(status: Status) -> (&'static str, Color, &'static str) {
    match status {
        Status::Pending => ("◌", Color::DarkGray, "pending"),
        Status::Running => ("⧖", Color::Yellow, "running"),
        Status::Done => ("✓", Color::Green, "done"),
        Status::Failed => ("✗", Color::Red, "failed"),
    }
}

#[cfg(test)]
mod tool_status_icon_tests {
    use super::{animated_status_icon, running_icon, status_style};
    use kid_agentic_coding::Status;

    #[test]
    fn running_tool_status_uses_the_live_spinner() {
        let (static_icon, _, _) = status_style(Status::Running);

        assert_eq!(
            animated_status_icon(Status::Running, 1, static_icon),
            running_icon(1)
        );
        assert_ne!(
            animated_status_icon(Status::Running, 1, static_icon),
            animated_status_icon(Status::Running, 2, static_icon)
        );
    }

    #[test]
    fn settled_tool_status_keeps_its_static_icon() {
        let (static_icon, _, _) = status_style(Status::Done);

        assert_eq!(
            animated_status_icon(Status::Done, 1, static_icon),
            static_icon
        );
    }
}

#[cfg(test)]
mod commit_fix_trigger_tests {
    use super::extract_commit_message;

    #[test]
    fn extracts_the_message_field_from_json_parameters() {
        let parameters = r#"{"message": "fix: correct the thing"}"#;

        assert_eq!(
            extract_commit_message(Some(parameters)),
            "fix: correct the thing"
        );
    }

    #[test]
    fn returns_empty_string_for_missing_parameters() {
        assert_eq!(extract_commit_message(None), "");
    }

    #[test]
    fn returns_empty_string_for_malformed_json() {
        assert_eq!(extract_commit_message(Some("not json")), "");
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [_, vertical, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(area);

    let [_, horizontal, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(vertical);

    horizontal
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

/// Runs the interactive terminal UI against the given agent component until
/// the user quits, restoring the terminal afterwards regardless of outcome.
pub async fn run(agent: AcpAgent, log_buffer: LogBuffer, disable_confetti: bool) -> io::Result<()> {
    let agent_config = agent.config().clone();
    let mut session = start_interactive_session(agent, disable_confetti, None);
    let mut term_events = spawn_terminal_events();
    let mut terminal = setup_terminal()?;
    let mut app = App::with_log_buffer(log_buffer);

    let result = run_app(
        &mut terminal,
        &mut app,
        &mut session,
        &agent_config,
        &mut term_events,
    )
    .await;

    restore_terminal(&mut terminal)?;

    result
}

#[cfg(test)]
mod handle_key_tests {
    use super::{App, SCROLL_STEP};
    use agent_client_protocol::schema::v1::{StopReason, ToolCallId, ToolCallStatus};
    use kid_agentic_coding::{Message, SessionEvent, SessionHandle, SessionNoticeKind};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn push_tool_call(app: &mut App, id: &str, name: &str) {
        app.handle_session_event(SessionEvent::ToolCall {
            id: ToolCallId::new(id.to_owned()),
            title: name.to_owned(),
            status: ToolCallStatus::Pending,
            parameters: None,
            result: None,
        });
    }

    fn test_session() -> SessionHandle {
        SessionHandle::new_disconnected_for_test()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(app: &mut App, session: &SessionHandle, text: &str) {
        for c in text.chars() {
            app.handle_key(key(KeyCode::Char(c)), session);
        }
    }

    #[test]
    fn session_error_is_added_to_chat_history() {
        let mut app = App::new();

        app.handle_session_event(SessionEvent::Error("connection lost".to_owned()));

        assert!(matches!(
            &app.chat_log.messages()[0],
            Message::SessionNotice(notice)
                if notice.kind == SessionNoticeKind::Error
                    && notice.text == "Session failed: connection lost"
        ));
    }

    #[test]
    fn non_normal_stop_is_added_to_chat_history() {
        let mut app = App::new();

        app.handle_session_event(SessionEvent::Stopped(StopReason::Cancelled));

        assert!(matches!(
            &app.chat_log.messages()[0],
            Message::SessionNotice(notice)
                if notice.kind == SessionNoticeKind::Stopped
                    && notice.text == "Session cancelled."
        ));
    }

    #[test]
    fn normal_stop_is_not_added_to_chat_history() {
        let mut app = App::new();

        app.handle_session_event(SessionEvent::Stopped(StopReason::EndTurn));

        assert!(app.chat_log.is_empty());
    }

    #[test]
    fn esc_clears_prompt_without_quitting() {
        let mut app = App::new();
        let (session, mut cancel_rx) = SessionHandle::new_cancelable_for_test();
        type_text(&mut app, &session, "hello");
        app.handle_key(key(KeyCode::Esc), &session);

        assert!(cancel_rx.try_recv().is_ok());
        assert!(!app.should_quit);
        assert!(app.prompt.lines().join(" ").trim().is_empty());
    }

    #[test]
    fn ctrl_c_cancels_the_current_turn() {
        let mut app = App::new();
        let (session, mut cancel_rx) = SessionHandle::new_cancelable_for_test();

        app.handle_key(ctrl_key(KeyCode::Char('c')), &session);

        assert!(!app.should_quit);
        assert!(cancel_rx.try_recv().is_ok());
    }

    #[test]
    fn colon_q_quits_on_enter() {
        let mut app = App::new();
        let session = test_session();

        type_text(&mut app, &session, ":q");
        app.handle_key(key(KeyCode::Enter), &session);

        assert!(app.should_quit);
    }

    #[test]
    fn colon_quit_quits_on_enter() {
        let mut app = App::new();
        let session = test_session();

        type_text(&mut app, &session, ":quit");
        app.handle_key(key(KeyCode::Enter), &session);

        assert!(app.should_quit);
    }

    #[test]
    fn regular_prompt_does_not_quit() {
        let mut app = App::new();
        let (session, _prompt_rx) = SessionHandle::new_connected_for_test();

        type_text(&mut app, &session, "hello agent");
        app.handle_key(key(KeyCode::Enter), &session);

        assert!(!app.should_quit);
        assert_eq!(app.chat_log.messages().len(), 1);
    }

    #[test]
    fn ctrl_up_without_a_cluster_is_a_no_op() {
        let mut app = App::new();
        let session = test_session();

        app.handle_key(ctrl_key(KeyCode::Up), &session);

        assert_eq!(app.focused_cluster, None);
    }

    #[test]
    fn ctrl_up_focuses_the_last_tool_cluster() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");

        app.handle_key(ctrl_key(KeyCode::Up), &session);

        assert_eq!(app.focused_cluster, Some(0));
    }

    #[test]
    fn autoscroll_is_active_by_default() {
        let app = App::new();

        assert!(app.autoscroll);
    }

    #[test]
    fn page_up_disables_autoscroll() {
        let mut app = App::new();
        let session = test_session();

        app.handle_key(key(KeyCode::PageUp), &session);

        assert!(!app.autoscroll);
    }

    #[test]
    fn ctrl_up_disables_autoscroll() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");

        app.handle_key(ctrl_key(KeyCode::Up), &session);

        assert!(!app.autoscroll);
    }

    #[test]
    fn end_reenables_autoscroll() {
        let mut app = App::new();
        let session = test_session();
        app.handle_key(key(KeyCode::PageUp), &session);

        app.handle_key(key(KeyCode::End), &session);

        assert!(app.autoscroll);
    }

    #[test]
    fn page_down_does_not_reenable_autoscroll() {
        let mut app = App::new();
        let session = test_session();
        app.handle_key(key(KeyCode::PageUp), &session);

        app.handle_key(key(KeyCode::PageDown), &session);

        assert!(!app.autoscroll);
    }

    #[test]
    fn up_down_navigate_between_clusters_skipping_other_messages() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");
        app.chat_log.push_agent("done with the first step");
        push_tool_call(&mut app, "call-2", "write_file");

        app.handle_key(ctrl_key(KeyCode::Up), &session);
        assert_eq!(app.focused_cluster, Some(2));

        app.handle_key(key(KeyCode::Up), &session);
        assert_eq!(app.focused_cluster, Some(0));

        app.handle_key(key(KeyCode::Down), &session);
        assert_eq!(app.focused_cluster, Some(2));
    }

    #[test]
    fn down_at_the_last_cluster_stays_put() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");

        app.handle_key(ctrl_key(KeyCode::Up), &session);
        app.handle_key(key(KeyCode::Down), &session);

        assert_eq!(app.focused_cluster, Some(0));
    }

    #[test]
    fn enter_toggles_the_focused_cluster() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");
        app.handle_key(ctrl_key(KeyCode::Up), &session);

        app.handle_key(key(KeyCode::Enter), &session);

        let Message::ToolCluster(cluster) = &app.chat_log.messages()[0] else {
            panic!("expected a tool cluster");
        };
        assert!(cluster.expanded());
    }

    #[test]
    fn space_toggles_the_focused_cluster() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");
        app.handle_key(ctrl_key(KeyCode::Up), &session);

        app.handle_key(key(KeyCode::Char(' ')), &session);

        let Message::ToolCluster(cluster) = &app.chat_log.messages()[0] else {
            panic!("expected a tool cluster");
        };
        assert!(cluster.expanded());
    }

    #[test]
    fn ctrl_down_returns_focus_to_the_prompt() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");
        app.handle_key(ctrl_key(KeyCode::Up), &session);

        app.handle_key(ctrl_key(KeyCode::Down), &session);

        assert_eq!(app.focused_cluster, None);
    }

    #[test]
    fn esc_returns_focus_to_the_prompt_without_clearing_it() {
        let mut app = App::new();
        let session = test_session();
        type_text(&mut app, &session, "hello");
        push_tool_call(&mut app, "call-1", "read_file");
        app.handle_key(ctrl_key(KeyCode::Up), &session);

        app.handle_key(key(KeyCode::Esc), &session);

        assert_eq!(app.focused_cluster, None);
        assert_eq!(app.prompt.lines().join(" ").trim(), "hello");
    }

    #[test]
    fn esc_reenables_autoscroll_when_leaving_cluster_focus() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");
        app.handle_key(ctrl_key(KeyCode::Up), &session);
        assert!(!app.autoscroll);

        app.handle_key(key(KeyCode::Esc), &session);

        assert!(app.autoscroll);
    }

    #[test]
    fn expanded_cluster_opens_selected_tool_call_details() {
        let mut app = App::new();
        push_tool_call(&mut app, "call-1", "read_file");

        app.handle_key(ctrl_key(KeyCode::Up), &test_session());
        app.handle_key(key(KeyCode::Enter), &test_session());
        app.handle_key(key(KeyCode::Enter), &test_session());

        assert!(app.tool_call_popup.is_some());

        app.handle_key(key(KeyCode::Esc), &test_session());
        assert!(app.tool_call_popup.is_none());
    }

    #[test]
    fn tool_call_popup_opens_for_tool_call_with_result() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "run_tests");
        app.handle_session_event(SessionEvent::ToolCallUpdate {
            id: ToolCallId::new("call-1".to_owned()),
            parameters: None,
            status: Some(ToolCallStatus::Failed),
            result: Some("assertion failed: left != right".to_owned()),
        });

        app.handle_key(ctrl_key(KeyCode::Up), &session);
        app.handle_key(key(KeyCode::Enter), &session);
        app.handle_key(key(KeyCode::Enter), &session);

        assert!(app.tool_call_popup.is_some());
        assert_eq!(
            app.chat_log
                .tool_call(app.tool_call_popup.expect("popup should be open"))
                .and_then(|entry| entry.result.as_deref()),
            Some("assertion failed: left != right")
        );
    }

    #[test]
    fn tool_call_popup_scrolls_multiline_result() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");
        app.handle_session_event(SessionEvent::ToolCallUpdate {
            id: ToolCallId::new("call-1".to_owned()),
            parameters: None,
            status: Some(ToolCallStatus::Completed),
            result: Some("line one\nline two\nline three".to_owned()),
        });

        app.handle_key(ctrl_key(KeyCode::Up), &session);
        app.handle_key(key(KeyCode::Enter), &session);
        app.handle_key(key(KeyCode::Enter), &session);
        app.handle_key(key(KeyCode::PageDown), &session);

        assert!(app.tool_call_popup.is_some());
        assert_eq!(app.tool_call_popup_scroll, SCROLL_STEP);
    }
    #[test]
    fn typing_while_focused_on_a_cluster_does_not_reach_the_prompt() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");
        app.handle_key(ctrl_key(KeyCode::Up), &session);

        type_text(&mut app, &session, "x");

        assert!(app.prompt.lines().join(" ").trim().is_empty());
    }

    #[test]
    fn ctrl_l_opens_the_log_popup() {
        let mut app = App::new();
        let session = test_session();

        app.handle_key(ctrl_key(KeyCode::Char('l')), &session);

        assert!(app.log_popup);
    }

    #[test]
    fn esc_closes_the_log_popup() {
        let mut app = App::new();
        let session = test_session();
        app.handle_key(ctrl_key(KeyCode::Char('l')), &session);

        app.handle_key(key(KeyCode::Esc), &session);

        assert!(!app.log_popup);
    }

    #[test]
    fn ctrl_l_again_closes_the_log_popup() {
        let mut app = App::new();
        let session = test_session();
        app.handle_key(ctrl_key(KeyCode::Char('l')), &session);

        app.handle_key(ctrl_key(KeyCode::Char('l')), &session);

        assert!(!app.log_popup);
    }

    #[test]
    fn page_down_scrolls_the_log_popup() {
        let mut app = App::new();
        let session = test_session();
        app.handle_key(ctrl_key(KeyCode::Char('l')), &session);

        app.handle_key(key(KeyCode::PageDown), &session);

        assert_eq!(app.log_popup_scroll, SCROLL_STEP);
    }

    #[test]
    fn page_down_does_not_scroll_when_log_popup_is_closed() {
        let mut app = App::new();
        let session = test_session();

        app.handle_key(key(KeyCode::PageDown), &session);

        assert_eq!(app.log_popup_scroll, 0);
    }
}

#[cfg(test)]
mod session_event_tests {
    use super::{App, map_tool_call_status, render_tool_cluster, strip_redundant_name};
    use agent_client_protocol::schema::v1::{
        ContentBlock, TextContent, ToolCallId, ToolCallStatus,
    };
    use kid_agentic_coding::{Message, SessionEvent, Status, Step, ToolCluster};

    fn thought(text: &str) -> SessionEvent {
        SessionEvent::Thought(Box::new(ContentBlock::Text(TextContent::new(
            text.to_owned(),
        ))))
    }

    fn chunk(text: &str) -> SessionEvent {
        SessionEvent::Chunk(Box::new(ContentBlock::Text(TextContent::new(
            text.to_owned(),
        ))))
    }

    fn tool_cluster(app: &App, message_index: usize) -> &ToolCluster {
        let Message::ToolCluster(cluster) = &app.chat_log.messages()[message_index] else {
            panic!("expected a tool cluster at index {message_index}");
        };
        cluster
    }

    fn nth_tool_call(cluster: &ToolCluster, index: usize) -> &kid_agentic_coding::ToolCallEntry {
        let mut calls = cluster.steps().iter().filter_map(|step| match step {
            Step::ToolCall(entry) => Some(entry),
            Step::Thought(_) => None,
        });
        calls
            .nth(index)
            .expect("expected a tool call at that index")
    }

    #[test]
    fn spoken_text_is_flushed_around_thoughts_and_tool_calls() {
        let mut app = App::new();

        app.handle_session_event(chunk("I will check that."));
        app.handle_session_event(thought("checking the project files"));
        app.handle_session_event(chunk("I found the relevant code."));
        app.handle_session_event(SessionEvent::ToolCall {
            id: ToolCallId::new("call-1".to_owned()),
            title: "read_file".to_owned(),
            status: ToolCallStatus::Pending,
            parameters: None,
            result: None,
        });
        app.handle_session_event(chunk("Here is the fix."));
        app.handle_session_event(SessionEvent::Stopped(
            agent_client_protocol::schema::v1::StopReason::EndTurn,
        ));

        assert_eq!(app.chat_log.len(), 5);
        assert!(
            matches!(&app.chat_log.messages()[0], Message::Agent(message)
            if message.text == "I will check that.")
        );
        assert!(matches!(
            app.chat_log.messages()[1],
            Message::ToolCluster(_)
        ));
        assert!(
            matches!(&app.chat_log.messages()[2], Message::Agent(message)
            if message.text == "I found the relevant code.")
        );
        assert!(matches!(
            app.chat_log.messages()[3],
            Message::ToolCluster(_)
        ));
        assert!(
            matches!(&app.chat_log.messages()[4], Message::Agent(message)
            if message.text == "Here is the fix.")
        );
    }

    #[test]
    fn tool_call_event_appends_entry_with_mapped_status() {
        let mut app = App::new();
        let id = ToolCallId::new("call-1".to_owned());

        app.handle_session_event(SessionEvent::ToolCall {
            id,
            title: "read_file".to_owned(),
            parameters: Some("{\"path\":\"src/lib.rs\"}".to_owned()),
            status: ToolCallStatus::InProgress,
            result: None,
        });

        assert_eq!(app.chat_log.len(), 1);
        let cluster = tool_cluster(&app, 0);
        assert_eq!(cluster.tool_call_count(), 1);
        let entry = nth_tool_call(cluster, 0);
        assert_eq!(entry.name, "read_file");
        assert_eq!(entry.status, Status::Running);
        assert_eq!(
            entry.parameters.as_deref(),
            Some("{\"path\":\"src/lib.rs\"}")
        );
    }

    #[test]
    fn tool_call_update_changes_status_of_the_matching_entry() {
        let mut app = App::new();
        let id = ToolCallId::new("call-1".to_owned());

        app.handle_session_event(SessionEvent::ToolCall {
            id: id.clone(),
            title: "read_file".to_owned(),
            parameters: Some("{\"path\":\"src/lib.rs\"}".to_owned()),
            status: ToolCallStatus::Pending,
            result: None,
        });
        app.handle_session_event(SessionEvent::ToolCall {
            id: ToolCallId::new("call-2".to_owned()),
            title: "write_file".to_owned(),
            parameters: Some("{\"path\":\"src/main.rs\"}".to_owned()),
            status: ToolCallStatus::Pending,
            result: None,
        });
        app.handle_session_event(SessionEvent::ToolCallUpdate {
            id,
            parameters: Some("{\"path\":\"src/lib.rs\"}".to_owned()),
            status: Some(ToolCallStatus::Completed),
            result: None,
        });
        let entry = nth_tool_call(tool_cluster(&app, 0), 0);
        assert_eq!(entry.status, Status::Done);
        assert_eq!(
            entry.parameters.as_deref(),
            Some("{\"path\":\"src/lib.rs\"}")
        );
        let second = nth_tool_call(tool_cluster(&app, 0), 1);
        assert_eq!(second.name, "write_file");
        assert_eq!(
            second.parameters.as_deref(),
            Some("{\"path\":\"src/main.rs\"}")
        );
    }

    #[test]
    fn tool_call_update_for_unknown_id_is_a_no_op() {
        let mut app = App::new();
        app.handle_session_event(SessionEvent::ToolCall {
            id: ToolCallId::new("call-1".to_owned()),
            title: "read_file".to_owned(),
            parameters: None,
            status: ToolCallStatus::Pending,
            result: None,
        });

        app.handle_session_event(SessionEvent::ToolCallUpdate {
            id: ToolCallId::new("call-unknown".to_owned()),
            parameters: None,
            status: Some(ToolCallStatus::Completed),
            result: None,
        });

        assert_eq!(
            nth_tool_call(tool_cluster(&app, 0), 0).status,
            Status::Pending
        );
    }

    #[test]
    fn tool_call_update_without_status_is_a_no_op() {
        let mut app = App::new();
        let id = ToolCallId::new("call-1".to_owned());
        app.handle_session_event(SessionEvent::ToolCall {
            id: id.clone(),
            title: "read_file".to_owned(),
            parameters: None,
            status: ToolCallStatus::Pending,
            result: None,
        });

        app.handle_session_event(SessionEvent::ToolCallUpdate {
            id,
            status: None,
            parameters: None,
            result: None,
        });

        assert_eq!(
            nth_tool_call(tool_cluster(&app, 0), 0).status,
            Status::Pending
        );
    }

    #[test]
    fn tool_call_result_on_creation_is_stored_on_the_entry() {
        let mut app = App::new();

        app.handle_session_event(SessionEvent::ToolCall {
            id: ToolCallId::new("call-1".to_owned()),
            title: "read_file".to_owned(),
            parameters: None,
            status: ToolCallStatus::Completed,
            result: Some("file contents".to_owned()),
        });

        let entry = nth_tool_call(tool_cluster(&app, 0), 0);
        assert_eq!(entry.result.as_deref(), Some("file contents"));
    }

    #[test]
    fn tool_call_update_stores_successful_result() {
        let mut app = App::new();
        let id = ToolCallId::new("call-1".to_owned());
        app.handle_session_event(SessionEvent::ToolCall {
            id: id.clone(),
            title: "run_tests".to_owned(),
            parameters: None,
            status: ToolCallStatus::InProgress,
            result: None,
        });

        app.handle_session_event(SessionEvent::ToolCallUpdate {
            id,
            status: Some(ToolCallStatus::Completed),
            parameters: None,
            result: Some("3 passed; 0 failed".to_owned()),
        });

        let entry = nth_tool_call(tool_cluster(&app, 0), 0);
        assert_eq!(entry.status, Status::Done);
        assert_eq!(entry.result.as_deref(), Some("3 passed; 0 failed"));
    }

    #[test]
    fn tool_cluster_renders_result_on_the_same_line_as_the_tool_call() {
        let mut app = App::new();
        app.handle_session_event(SessionEvent::ToolCall {
            id: ToolCallId::new("call-1".to_owned()),
            title: "Shell command".to_owned(),
            parameters: None,
            status: ToolCallStatus::Completed,
            result: Some("command output".to_owned()),
        });

        let rendered = render_tool_cluster(tool_cluster(&app, 0), false, true, None, 0, 80);
        assert!(rendered.lines.iter().any(|line| {
            let text = line.to_string();
            text.contains("Shell command") && text.contains("command output")
        }));

        let entry = nth_tool_call(tool_cluster(&app, 0), 0);
        assert_eq!(entry.result.as_deref(), Some("command output"));
    }

    #[test]
    fn tool_cluster_strips_redundant_name_prefix_from_result_comment() {
        let mut app = App::new();
        app.handle_session_event(SessionEvent::ToolCall {
            id: ToolCallId::new("call-1".to_owned()),
            title: "run_tests".to_owned(),
            parameters: None,
            status: ToolCallStatus::Completed,
            result: Some(
                "run_tests: ok (12 words) with a long result comment that must wrap".to_owned(),
            ),
        });

        let rendered = render_tool_cluster(tool_cluster(&app, 0), false, true, None, 0, 20);
        assert!(
            rendered.lines.len() > 2
                && rendered
                    .lines
                    .iter()
                    .all(|line| line.to_string().chars().count() <= 20)
                && rendered
                    .lines
                    .iter()
                    .map(|line| line.to_string())
                    .map(|text| text.matches("run_tests").count())
                    .sum::<usize>()
                    == 1
        );
        assert!(
            rendered
                .lines
                .iter()
                .skip(2)
                .all(|line| { line.to_string().starts_with("│  ") })
        );
    }

    #[test]
    fn strip_redundant_name_handles_colon_and_plain_prefixes() {
        assert_eq!(strip_redundant_name("run_tests: ok", "run_tests"), "ok");
        assert_eq!(strip_redundant_name("run_tests ok", "run_tests"), "ok");
        assert_eq!(strip_redundant_name("run_tests:ok", "run_tests"), "ok");
        assert_eq!(
            strip_redundant_name("other message", "run_tests"),
            "other message"
        );
    }

    #[test]
    fn tool_call_update_stores_failure_result() {
        let mut app = App::new();
        let id = ToolCallId::new("call-1".to_owned());
        app.handle_session_event(SessionEvent::ToolCall {
            id: id.clone(),
            title: "run_tests".to_owned(),
            parameters: None,
            status: ToolCallStatus::InProgress,
            result: None,
        });

        app.handle_session_event(SessionEvent::ToolCallUpdate {
            id,
            status: Some(ToolCallStatus::Failed),
            parameters: None,
            result: Some("assertion failed: left != right".to_owned()),
        });

        let entry = nth_tool_call(tool_cluster(&app, 0), 0);
        assert_eq!(entry.status, Status::Failed);
        assert_eq!(
            entry.result.as_deref(),
            Some("assertion failed: left != right")
        );
    }

    #[test]
    fn map_tool_call_status_covers_the_known_variants() {
        assert_eq!(
            map_tool_call_status(ToolCallStatus::Pending),
            Status::Pending
        );
        assert_eq!(
            map_tool_call_status(ToolCallStatus::InProgress),
            Status::Running
        );
        assert_eq!(
            map_tool_call_status(ToolCallStatus::Completed),
            Status::Done
        );
        assert_eq!(map_tool_call_status(ToolCallStatus::Failed), Status::Failed);
    }

    #[test]
    fn consecutive_thoughts_are_merged_into_single_cluster() {
        let mut app = App::new();

        // Emit 3 consecutive Thought events
        app.handle_session_event(thought("checking the code"));
        app.handle_session_event(thought("analyzing the structure"));
        app.handle_session_event(thought("found the issue"));

        // After 3 thoughts, should have 1 cluster with all thoughts combined
        assert_eq!(app.chat_log.len(), 1);

        let Message::ToolCluster(cluster) = &app.chat_log.messages()[0] else {
            panic!("expected a tool cluster with thoughts");
        };

        // All 3 thoughts should be combined into ONE thought step
        let thought_count = cluster
            .steps()
            .iter()
            .filter(|step| matches!(step, Step::Thought(_)))
            .count();
        assert_eq!(thought_count, 1);

        // The combined thought should contain all 3 texts separated by spaces
        if let Step::Thought(text) = &cluster.steps()[0] {
            assert!(text.contains("checking the code"));
            assert!(text.contains("analyzing the structure"));
            assert!(text.contains("found the issue"));
        } else {
            panic!("expected a thought step");
        }
    }

    #[test]
    fn tool_call_event_flushes_pending_thoughts() {
        let mut app = App::new();

        app.handle_session_event(thought("thinking about this"));
        app.handle_session_event(SessionEvent::ToolCall {
            id: ToolCallId::new("call-1".to_owned()),
            title: "run_tests".to_owned(),
            status: ToolCallStatus::Pending,
            parameters: None,
            result: None,
        });

        assert_eq!(app.chat_log.len(), 1);

        let Message::ToolCluster(cluster) = &app.chat_log.messages()[0] else {
            panic!("expected a tool cluster");
        };

        // Should have thought followed by tool call
        assert!(matches!(cluster.steps()[0], Step::Thought(_)));
        assert!(matches!(cluster.steps()[1], Step::ToolCall(_)));
    }
}

#[cfg(test)]
mod confetti_tests {
    use super::*;

    #[test]
    fn confetti_advances_and_expires() {
        let mut confetti = Confetti::new();
        assert!(confetti.tick());
        assert_eq!(confetti.frame, 1);

        for _ in 1..Confetti::DURATION {
            confetti.tick();
        }

        assert_eq!(confetti.frame, Confetti::DURATION);
        assert!(!confetti.tick());
    }

    #[test]
    fn confetti_event_starts_animation() {
        let mut app = App::new();
        assert!(app.confetti.is_none());

        app.handle_session_event(SessionEvent::Confetti);

        assert_eq!(
            app.confetti.as_ref().map(|confetti| confetti.frame),
            Some(0)
        );
    }
}
