//! Interactive terminal UI for an ACP session.

use agent_client_protocol::schema::v1::{PermissionOption, ToolCallId, ToolCallStatus};
use agent_client_protocol::{Client, ConnectTo};
use kid_agentic_coding::start_interactive_session;
use kid_agentic_coding::{
    BubbleLayout, ChatLog, EntryId, Message, PromptRunner, SessionEvent, SessionHandle, Status,
    ToolCluster, VisibleBubble,
};
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
use std::collections::HashMap;
use std::io::{self, Stdout};
use std::mem;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{mpsc, oneshot};

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

/// A permission request awaiting the user's decision.
struct PendingPermission {
    options: Vec<PermissionOption>,
    reply: oneshot::Sender<Option<String>>,
}

/// TUI application state.
struct App {
    chat_log: ChatLog,
    prompt: TextArea<'static>,
    agent_buffer: String,
    scroll_offset: u16,
    pending_permission: Option<PendingPermission>,
    should_quit: bool,
    tool_call_ids: HashMap<ToolCallId, EntryId>,
    /// Index into `chat_log.messages()` of the `ToolCluster` currently
    /// navigated via Ctrl+↑/↓, if any. `None` means the prompt has focus.
    focused_cluster: Option<usize>,
}

impl App {
    fn new() -> Self {
        Self {
            chat_log: ChatLog::new(),
            prompt: new_prompt_textarea(),
            agent_buffer: String::new(),
            scroll_offset: 0,
            pending_permission: None,
            should_quit: false,
            tool_call_ids: HashMap::new(),
            focused_cluster: None,
        }
    }

    fn handle_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Chunk(block) => {
                self.agent_buffer
                    .push_str(&PromptRunner::content_block_to_string(&block));
            }
            SessionEvent::PermissionRequest { options, reply } => {
                self.pending_permission = Some(PendingPermission { options, reply });
            }
            SessionEvent::Stopped(_) => {
                if !self.agent_buffer.is_empty() {
                    self.chat_log.push_agent(mem::take(&mut self.agent_buffer));
                }
            }
            SessionEvent::Thought(block) => {
                let text = PromptRunner::content_block_to_string(&block);
                self.chat_log.push_thought(text);
            }
            SessionEvent::ToolCall { id, title, status } => {
                let entry_id = self.chat_log.push_tool_call(title);
                self.chat_log
                    .update_tool_call_status(entry_id, map_tool_call_status(status));
                self.tool_call_ids.insert(id, entry_id);
            }
            SessionEvent::ToolCallUpdate { id, status } => {
                if let Some(status) = status
                    && let Some(&entry_id) = self.tool_call_ids.get(&id)
                {
                    self.chat_log
                        .update_tool_call_status(entry_id, map_tool_call_status(status));
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent, session: &SessionHandle) {
        if self.pending_permission.is_some() {
            handle_permission_key(key.code, &mut self.pending_permission);
            return;
        }

        if let Some(focused) = self.focused_cluster {
            self.handle_cluster_focus_key(key, focused);
            return;
        }

        match key.code {
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.focused_cluster = last_cluster_index(&self.chat_log);
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
                self.prompt = new_prompt_textarea();
                self.chat_log.push_user(prompt_text.clone());
                if session.send_prompt(prompt_text).is_err() {
                    self.chat_log.push_agent("[session closed]");
                    self.should_quit = true;
                }
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_sub(SCROLL_STEP);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_add(SCROLL_STEP);
            }
            KeyCode::Esc => self.prompt = new_prompt_textarea(),
            _ => {
                self.prompt.input(key);
            }
        }
    }

