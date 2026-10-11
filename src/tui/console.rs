//! Remote console controls. The runtime owns the listener; this state survives Back.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

use super::app::Effect;
use super::theme::Palette;
use super::widgets::wrap_line;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Access {
    #[default]
    Local,
    Tailscale,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    pub access: Access,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Start {
        options: Options,
        open_browser: bool,
    },
    NewLink {
        open_browser: bool,
    },
    Stop,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PairingLink {
    pub origin: String,
    pub code: String,
    pub expires_at: f64,
}

impl PairingLink {
    pub fn url(&self) -> String {
        format!("{}/#pair={}", self.origin, self.code)
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Link(PairingLink),
    BrowserFailed,
    Stopped(Result<(), String>),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConsoleScreen {
    pub options: Options,
    pub status: Status,
    pub link: Option<PairingLink>,
    pub error: Option<String>,
    cursor: usize,
    link_pending: bool,
    scroll: u16,
    buttons: Vec<Rect>,
}

impl ConsoleScreen {
    pub fn active(&self) -> bool {
        self.status != Status::Stopped
    }

    pub fn receive(&mut self, event: Event) {
        match event {
            Event::Link(link) if matches!(self.status, Status::Starting | Status::Running) => {
                self.status = Status::Running;
                self.link = Some(link);
                self.link_pending = false;
                self.error = None;
            }
            Event::BrowserFailed if self.status == Status::Running => {
                self.error = Some("Could not open the browser. Use the pairing link below.".into());
            }
            Event::Stopped(result) => {
                self.status = Status::Stopped;
                self.link = None;
                self.link_pending = false;
                self.cursor = 0;
                self.scroll = 0;
                self.error = result.err();
            }
            _ => {}
        }
    }

    fn actions(&self) -> Vec<String> {
        match self.status {
            Status::Stopped => vec![
                "Start and open browser".into(),
                "Start without opening browser".into(),
                format!(
                    "Access: {}",
                    match self.options.access {
                        Access::Local => "This computer",
                        Access::Tailscale => "Tailscale",
                    }
                ),
                format!(
                    "Read-only: {}",
                    if self.options.read_only { "On" } else { "Off" }
                ),
                "← back".into(),
            ],
            Status::Running => vec![
                "Open browser (new pairing link)".into(),
                "New pairing link".into(),
                "Stop console".into(),
                "← back".into(),
            ],
            Status::Starting => vec!["Cancel start".into(), "← back".into()],
            Status::Stopping => vec!["← back".into()],
        }
    }

    fn activate(&mut self) -> Vec<Effect> {
        let request = match (self.status, self.cursor) {
            (Status::Stopped, 0 | 1) => {
                self.status = Status::Starting;
                self.error = None;
                Some(Request::Start {
                    options: self.options,
                    open_browser: self.cursor == 0,
                })
            }
            (Status::Stopped, 2) => {
                self.options.access = match self.options.access {
                    Access::Local => Access::Tailscale,
                    Access::Tailscale => Access::Local,
                };
                None
            }
            (Status::Stopped, 3) => {
                self.options.read_only = !self.options.read_only;
                None
            }
            (Status::Running, 0 | 1) if !self.link_pending => {
                self.link_pending = true;
                Some(Request::NewLink {
                    open_browser: self.cursor == 0,
                })
            }
            (Status::Running, 0 | 1) => None,
            (Status::Running, 2) | (Status::Starting, 0) => {
                self.status = Status::Stopping;
                self.link = None;
                Some(Request::Stop)
            }
            _ => return vec![Effect::Pop],
        };
        if request.is_some() {
            self.cursor = 0;
        }
        request.into_iter().map(Effect::Console).collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return vec![Effect::Pop],
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.actions().len() - 1)
            }
            KeyCode::Enter => return self.activate(),
            KeyCode::Char('o') if matches!(self.status, Status::Stopped | Status::Running) => {
                self.cursor = 0;
                return self.activate();
            }
            KeyCode::Char('n') if self.status == Status::Running => {
                self.cursor = 1;
                return self.activate();
            }
            KeyCode::Char('s') if matches!(self.status, Status::Starting | Status::Running) => {
                self.cursor = if self.status == Status::Starting {
                    0
                } else {
                    2
                };
                return self.activate();
            }
            KeyCode::PageDown => self.scroll_by(5),
            KeyCode::PageUp => self.scroll_by(-5),
            _ => {}
        }
        Vec::new()
    }

    pub fn click(&mut self, x: u16, y: u16) -> Vec<Effect> {
        if let Some(index) = self
            .buttons
            .iter()
            .position(|area| area.contains((x, y).into()))
        {
            // A resize or worker event may have changed the set of actions.
            if self.buttons.len() == self.actions().len() {
                self.cursor = index;
                return self.activate();
            }
        }
        Vec::new()
    }

    pub fn scroll_by(&mut self, delta: i16) {
        self.scroll = self.scroll.saturating_add_signed(delta);
    }

    pub fn draw(&mut self, area: Rect, buf: &mut Buffer, now: f64, p: &Palette) {
        let inner = Rect::new(
            area.x + 2.min(area.width),
            area.y,
            area.width.saturating_sub(4),
            area.height,
        );
        let status = match self.status {
            Status::Stopped => "Stopped",
            Status::Starting => "Starting…",
            Status::Running => "Running",
            Status::Stopping => "Stopping…",
        };
        Paragraph::new(format!("Remote Console · {status}"))
            .style(Style::new().fg(p.accent).bold())
            .render(
                Rect {
                    height: 1.min(inner.height),
                    ..inner
                },
                buf,
            );
        let actions = self.actions();
        self.cursor = self.cursor.min(actions.len() - 1);
        self.buttons.clear();
        // Keep the selected action visible even in a short terminal.
        let room = inner.height.saturating_sub(2) as usize;
        let offset = (self.cursor + 1).saturating_sub(room);
        for (index, label) in actions.iter().enumerate() {
            let rect = if index >= offset && index < offset + room {
                Rect::new(
                    inner.x,
                    inner.y + 2 + (index - offset) as u16,
                    inner.width,
                    1,
                )
            } else {
                Rect::default()
            };
            self.buttons.push(rect);
            if rect.is_empty() {
                continue;
            }
            let selected = index == self.cursor;
            Paragraph::new(format!("{} {label}", if selected { "›" } else { " " }))
                .style(if selected {
                    Style::new().fg(p.accent).bg(p.surface)
                } else {
                    Style::new().fg(p.fg)
                })
                .render(rect, buf);
        }
        let top = (actions.len() as u16 + 3).min(inner.height);
        let details = Rect::new(inner.x, inner.y + top, inner.width, inner.height - top);
        let mut lines = Vec::new();
        if let Some(error) = &self.error {
            lines.push(Line::styled(error.clone(), Style::new().fg(p.crit)));
            lines.push(Line::default());
        }
        if let Some(link) = &self.link {
            lines.push(Line::from(format!("Console URL: {}/", link.origin)));
            lines.push(Line::from(format!(
                "Access: {}",
                if self.options.read_only {
                    "Read-only"
                } else {
                    "View + switch accounts"
                }
            )));
            lines.push(Line::default());
            lines.push(Line::from("Pairing link:"));
            lines.push(Line::styled(link.url(), Style::new().fg(p.accent)));
            lines.push(Line::default());
            lines.push(Line::from("Pairing code:"));
            lines.push(Line::from(link.code.clone()));
            let remaining = (link.expires_at - now).ceil().max(0.0) as u64;
            lines.push(Line::styled(
                if remaining == 0 {
                    "Link expired. Open browser or generate a new link.".into()
                } else {
                    format!(
                        "Single use · expires in {}:{:02} if unused",
                        remaining / 60,
                        remaining % 60
                    )
                },
                Style::new().fg(if remaining == 0 { p.crit } else { p.muted }),
            ));
        } else if self.status == Status::Stopped {
            lines.push(Line::from(
                "The browser pairs automatically when you open its link.",
            ));
            lines.push(Line::from("An available port is selected at startup."));
            lines.push(Line::from(
                "Choose Tailscale to connect from another device on your tailnet.",
            ));
        }
        lines.push(Line::default());
        lines.push(Line::styled(
            "Back keeps the console running. Quit TUI to stop it.",
            Style::new().fg(p.muted),
        ));
        let lines: Vec<_> = lines
            .into_iter()
            .flat_map(|line| wrap_line(line, details.width as usize))
            .collect();
        self.scroll = self
            .scroll
            .min(lines.len().saturating_sub(details.height as usize) as u16);
        Paragraph::new(lines)
            .scroll((self.scroll, 0))
            .render(details, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn press(screen: &mut ConsoleScreen, code: KeyCode) -> Vec<Effect> {
        screen.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn start_cancel_and_late_link_do_not_reopen_console() {
        let mut screen = ConsoleScreen::default();
        assert_eq!(
            press(&mut screen, KeyCode::Enter),
            vec![Effect::Console(Request::Start {
                options: Options::default(),
                open_browser: true
            })]
        );
        assert_eq!(
            press(&mut screen, KeyCode::Char('s')),
            vec![Effect::Console(Request::Stop)]
        );
        screen.receive(Event::Link(PairingLink {
            origin: "http://127.0.0.1:1234".into(),
            code: "unused".into(),
            expires_at: 300.0,
        }));
        assert_eq!(screen.status, Status::Stopping);
        assert!(screen.link.is_none());
        screen.receive(Event::Stopped(Ok(())));
        assert!(!screen.active());
    }

    #[test]
    fn link_requests_are_bounded_and_browser_failure_keeps_manual_link() {
        let mut screen = ConsoleScreen::default();
        press(&mut screen, KeyCode::Enter);
        let link = PairingLink {
            origin: "http://127.0.0.1:1234".into(),
            code: "a".repeat(64),
            expires_at: 300.0,
        };
        screen.receive(Event::Link(link.clone()));
        assert_eq!(
            press(&mut screen, KeyCode::Char('o')),
            vec![Effect::Console(Request::NewLink { open_browser: true })]
        );
        assert!(press(&mut screen, KeyCode::Char('o')).is_empty());
        screen.receive(Event::Link(link.clone()));
        screen.receive(Event::BrowserFailed);
        assert!(screen.error.is_some());
        assert_eq!(screen.link, Some(link));
        assert_eq!(press(&mut screen, KeyCode::Esc), vec![Effect::Pop]);
        assert!(screen.active());
        screen.receive(Event::Stopped(Err("Address unavailable".into())));
        assert_eq!(screen.error.as_deref(), Some("Address unavailable"));
        assert!(screen.link.is_none());
    }
}
