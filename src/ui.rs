//! Interactive terminal UI for an ACP session.

use agent_client_protocol::schema::v1::PermissionOption;
use agent_client_protocol::{Client, ConnectTo};
use kid_agentic_coding::{PromptRunner, SessionEvent, SessionHandle};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use std::io::{self, Stdout};
use std::mem;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{mpsc, oneshot};

/// A permission request awaiting the user's decision.
struct PendingPermission {
    options: Vec<PermissionOption>,
    reply: oneshot::Sender<Option<String>>,
}

/// TUI application state.
#[derive(Default)]
struct App {
    input: String,
    log: Vec<String>,
    agent_buffer: String,
    pending_permission: Option<PendingPermission>,
    should_quit: bool,
}

impl App {
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
                    self.log.push(mem::take(&mut self.agent_buffer));
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyCode, session: &SessionHandle) {
        if self.pending_permission.is_some() {
            handle_permission_key(key, &mut self.pending_permission);
            return;
        }

        match key {
            KeyCode::Enter if !self.input.is_empty() => {
                let prompt_text = mem::take(&mut self.input);
                self.log.push(format!("> {prompt_text}"));
                if session.send_prompt(prompt_text).is_err() {
                    self.log.push("[session closed]".to_owned());
                    self.should_quit = true;
                }
            }
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Esc => self.should_quit = true,
            _ => {}
        }
    }
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
                    app.handle_key(key.code, session);
                }
            }
        }
    }

    Ok(())
}

/// Draws application state onto a ratatui `Frame`.
trait DrawApp {
    /// Renders the session log, input box, and permission popup (if any).
    fn draw_app(&mut self, app: &App);

    /// Renders the permission popup over the given area.
    fn draw_permission_popup(&mut self, pending: &PendingPermission, area: Rect);
}

impl DrawApp for Frame<'_> {
    fn draw_app(&mut self, app: &App) {
        let [log_area, input_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(self.area());

        let mut lines: Vec<ListItem> = app
            .log
            .iter()
            .map(|line| ListItem::new(line.as_str()))
            .collect();
        if !app.agent_buffer.is_empty() {
            lines.push(ListItem::new(app.agent_buffer.as_str()));
        }
        let log =
            List::new(lines).block(Block::default().borders(Borders::ALL).title("Session"));
        self.render_widget(log, log_area);

        let input = Paragraph::new(app.input.as_str())
            .block(Block::default().borders(Borders::ALL).title("Prompt"));
        self.render_widget(input, input_area);

        if let Some(pending) = &app.pending_permission {
            self.draw_permission_popup(pending, self.area());
        }
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
            Block::default()
                .borders(Borders::ALL)
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
    let mut app = App::default();

    let result = run_app(&mut terminal, &mut app, &mut session, &mut term_events).await;

    restore_terminal(&mut terminal)?;

    result
}
