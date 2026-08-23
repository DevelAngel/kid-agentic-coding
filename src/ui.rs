//! Interactive terminal UI for an ACP session.

use agent_client_protocol::schema::v1::PermissionOption;
use agent_client_protocol::{Client, ConnectTo};
use kid_agentic_coding::{
    BubbleLayout, ChatLog, Message, PromptRunner, SessionEvent, SessionHandle,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
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
    prompt: ratatui_textarea::TextArea<'static>,
    agent_buffer: String,
    scroll_offset: u16,
    pending_permission: Option<PendingPermission>,
    should_quit: bool,
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
        }
    }

    fn handle_key(&mut self, key: KeyEvent, session: &SessionHandle) {
        if self.pending_permission.is_some() {
            handle_permission_key(key.code, &mut self.pending_permission);
            return;
        }

        match key.code {
            KeyCode::Enter => {
                let prompt_text = self.prompt.lines().join(" ").trim().to_owned();
                if prompt_text.is_empty() {
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
            KeyCode::Esc => self.should_quit = true,
            _ => {
                self.prompt.input(key);
            }
        }
    }
}

/// Builds a fresh single-line prompt textarea with the mage-themed block.
fn new_prompt_textarea() -> ratatui_textarea::TextArea<'static> {
    let mut textarea = ratatui_textarea::TextArea::default();
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

        let mut layout = BubbleLayout::new(&render_log, area.width, area.height);
        let delta = (i32::from(app.scroll_offset) - i32::from(layout.scroll_offset()))
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        layout.scroll(delta);
        app.scroll_offset = layout.scroll_offset();

        let messages = render_log.messages().iter();
        let visible = layout.visible_bubbles().into_iter();
        for (message, visible_bubble) in messages.zip(visible) {
            let Some(visible_bubble) = visible_bubble else {
                continue;
            };

            let render_rect = Rect {
                x: area.x + visible_bubble.screen_rect.x,
                y: area.y + visible_bubble.screen_rect.y,
                width: visible_bubble.screen_rect.width,
                height: visible_bubble.screen_rect.height,
            };

            let (icon, name, color, text) = match message {
                Message::User(m) => (USER_ICON, USER_NAME, USER_COLOR, m.text.as_str()),
                Message::Agent(m) => (AGENT_ICON, AGENT_NAME, AGENT_COLOR, m.text.as_str()),
            };

            let block = Block::default()
                .borders(visible_bubble.borders)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color))
                .title(Span::styled(
                    format!(" {icon} {name} "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ));

            let paragraph = Paragraph::new(text)
                .wrap(Wrap { trim: true })
                .scroll((visible_bubble.text_line_skip, 0))
                .block(block);
            self.render_widget(paragraph, render_rect);
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
    let mut session = kid_agentic_coding::start_interactive_session(component);
    let mut term_events = spawn_terminal_events();
    let mut terminal = setup_terminal()?;
    let mut app = App::new();

    let result = run_app(&mut terminal, &mut app, &mut session, &mut term_events).await;

    restore_terminal(&mut terminal)?;

    result
}