    /// Applies a key press while a tool cluster has focus (entered via
    /// Ctrl+↑): Ctrl+↓/Esc return focus to the prompt, ↑/↓ move between
    /// clusters skipping any other message in between, Enter/Space toggle
    /// the focused cluster's expanded state. Everything else is ignored —
    /// typing while a cluster is focused must not reach the prompt.
    fn handle_cluster_focus_key(&mut self, key: KeyEvent, focused: usize) {
        match key.code {
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.focused_cluster = None;
            }
            KeyCode::Esc => {
                self.focused_cluster = None;
            }
            KeyCode::Up => {
                if let Some(prev) = cluster_index_before(&self.chat_log, focused) {
                    self.focused_cluster = Some(prev);
                }
            }
            KeyCode::Down => {
                if let Some(next) = cluster_index_after(&self.chat_log, focused) {
                    self.focused_cluster = Some(next);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.chat_log.toggle_cluster(focused);
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

/// Builds a fresh single-line prompt textarea with the mage-themed block.
fn new_prompt_textarea() -> TextArea<'static> {
    let mut textarea = TextArea::default();
    textarea.set_block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(USER_COLOR))
            .title(Span::styled(
                format!(" {USER_ICON} Prompt "),
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
    tokio::task::spawn_blocking(move || {
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
    session: &mut SessionHandle,
    term_events: &mut UnboundedReceiver<Event>,
) -> io::Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| frame.draw_app(app))?;
        tokio::select! {
            Some(session_event) = session.recv_event() => {
                app.handle_session_event(session_event);
            }
            Some(term_event) = term_events.recv() => {
                if let Event::Key(key) = term_event
                    && key.kind == KeyEventKind::Press
                {
                    app.handle_key(key, session);
                }
            }
        }
    }

    Ok(())
}

/// Draws application state onto a ratatui `Frame`.
trait DrawApp {
    /// Renders the chat bubbles, prompt textarea, and permission popup
    /// (if any).
    fn draw_app(&mut self, app: &mut App);

    /// Renders the chat log as scrollable speech bubbles.
    fn draw_chat_log(&mut self, app: &mut App, area: Rect);

    /// Renders the permission popup over the given area.
    fn draw_permission_popup(&mut self, pending: &PendingPermission, area: Rect);
}

impl DrawApp for Frame<'_> {
    fn draw_app(&mut self, app: &mut App) {
        let [log_area, input_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(self.area());

        self.draw_chat_log(app, log_area);
        self.render_widget(&app.prompt, input_area);

        if let Some(pending) = &app.pending_permission {
            self.draw_permission_popup(pending, self.area());
        }
    }

    fn draw_chat_log(&mut self, app: &mut App, area: Rect) {
        let mut render_log = app.chat_log.clone();
        if !app.agent_buffer.is_empty() {
            render_log.push_agent(app.agent_buffer.clone());
        }

        let layout = BubbleLayout::new(&render_log, area.width, area.height);

        // Keep the focused cluster fully in view before applying the
        // regular scroll delta, so Ctrl+↑/↓ navigation scrolls the
        // viewport instead of leaving the selection off-screen.
        if let Some(focused) = app.focused_cluster
            && let Some(bubble) = layout.bubbles().get(focused)
        {
            let bubble_top = bubble.rect.y;
            let bubble_bottom = bubble_top.saturating_add(bubble.rect.height);
            let viewport_bottom = app.scroll_offset.saturating_add(area.height);
            if bubble_top < app.scroll_offset {
                app.scroll_offset = bubble_top;
            } else if bubble_bottom > viewport_bottom {
                app.scroll_offset = bubble_bottom.saturating_sub(area.height);
            }
        }

        let mut layout = layout;
        let delta = (i32::from(app.scroll_offset) - i32::from(layout.scroll_offset()))
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        layout.scroll(delta);
        app.scroll_offset = layout.scroll_offset();

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
                Message::Thought(text) => {
                    let paragraph = Paragraph::new(text.as_str())
                        .wrap(Wrap { trim: true })
                        .scroll((visible_bubble.text_line_skip, 0))
                        .style(Style::default().fg(Color::DarkGray));
                    self.render_widget(paragraph, render_rect);
                }
                Message::ToolCluster(cluster) => {
                    let is_focused = app.focused_cluster == Some(index);
                    let text = render_tool_cluster(cluster, is_focused);
                    let paragraph = Paragraph::new(text).scroll((visible_bubble.text_line_skip, 0));
                    self.render_widget(paragraph, render_rect);
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

/// The aggregate status of a tool cluster: [`Status::Running`] if any entry
/// is running, else [`Status::Failed`] if any failed, else [`Status::Done`]
/// only if every entry is done, else [`Status::Pending`].
fn cluster_status(cluster: &ToolCluster) -> Status {
    let mut aggregate = Status::Done;
    for entry in cluster.entries() {
        match entry.status {
            Status::Running => return Status::Running,
            Status::Failed => aggregate = Status::Failed,
            Status::Pending if aggregate != Status::Failed => aggregate = Status::Pending,
            Status::Done | Status::Pending => {}
        }
    }
    aggregate
}

/// Renders a tool cluster as a summary line, or a summary line followed by
/// a `├─`/`╰─` tree of its entries when expanded. `is_focused` renders the
/// summary in bold white with a `▸` marker so Ctrl+↑/↓ navigation has a
/// visible target on an otherwise unframed row.
fn render_tool_cluster(cluster: &ToolCluster, is_focused: bool) -> Text<'static> {
    let (icon, color, _) = status_style(cluster_status(cluster));
    let count = cluster.entries().len();
    let noun = if count == 1 { "tool" } else { "tools" };
    let prefix = if is_focused { "\u{25b8} " } else { "  " };
    let mut summary_style = Style::default().fg(color);
    if is_focused {
        summary_style = summary_style
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
    }
    let summary = Span::styled(
        format!("{prefix}{icon} Used KID-Text-Editor \u{b7} {count} {noun}"),
        summary_style,
    );

    if !cluster.expanded() {
        return Text::from(Line::from(summary));
    }

    let mut lines = vec![Line::from(summary)];
    for (index, entry) in cluster.entries().iter().enumerate() {
        let is_last = index + 1 == cluster.entries().len();
        let branch = if is_last {
            "\u{2570}\u{2500} "
        } else {
            "\u{251c}\u{2500} "
        };
        let (entry_icon, entry_color, _) = status_style(entry.status);
        lines.push(Line::from(vec![
            Span::styled(branch, Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{entry_icon} {}", entry.name),
                Style::default().fg(entry_color),
            ),
        ]));
    }
    Text::from(lines)
}

/// Icon, accent color, and status label for a tool call's status.
fn status_style(status: Status) -> (&'static str, Color, &'static str) {
    match status {
        Status::Pending => ("\u{25cb}", Color::DarkGray, "pending"),
        Status::Running => ("\u{25d0}", Color::Yellow, "running"),
        Status::Done => ("\u{25cf}", Color::Green, "done"),
        Status::Failed => ("\u{2717}", Color::Red, "failed"),
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
pub async fn run(component: impl ConnectTo<Client> + 'static) -> io::Result<()> {
    let mut session = start_interactive_session(component);
    let mut term_events = spawn_terminal_events();
    let mut terminal = setup_terminal()?;
    let mut app = App::new();

    let result = run_app(&mut terminal, &mut app, &mut session, &mut term_events).await;

    restore_terminal(&mut terminal)?;

    result
}

#[cfg(test)]
mod handle_key_tests {
    use super::App;
    use agent_client_protocol::schema::v1::{ToolCallId, ToolCallStatus};
    use kid_agentic_coding::{Message, SessionEvent, SessionHandle};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn push_tool_call(app: &mut App, id: &str, name: &str) {
        app.handle_session_event(SessionEvent::ToolCall {
            id: ToolCallId::new(id.to_owned()),
            title: name.to_owned(),
            status: ToolCallStatus::Pending,
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
    fn esc_clears_prompt_without_quitting() {
        let mut app = App::new();
        let session = test_session();

        type_text(&mut app, &session, "hello");
        app.handle_key(key(KeyCode::Esc), &session);

        assert!(!app.should_quit);
        assert!(app.prompt.lines().join(" ").trim().is_empty());
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
    fn up_down_navigate_between_clusters_skipping_other_messages() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");
        app.chat_log.push_thought("checking existing error handling");
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
    fn typing_while_focused_on_a_cluster_does_not_reach_the_prompt() {
        let mut app = App::new();
        let session = test_session();
        push_tool_call(&mut app, "call-1", "read_file");
        app.handle_key(ctrl_key(KeyCode::Up), &session);

        type_text(&mut app, &session, "x");

        assert!(app.prompt.lines().join(" ").trim().is_empty());
    }
}

#[cfg(test)]
mod session_event_tests {
    use super::{App, map_tool_call_status};
    use agent_client_protocol::schema::v1::{
        ContentBlock, TextContent, ToolCallId, ToolCallStatus,
    };
    use kid_agentic_coding::{Message, SessionEvent, Status, ToolCluster};

    fn thought(text: &str) -> SessionEvent {
        SessionEvent::Thought(Box::new(ContentBlock::Text(TextContent::new(
            text.to_owned(),
        ))))
    }

    fn tool_cluster(app: &App, message_index: usize) -> &ToolCluster {
        let Message::ToolCluster(cluster) = &app.chat_log.messages()[message_index] else {
            panic!("expected a tool cluster at index {message_index}");
        };
        cluster
    }

    #[test]
    fn thought_event_appends_a_thought_message() {
        let mut app = App::new();

        app.handle_session_event(thought("checking existing error handling"));

        assert_eq!(app.chat_log.len(), 1);
        assert!(matches!(
            app.chat_log.messages()[0],
            Message::Thought(ref t) if t == "checking existing error handling"
        ));
    }

    #[test]
    fn tool_call_event_appends_entry_with_mapped_status() {
        let mut app = App::new();
        let id = ToolCallId::new("call-1".to_owned());

        app.handle_session_event(SessionEvent::ToolCall {
            id,
            title: "read_file".to_owned(),
            status: ToolCallStatus::InProgress,
        });

        assert_eq!(app.chat_log.len(), 1);
        let cluster = tool_cluster(&app, 0);
        assert_eq!(cluster.entries().len(), 1);
        assert_eq!(cluster.entries()[0].name, "read_file");
        assert_eq!(cluster.entries()[0].status, Status::Running);
    }

    #[test]
    fn tool_call_update_changes_status_of_the_matching_entry() {
        let mut app = App::new();
        let id = ToolCallId::new("call-1".to_owned());

        app.handle_session_event(SessionEvent::ToolCall {
            id: id.clone(),
            title: "read_file".to_owned(),
            status: ToolCallStatus::Pending,
        });
        app.handle_session_event(SessionEvent::ToolCallUpdate {
            id,
            status: Some(ToolCallStatus::Completed),
        });

        assert_eq!(tool_cluster(&app, 0).entries()[0].status, Status::Done);
    }

    #[test]
    fn tool_call_update_for_unknown_id_is_a_no_op() {
        let mut app = App::new();
        app.handle_session_event(SessionEvent::ToolCall {
            id: ToolCallId::new("call-1".to_owned()),
            title: "read_file".to_owned(),
            status: ToolCallStatus::Pending,
        });

        app.handle_session_event(SessionEvent::ToolCallUpdate {
            id: ToolCallId::new("call-unknown".to_owned()),
            status: Some(ToolCallStatus::Completed),
        });

        assert_eq!(tool_cluster(&app, 0).entries()[0].status, Status::Pending);
    }

    #[test]
    fn tool_call_update_without_status_is_a_no_op() {
        let mut app = App::new();
        let id = ToolCallId::new("call-1".to_owned());
        app.handle_session_event(SessionEvent::ToolCall {
            id: id.clone(),
            title: "read_file".to_owned(),
            status: ToolCallStatus::Pending,
        });

        app.handle_session_event(SessionEvent::ToolCallUpdate { id, status: None });

        assert_eq!(tool_cluster(&app, 0).entries()[0].status, Status::Pending);
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
}
