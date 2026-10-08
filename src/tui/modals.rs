//! Confirmation, browser login, add-token and output modals.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Widget, Wrap};

use crate::switcher::{Line as UiLine, Style as UiStyle};

use super::theme::Palette;
use super::widgets::wrap_text;

/// What a confirmed modal goes on to do.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingAction {
    AddCurrent,
    Remove(u32),
    AddToken(TokenForm),
    GoLive,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmModal {
    pub title: String,
    pub message: String,
    pub yes_label: String,
    pub focus_yes: bool,
    pub action: PendingAction,
}

impl ConfirmModal {
    pub fn new(
        title: impl Into<String>,
        message: impl Into<String>,
        yes_label: impl Into<String>,
        action: PendingAction,
    ) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            yes_label: yes_label.into(),
            focus_yes: true,
            action,
        }
    }

    pub fn add_current() -> Self {
        Self::new(
            "Add account",
            "Back up the current Codex and Claude Code logins as managed accounts?\n\nA login that is already managed has its stored credentials refreshed in place.",
            "Add",
            PendingAction::AddCurrent,
        )
    }

    pub fn remove(number: u32, email: &str) -> Self {
        Self::new(
            "Remove account",
            format!("Remove account {number} ({email})?\n\nIts stored credentials are deleted."),
            "Remove",
            PendingAction::Remove(number),
        )
    }

    pub fn overwrite_slot(slot: u32, email: &str, form: TokenForm) -> Self {
        Self::new(
            "Overwrite slot",
            format!("Slot {slot} is occupied by {email}. Overwrite?"),
            "Overwrite",
            PendingAction::AddToken(form),
        )
    }

    pub fn go_live() -> Self {
        Self::new(
            "Go live",
            "Go live? cswitch will switch your active account automatically when the threshold is reached.\n\n(Same behavior as running `cswitch auto` in a terminal.)",
            "Go live",
            PendingAction::GoLive,
        )
    }

    fn hint(&self) -> String {
        format!(
            "← → · enter  ·  y {}  ·  n / esc cancel",
            self.yes_label.to_lowercase()
        )
    }
}

