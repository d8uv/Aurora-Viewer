use crate::model::{Editor, Model, clean};
use aurora_i18n::tr;
use aurora_net::login::{LastExec, LoginRequest, Platform, SL_MAIN_GRID, StartLocation};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io::{self, IsTerminal, Stdout};
use uuid::Uuid;

pub enum Approval {
    Tos,
    Critical,
}

#[derive(Default)]
pub struct Login {
    pub fields: [Editor; 3],
    pub selected: usize,
    pub home: bool,
    pub tos: bool,
    pub critical: bool,
    pub approval: Option<Approval>,
    pub message: String,
    pub scroll: u16,
}

impl Login {
    pub fn request(&mut self) -> Option<LoginRequest> {
        if self.fields[0].text.trim().is_empty() || self.fields[1].text.is_empty() {
            self.message = tr!("tui-login-required").into();
            return None;
        }
        let username = self.fields[0].text.trim().to_owned();
        let password = self.fields[1].take();
        let mfa_token = self.fields[2].take();
        // Per-process opaque device ids, never the machine's network or disk ids.
        let mac = Uuid::new_v4().simple().to_string();
        let id0 = Uuid::new_v4().simple().to_string();
        Some(LoginRequest {
            login_uri: SL_MAIN_GRID.into(),
            username,
            password,
            start: if self.home { StartLocation::Home } else { StartLocation::Last },
            agree_to_tos: self.tos,
            read_critical: self.critical,
            mfa_token,
            mfa_hash: String::new(),
            mac,
            id0,
            last_exec: LastExec::default(),
            platform: Platform::default(),
            accepted_next_servers: Vec::new(),
        })
    }

    pub fn key(&mut self, key: KeyEvent) {
        if self.approval.is_some() {
            match key.code {
                KeyCode::Char('y' | 'Y') => {
                    match self.approval.take() {
                        Some(Approval::Tos) => self.tos = true,
                        Some(Approval::Critical) => self.critical = true,
                        None => {}
                    }
                    self.message = tr!("tui-accepted").into();
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    self.approval = None;
                    self.message = tr!("tui-declined").into();
                }
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(8),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Tab => self.selected = (self.selected + 1) % 3,
            KeyCode::BackTab => self.selected = (self.selected + 2) % 3,
            KeyCode::F(2) => self.home = !self.home,
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(8),
            _ => edit(&mut self.fields[self.selected], key),
        }
    }
}

pub fn edit(editor: &mut Editor, key: KeyEvent) {
    match key.code {
        KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => editor.insert(&c.to_string()),
        KeyCode::Backspace => editor.backspace(),
        KeyCode::Delete => editor.delete(),
        KeyCode::Left => editor.left(),
        KeyCode::Right => editor.right(),
        KeyCode::Home => editor.cursor = 0,
        KeyCode::End => editor.cursor = editor.text.len(),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            editor.take();
        }
        _ => {}
    }
}

pub fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen, crossterm::cursor::Show);
}

pub struct Screen {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Screen {
    pub fn new() -> io::Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::other(tr!("tui-needs-terminal")));
        }
        enable_raw_mode()?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste) {
            restore();
            return Err(error);
        }
        match Terminal::new(CrosstermBackend::new(io::stdout())) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                restore();
                Err(error)
            }
        }
    }

    pub fn draw(&mut self, model: &Model, login: &Login, show_login: bool, status: &str) -> io::Result<()> {
        self.terminal.draw(|frame| {
            let areas = Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).split(frame.area());
            frame.render_widget(
                Paragraph::new(format!("Aurora Chat {} | {}", aurora_version::version(), clean(status)))
                    .style(Style::default().add_modifier(Modifier::REVERSED)),
                areas[0],
            );
            if show_login {
                draw_login(frame, areas[1], login);
            } else {
                draw_chat(frame, areas[1], model);
            }
            frame.render_widget(Paragraph::new(tr!("tui-keys")), areas[2]);
        })?;
        Ok(())
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        restore();
    }
}

fn input(frame: &mut Frame<'_>, area: Rect, editor: &Editor, title: &str, secret: bool, focus: bool) {
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    let text = if secret {
        "*".repeat(editor.text.chars().count())
    } else {
        editor.text.to_string()
    };
    let cursor = if secret {
        editor.text[..editor.cursor].chars().count()
    } else {
        Line::from(&editor.text[..editor.cursor]).width()
    };
    let offset = cursor.saturating_sub(usize::from(inner.width.saturating_sub(1)));
    let offset = offset.min(u16::MAX as usize) as u16;
    frame.render_widget(Paragraph::new(text).block(block).scroll((0, offset)), area);
    if focus && inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position((inner.x + (cursor as u16).saturating_sub(offset), inner.y));
    }
}

fn draw_login(frame: &mut Frame<'_>, area: Rect, login: &Login) {
    let areas = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(tr!("tui-login-intro")), areas[0]);
    for (i, title) in [tr!("tui-username"), tr!("tui-password"), tr!("tui-mfa")].into_iter().enumerate() {
        input(
            frame,
            areas[i + 1],
            &login.fields[i],
            title,
            i != 0,
            i == login.selected && login.approval.is_none(),
        );
    }
    let start = if login.home { tr!("tui-home") } else { tr!("tui-last") };
    frame.render_widget(
        Paragraph::new(tr!("tui-login-keys", start = start)).wrap(Wrap { trim: false }),
        areas[4],
    );
    let mut message: Vec<Line<'_>> = login.message.lines().map(|line| Line::from(clean(line))).collect();
    if login.approval.is_some() {
        message.insert(0, Line::from(tr!("tui-approval")));
    }
    let max_scroll = Paragraph::new(message.clone())
        .wrap(Wrap { trim: false })
        .line_count(areas[5].width)
        .saturating_sub(usize::from(areas[5].height));
    frame.render_widget(
        Paragraph::new(message)
            .wrap(Wrap { trim: false })
            .scroll((login.scroll.min(max_scroll.min(u16::MAX as usize) as u16), 0)),
        areas[5],
    );
}

fn draw_chat(frame: &mut Frame<'_>, area: Rect, model: &Model) {
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(area);
    let (sidebar, transcript) = if area.width >= 70 {
        let columns = Layout::horizontal([Constraint::Length(22), Constraint::Min(1)]).split(rows[0]);
        (Some(columns[0]), columns[1])
    } else {
        (None, rows[0])
    };
    if let Some(sidebar) = sidebar {
        let items: Vec<_> = model
            .buffers
            .iter()
            .map(|buffer| {
                let unread = if buffer.unread == 0 {
                    String::new()
                } else {
                    format!(" ({})", buffer.unread)
                };
                ListItem::new(format!("{}{}", buffer.name, unread))
            })
            .collect();
        let mut state = ListState::default().with_selected(Some(model.current));
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(tr!("tui-buffers")))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
            sidebar,
            &mut state,
        );
    }
    let active = model.active();
    let block = Block::default().borders(Borders::ALL).title(active.name.as_str());
    let inner = block.inner(transcript);
    let lines: Vec<Line<'_>> = active.lines.iter().map(|s| Line::from(s.as_str())).collect();
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let height = paragraph.line_count(inner.width);
    let bottom = height.saturating_sub(usize::from(inner.height)).min(u16::MAX as usize) as u16;
    frame.render_widget(paragraph.block(block).scroll((bottom.saturating_sub(active.scroll), 0)), transcript);
    let title = if active.ready {
        tr!("tui-input")
    } else {
        tr!("tui-group-not-ready")
    };
    input(frame, rows[1], &active.draft, title, false, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn login_masks_secrets_and_renders_in_small_terminals() {
        let mut login = Login::default();
        login.fields[1].insert("dummy-password");
        login.fields[2].insert("123456");
        for (width, height) in [(100, 30), (40, 15), (10, 4), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw_login(frame, frame.area(), &login)).unwrap();
            let output: String = terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            assert!(!output.contains("dummy-password"));
            assert!(!output.contains("123456"));
        }
    }

    #[test]
    fn chat_wraps_and_scrolls_without_overflow_at_small_sizes() {
        let mut model = Model::default();
        model.notice(&"桜".repeat(500));
        model.active_mut().draft.insert(&"世界".repeat(100));
        for (width, height) in [(100, 30), (40, 15), (10, 4), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw_chat(frame, frame.area(), &model)).unwrap();
        }
    }
}