/// The validated form: `email` and `slot` are `None` when left blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenForm {
    pub token: String,
    pub email: Option<String>,
    pub slot: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Token,
    Email,
    Slot,
    Add,
    Cancel,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Self::Token => Self::Email,
            Self::Email => Self::Slot,
            Self::Slot => Self::Add,
            Self::Add => Self::Cancel,
            Self::Cancel => Self::Token,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Token => Self::Cancel,
            Self::Email => Self::Token,
            Self::Slot => Self::Email,
            Self::Add => Self::Slot,
            Self::Cancel => Self::Add,
        }
    }

    fn is_button(self) -> bool {
        matches!(self, Self::Add | Self::Cancel)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddTokenModal {
    token: String,
    email: String,
    slot: String,
    focus: Focus,
    error: Option<String>,
}

impl Default for AddTokenModal {
    fn default() -> Self {
        Self::new()
    }
}

impl AddTokenModal {
    pub fn new() -> Self {
        Self {
            token: String::new(),
            email: String::new(),
            slot: String::new(),
            focus: Focus::Token,
            error: None,
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Empty token → `Token is required.`; slot must be a whole number ≥ 1.
    pub fn validate(&self) -> Result<TokenForm, String> {
        let token = self.token.trim();
        if token.is_empty() {
            return Err("Token is required.".to_string());
        }
        let slot = self.slot.trim();
        let slot = if slot.is_empty() {
            None
        } else {
            match slot.parse::<i64>() {
                Ok(n) if n >= 1 => Some(n as u32),
                Ok(_) => return Err("Slot must be >= 1.".to_string()),
                Err(_) => return Err("Slot must be a number.".to_string()),
            }
        };
        let email = self.email.trim();
        Ok(TokenForm {
            token: token.to_string(),
            email: (!email.is_empty()).then(|| email.to_string()),
            slot,
        })
    }

    fn field_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            Focus::Token => Some(&mut self.token),
            Focus::Email => Some(&mut self.email),
            Focus::Slot => Some(&mut self.slot),
            _ => None,
        }
    }

    fn submit(&mut self) -> ModalOutcome {
        match self.validate() {
            Ok(form) => ModalOutcome::Submitted(form),
            Err(error) => {
                self.error = Some(error);
                ModalOutcome::Open
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => ModalOutcome::Closed,
            KeyCode::Tab | KeyCode::Down => {
                self.focus = self.focus.next();
                ModalOutcome::Open
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = self.focus.prev();
                ModalOutcome::Open
            }
            KeyCode::Left | KeyCode::Right if self.focus.is_button() => {
                self.focus = if self.focus == Focus::Add {
                    Focus::Cancel
                } else {
                    Focus::Add
                };
                ModalOutcome::Open
            }
            KeyCode::Enter => {
                if self.focus == Focus::Cancel {
                    ModalOutcome::Closed
                } else {
                    self.submit()
                }
            }
            KeyCode::Backspace => {
                if let Some(field) = self.field_mut() {
                    field.pop();
                }
                ModalOutcome::Open
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                if let Some(field) = self.field_mut() {
                    field.push(c);
                }
                ModalOutcome::Open
            }
            _ => ModalOutcome::Open,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutputModal {
    pub title: String,
    pub lines: Vec<UiLine>,
    pub scroll: usize,
}

impl OutputModal {
    pub fn new(title: impl Into<String>, lines: Vec<UiLine>) -> Self {
        Self {
            title: title.into(),
            lines,
            scroll: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoginModal {
    pub url: Option<String>,
    pub cancelling: bool,
    pub scroll: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    Confirm(ConfirmModal),
    AddToken(AddTokenModal),
    Output(OutputModal),
    Login(LoginModal),
}

/// What a key did to the modal.
#[derive(Debug, Clone, PartialEq)]
pub enum ModalOutcome {
    Open,
    Closed,
    Confirmed(PendingAction),
    Submitted(TokenForm),
    CancelLogin,
}

impl Modal {
    pub fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match self {
            Modal::Login(login) => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    login.scroll = login.scroll.saturating_add(1);
                    ModalOutcome::Open
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    login.scroll = login.scroll.saturating_sub(1);
                    ModalOutcome::Open
                }
                KeyCode::Esc | KeyCode::Char('q') if !login.cancelling => ModalOutcome::CancelLogin,
                _ => ModalOutcome::Open,
            },
            Modal::Confirm(confirm) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    ModalOutcome::Confirmed(confirm.action.clone())
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => ModalOutcome::Closed,
                KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                    confirm.focus_yes = !confirm.focus_yes;
                    ModalOutcome::Open
                }
                KeyCode::Enter => {
                    if confirm.focus_yes {
                        ModalOutcome::Confirmed(confirm.action.clone())
                    } else {
                        ModalOutcome::Closed
                    }
                }
                _ => ModalOutcome::Open,
            },
            Modal::AddToken(form) => form.handle_key(key),
            Modal::Output(output) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => ModalOutcome::Closed,
                KeyCode::Down | KeyCode::Char('j') => {
                    output.scroll = (output.scroll + 1).min(output.lines.len().saturating_sub(1));
                    ModalOutcome::Open
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    output.scroll = output.scroll.saturating_sub(1);
                    ModalOutcome::Open
                }
                _ => ModalOutcome::Open,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn button(label: &str, focused: bool, p: &Palette) -> Span<'static> {
    let text = format!("{:^12}", format!(" {label} "));
    if focused {
        Span::styled(
            text,
            Style::new()
                .fg(p.bg)
                .bg(p.accent)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(text, Style::new().fg(p.fg).bg(p.panel))
    }
}

fn buttons_line(buttons: &[(&str, bool)], width: usize, p: &Palette) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (i, (label, focused)) in buttons.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
            used += 2;
        }
        let span = button(label, *focused, p);
        used += span.width();
        spans.push(span);
    }
    let pad = width.saturating_sub(used);
    spans.insert(0, Span::raw(" ".repeat(pad)));
    Line::from(spans)
}

fn ui_style(style: UiStyle, p: &Palette) -> Style {
    match style {
        UiStyle::Plain => p.fg_style(),
        UiStyle::Accent => p.accent_style(),
        UiStyle::Muted => p.muted_style(),
        UiStyle::Dimmed => p.muted_style().add_modifier(Modifier::DIM),
        UiStyle::Bold => p.bold_fg(),
        UiStyle::BoldAccent => p.bold_accent(),
        UiStyle::Warning => p.warn_style(),
    }
}

/// A switcher line with the palette applied per span.
pub fn ui_line(line: &UiLine, p: &Palette) -> Line<'static> {
    Line::from(
        line.spans
            .iter()
            .map(|span| Span::styled(span.text.clone(), ui_style(span.style, p)))
            .collect::<Vec<_>>(),
    )
}

fn wrapped(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap_text(text, width)
        .into_iter()
        .map(|row| Line::from(Span::styled(row, style)))
        .collect()
}

/// The body of a modal as lines at `width`; shared with the render tests.
pub fn modal_lines(modal: &Modal, width: usize, p: &Palette) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match modal {
        Modal::Login(login) => {
            lines.push(Line::from(Span::styled("Add new account", p.bold_accent())));
            lines.push(Line::default());
            if login.cancelling {
                lines.push(Line::from("Cancelling login…"));
            } else {
                lines.extend(wrapped(
                    if login.url.is_some() {
                        "Complete sign-in in your browser. The account will be saved and activated."
                    } else {
                        "Opening your browser…"
                    },
                    width,
                    p.fg_style(),
                ));
                lines.push(Line::from(Span::styled(
                    "↑ ↓ scroll  ·  esc cancel",
                    p.muted_style(),
                )));
                if let Some(url) = &login.url {
                    lines.push(Line::default());
                    lines.extend(wrapped(
                        "If the browser did not open, use this URL:",
                        width,
                        p.muted_style(),
                    ));
                    lines.extend(wrapped(url, width, p.fg_style()));
                }
            }
        }
        Modal::Confirm(confirm) => {
            lines.push(Line::from(Span::styled(
                confirm.title.clone(),
                p.bold_accent(),
            )));
            lines.push(Line::default());
            lines.extend(wrapped(&confirm.message, width, p.fg_style()));
            lines.push(Line::default());
            lines.push(buttons_line(
                &[
                    (confirm.yes_label.as_str(), confirm.focus_yes),
                    ("Cancel", !confirm.focus_yes),
                ],
                width,
                p,
            ));
            lines.push(Line::from(Span::styled(confirm.hint(), p.muted_style())));
        }
        Modal::AddToken(form) => {
            lines.push(Line::from(Span::styled(
                "Add account from a token",
                p.bold_accent(),
            )));
            lines.extend(wrapped(
                "OpenAI API key (sk-…), or an Anthropic setup-token / API key (sk-ant-…); API-key accounts have no usage quota.",
                width,
                p.muted_style(),
            ));
            let field = |value: &str, masked: bool, focused: bool, label: &str| {
                let shown: String = if masked {
                    "•".repeat(value.chars().count())
                } else {
                    value.to_string()
                };
                let inner = 40usize;
                let visible: String = shown
                    .chars()
                    .rev()
                    .take(inner)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let mut style = Style::new().fg(p.fg).bg(p.panel);
                if focused {
                    style = style.bg(p.surface).add_modifier(Modifier::REVERSED);
                }
                Line::from(vec![
                    Span::styled(format!("[{visible:<inner$}]"), style),
                    Span::styled(format!("  {label}"), p.muted_style()),
                ])
            };
            lines.push(field(
                &form.token,
                true,
                form.focus == Focus::Token,
                "token (required)",
            ));
            lines.push(field(
                &form.email,
                false,
                form.focus == Focus::Email,
                "email label (optional)",
            ));
            lines.push(field(
                &form.slot,
                false,
                form.focus == Focus::Slot,
                "slot number (optional)",
            ));
            lines.push(Line::from(Span::styled(
                form.error.clone().unwrap_or_default(),
                p.crit_style(),
            )));
            lines.push(buttons_line(
                &[
                    ("Add", form.focus == Focus::Add),
                    ("Cancel", form.focus == Focus::Cancel),
                ],
                width,
                p,
            ));
            lines.push(Line::from(Span::styled(
                "enter add  ·  tab next field  ·  esc cancel",
                p.muted_style(),
            )));
        }
        Modal::Output(output) => {
            lines.push(Line::from(Span::styled(
                output.title.clone(),
                p.bold_accent(),
            )));
            lines.push(Line::default());
            if output.lines.iter().all(|l| l.text().trim().is_empty()) {
                lines.push(Line::from(Span::styled("(no output)", p.muted_style())));
            } else {
                let body: Vec<Line<'static>> = output
                    .lines
                    .iter()
                    .skip(output.scroll)
                    .take(20)
                    .map(|l| ui_line(l, p))
                    .collect();
                lines.extend(body);
            }
            lines.push(Line::default());
            lines.push(buttons_line(&[("Close", true)], width, p));
            lines.push(Line::from(Span::styled("esc close", p.muted_style())));
        }
    }
    lines
}

/// Draw the modal centered over `area` after dimming what lies beneath.
pub fn render_modal(buf: &mut Buffer, area: Rect, modal: &mut Modal, p: &Palette) {
    buf.set_style(area, Style::new().add_modifier(Modifier::DIM));
    let box_width = match modal {
        Modal::Output(_) | Modal::Login(_) => 90u16,
        _ => 64u16,
    }
    .min(area.width * 9 / 10)
    .max(20);
    let inner_width = box_width.saturating_sub(6) as usize;
    let lines = modal_lines(modal, inner_width, p);
    let max_height = (area.height * 4 / 5).max(5);
    let box_height = (lines.len() as u16 + 4).min(max_height);
    let scroll = if let Modal::Login(login) = modal {
        login.scroll = login.scroll.min(
            lines
                .len()
                .saturating_sub(box_height.saturating_sub(4) as usize),
        );
        login.scroll as u16
    } else {
        0
    };
    let rect = area.centered(
        Constraint::Length(box_width),
        Constraint::Length(box_height),
    );
    Clear.render(rect, buf);
    Paragraph::new(lines)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false })
        .style(Style::new().fg(p.fg).bg(p.surface))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(p.panel).bg(p.surface))
                .padding(Padding::new(2, 2, 1, 1)),
        )
        .render(rect, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn confirm_keys() {
        let mut modal = Modal::Confirm(ConfirmModal::remove(3, "a@b.c"));
        assert_eq!(
            modal.handle_key(key(KeyCode::Char('x'))),
            ModalOutcome::Open
        );
        assert_eq!(
            modal.handle_key(key(KeyCode::Char('y'))),
            ModalOutcome::Confirmed(PendingAction::Remove(3))
        );
        assert_eq!(
            modal.handle_key(key(KeyCode::Char('n'))),
            ModalOutcome::Closed
        );
        assert_eq!(modal.handle_key(key(KeyCode::Esc)), ModalOutcome::Closed);
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Confirmed(PendingAction::Remove(3)),
            "yes is focused first"
        );
        modal.handle_key(key(KeyCode::Right));
        assert_eq!(modal.handle_key(key(KeyCode::Enter)), ModalOutcome::Closed);
        if let Modal::Confirm(c) = &modal {
            assert_eq!(c.hint(), "← → · enter  ·  y remove  ·  n / esc cancel");
            assert_eq!(
                c.message,
                "Remove account 3 (a@b.c)?\n\nIts stored credentials are deleted."
            );
        }
        let live = ConfirmModal::go_live();
        assert!(live.message.starts_with("Go live? cswitch will switch"));
        assert!(live.message.contains("`cswitch auto`"));
        assert!(
            ConfirmModal::add_current()
                .message
                .starts_with("Back up the current Codex and Claude Code logins")
        );
        assert_eq!(
            ConfirmModal::overwrite_slot(
                2,
                "x@y.z",
                TokenForm {
                    token: "t".into(),
                    email: None,
                    slot: Some(2)
                }
            )
            .message,
            "Slot 2 is occupied by x@y.z. Overwrite?"
        );
    }

    #[test]
    fn token_form_validation_order() {
        let mut form = AddTokenModal::new();
        assert_eq!(form.validate(), Err("Token is required.".to_string()));
        form.token = "sk-test".into();
        form.slot = "abc".into();
        assert_eq!(form.validate(), Err("Slot must be a number.".to_string()));
        form.slot = "0".into();
        assert_eq!(form.validate(), Err("Slot must be >= 1.".to_string()));
        form.slot = " 4 ".into();
        form.email = "  ".into();
        assert_eq!(
            form.validate(),
            Ok(TokenForm {
                token: "sk-test".into(),
                email: None,
                slot: Some(4)
            })
        );
        form.email = "me@x.y".into();
        form.slot = String::new();
        assert_eq!(form.validate().unwrap().email.as_deref(), Some("me@x.y"));
        assert_eq!(form.validate().unwrap().slot, None);
    }

    #[test]
    fn token_form_keys() {
        let mut modal = Modal::AddToken(AddTokenModal::new());
        assert_eq!(modal.handle_key(key(KeyCode::Enter)), ModalOutcome::Open);
        if let Modal::AddToken(f) = &modal {
            assert_eq!(f.error(), Some("Token is required."));
        }
        for c in "sk-1".chars() {
            modal.handle_key(key(KeyCode::Char(c)));
        }
        modal.handle_key(key(KeyCode::Tab));
        for c in "a@b.c".chars() {
            modal.handle_key(key(KeyCode::Char(c)));
        }
        modal.handle_key(key(KeyCode::Tab));
        modal.handle_key(key(KeyCode::Char('7')));
        modal.handle_key(key(KeyCode::Char('x')));
        modal.handle_key(key(KeyCode::Backspace));
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Submitted(TokenForm {
                token: "sk-1".into(),
                email: Some("a@b.c".into()),
                slot: Some(7)
            })
        );
        // Tab to the buttons: ←/→ move between them, Enter on Cancel closes.
        modal.handle_key(key(KeyCode::Tab));
        modal.handle_key(key(KeyCode::Right));
        assert_eq!(modal.handle_key(key(KeyCode::Enter)), ModalOutcome::Closed);
        assert_eq!(modal.handle_key(key(KeyCode::Esc)), ModalOutcome::Closed);
    }

    #[test]
    fn output_modal_keys_and_lines() {
        let mut modal = Modal::Output(OutputModal::new(
            "Add current login",
            vec![UiLine::plain("one"), UiLine::plain("two")],
        ));
        modal.handle_key(key(KeyCode::Char('j')));
        modal.handle_key(key(KeyCode::Char('j')));
        if let Modal::Output(o) = &modal {
            assert_eq!(o.scroll, 1);
        }
        assert_eq!(
            modal.handle_key(key(KeyCode::Char('q'))),
            ModalOutcome::Closed
        );
        let lines = modal_lines(&modal, 60, &crate::tui::theme::DARK);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(texts[0], "Add current login");
        assert_eq!(texts[2], "two");
        assert_eq!(texts.last().unwrap(), "esc close");
        let empty = Modal::Output(OutputModal::new("x", Vec::new()));
        let lines = modal_lines(&empty, 60, &crate::tui::theme::DARK);
        assert_eq!(lines[2].spans[0].content, "(no output)");
    }
}
