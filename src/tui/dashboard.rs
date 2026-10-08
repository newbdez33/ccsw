//! The dashboard: accounts panel plus the nested menu (research notes
//! `cswap-tui.md` §4).

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::{Line, Span};

use super::app::{Action, Effect};
use super::modals::{AddTokenModal, ConfirmModal, Modal};
use super::snapshot::AccountsSnapshot;
use super::theme::{Palette, ThemeName};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuId {
    Switch,
    Watch,
    Auto,
    AddMenu,
    DisableMenu,
    RemoveMenu,
    ThemeMenu,
    Quit,
    AddNew,
    AddLogin,
    AddToken,
    Remove(u32),
    Disable(u32),
    Theme(ThemeName),
    Back,
    /// A non-selectable provider heading inside an account submenu.
    Header,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub label: String,
    pub id: MenuId,
}

fn entry(label: impl Into<String>, id: MenuId) -> MenuEntry {
    MenuEntry {
        label: label.into(),
        id,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuLevel {
    pub title: String,
    pub entries: Vec<MenuEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardScreen {
    levels: Vec<MenuLevel>,
    cursor: usize,
}

impl Default for DashboardScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn root_menu() -> MenuLevel {
    MenuLevel {
        title: "menu".to_string(),
        entries: vec![
            entry("Switch account…", MenuId::Switch),
            entry("Watch accounts", MenuId::Watch),
            entry("Auto-switch view", MenuId::Auto),
            entry("Add account…", MenuId::AddMenu),
            entry("Disable / enable account…", MenuId::DisableMenu),
            entry("Remove account…", MenuId::RemoveMenu),
            entry("Theme…", MenuId::ThemeMenu),
            entry("Quit", MenuId::Quit),
        ],
    }
}

fn with_back(mut entries: Vec<MenuEntry>) -> Vec<MenuEntry> {
    entries.push(entry("← back", MenuId::Back));
    entries
}

impl DashboardScreen {
    pub fn new() -> Self {
        Self {
            levels: vec![root_menu()],
            cursor: 0,
        }
    }

    /// `menu › add account`.
    pub fn breadcrumb(&self) -> String {
        self.levels
            .iter()
            .map(|l| l.title.as_str())
            .collect::<Vec<_>>()
            .join(" › ")
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn entries(&self) -> &[MenuEntry] {
        &self.levels.last().expect("root menu").entries
    }

    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    fn push(&mut self, level: MenuLevel) {
        self.cursor = level
            .entries
            .iter()
            .position(|e| e.id != MenuId::Header)
            .unwrap_or(0);
        self.levels.push(level);
    }

    /// No-op at the root.
    pub fn pop(&mut self) -> bool {
        if self.levels.len() > 1 {
            self.levels.pop();
            self.cursor = 0;
            true
        } else {
            false
        }
    }

    fn pop_to_root(&mut self) {
        self.levels.truncate(1);
        self.cursor = 0;
    }

    fn move_cursor(&mut self, delta: i64) {
        let entries = self.entries();
        let len = entries.len() as i64;
        if len == 0 {
            return;
        }
        let step = delta.signum();
        let mut next = (self.cursor as i64 + delta).clamp(0, len - 1);
        while entries[next as usize].id == MenuId::Header {
            let candidate = next + step;
            if candidate < 0 || candidate >= len {
                return;
            }
            next = candidate;
        }
        self.cursor = next as usize;
    }

    fn account_rows(
        snapshot: Option<&AccountsSnapshot>,
        make: impl Fn(&super::snapshot::AccountSnapshot) -> MenuEntry,
    ) -> Vec<MenuEntry> {
        let Some(snapshot) = snapshot else {
            return Vec::new();
        };
        let mixed = snapshot.is_mixed();
        let mut rows = Vec::new();
        for (provider, group) in snapshot.grouped() {
            if mixed {
                rows.push(entry(provider.as_str(), MenuId::Header));
            }
            rows.extend(group.into_iter().map(&make));
        }
        rows
    }

    fn activate(&mut self, snapshot: Option<&AccountsSnapshot>, theme: ThemeName) -> Vec<Effect> {
        let Some(selected) = self.entries().get(self.cursor).cloned() else {
            return Vec::new();
        };
        match selected.id {
            MenuId::Switch => vec![Effect::OpenSwitch],
            MenuId::Watch => vec![Effect::OpenWatch],
            MenuId::Auto => vec![Effect::OpenAuto],
            MenuId::Quit => vec![Effect::Quit],
            MenuId::Header => Vec::new(),
            MenuId::Back => {
                self.pop();
                Vec::new()
            }
            MenuId::AddMenu => {
                self.push(MenuLevel {
                    title: "add account".to_string(),
                    entries: with_back(vec![
                        entry("Add new account", MenuId::AddNew),
                        entry("From current logins", MenuId::AddLogin),
                        entry("From a token…", MenuId::AddToken),
                    ]),
                });
                Vec::new()
            }
            MenuId::DisableMenu => {
                let rows = Self::account_rows(snapshot, |acc| {
                    let state = if acc.disabled { "  (disabled)" } else { "" };
                    let verb = if acc.disabled {
                        "→ enable"
                    } else {
                        "→ disable"
                    };
                    entry(
                        format!("{}  {}{state}   {verb}", acc.number, acc.label()),
                        MenuId::Disable(acc.number),
                    )
                });
                self.push(MenuLevel {
                    title: "disable / enable".to_string(),
                    entries: with_back(rows),
                });
                Vec::new()
            }
            MenuId::RemoveMenu => {
                let rows = Self::account_rows(snapshot, |acc| {
                    entry(
                        format!("{}  {}  [{}]", acc.number, acc.label(), acc.tag),
                        MenuId::Remove(acc.number),
                    )
                });
                self.push(MenuLevel {
                    title: "remove account".to_string(),
                    entries: with_back(rows),
                });
                Vec::new()
            }
            MenuId::ThemeMenu => {
                let rows = [ThemeName::Dark, ThemeName::Light, ThemeName::Auto]
                    .into_iter()
                    .map(|name| {
                        let mark = if name == theme { "●" } else { " " };
                        entry(format!("{mark} {}", name.as_str()), MenuId::Theme(name))
                    })
                    .collect();
                self.push(MenuLevel {
                    title: "theme".to_string(),
                    entries: with_back(rows),
                });
                Vec::new()
            }
            MenuId::AddNew => vec![Effect::Action(Action::AddNew)],
            MenuId::AddLogin => vec![Effect::OpenModal(Modal::Confirm(
                ConfirmModal::add_current(),
            ))],
            MenuId::AddToken => vec![Effect::OpenModal(Modal::AddToken(AddTokenModal::new()))],
            MenuId::Remove(number) => {
                let email = snapshot
                    .and_then(|s| s.account(number))
                    .map(|a| a.email.clone())
                    .unwrap_or_default();
                vec![Effect::OpenModal(Modal::Confirm(ConfirmModal::remove(
                    number, &email,
                )))]
            }
            MenuId::Disable(number) => {
                self.pop_to_root();
                match snapshot.and_then(|s| s.account(number)) {
                    Some(acc) => vec![Effect::Action(Action::SetDisabled {
                        number,
                        disabled: !acc.disabled,
                    })],
                    None => Vec::new(),
                }
            }
            MenuId::Theme(name) => {
                self.pop_to_root();
                vec![Effect::ApplyTheme(name)]
            }
        }
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        snapshot: Option<&AccountsSnapshot>,
        theme: ThemeName,
    ) -> Vec<Effect> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cursor(-1);
                Vec::new()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cursor(1);
                Vec::new()
            }
            KeyCode::Home => {
                self.cursor = self
                    .entries()
                    .iter()
                    .position(|e| e.id != MenuId::Header)
                    .unwrap_or(0);
                Vec::new()
            }
            KeyCode::End => {
                self.cursor = self
                    .entries()
                    .iter()
                    .rposition(|e| e.id != MenuId::Header)
                    .unwrap_or(0);
                Vec::new()
            }
            KeyCode::Enter => self.activate(snapshot, theme),
            KeyCode::Esc | KeyCode::Left => {
                self.pop();
                Vec::new()
            }
            KeyCode::Char('s') => vec![Effect::OpenSwitch],
            KeyCode::Char('w') => vec![Effect::OpenWatch],
            KeyCode::Char('g') => vec![Effect::OpenAuto],
            KeyCode::Char('f') => vec![Effect::RefreshFull],
            KeyCode::Char('q') => vec![Effect::Quit],
            _ => Vec::new(),
        }
    }

    /// The menu block: breadcrumb, blank, one row per entry.
    pub fn menu_lines(&self, p: &Palette) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from(Span::styled(self.breadcrumb(), p.muted_style())),
            Line::default(),
        ];
        for (i, entry) in self.entries().iter().enumerate() {
            if entry.id == MenuId::Header {
                lines.push(Line::from(Span::styled(
                    format!("  {}", entry.label),
                    p.muted_style(),
                )));
                continue;
            }
            let highlighted = i == self.cursor;
            let label_style = if entry.id == MenuId::Back {
                p.muted_style()
            } else {
                p.fg_style()
            };
            let mut line = if highlighted {
                Line::from(vec![
                    Span::styled("▌", p.accent_style()),
                    Span::styled(format!(" {}", entry.label), label_style),
                ])
            } else {
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(format!(" {}", entry.label), label_style),
                ])
            };
            if highlighted {
                line = line.style(ratatui::style::Style::new().bg(p.surface));
            }
            lines.push(line);
        }
        lines
    }

    pub fn footer(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("s", "Switch accounts"),
            ("w", "Watch"),
            ("q", "Quit"),
            ("^t", "Theme"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::{account, claude_account, entry as usage_entry, snapshot};
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample() -> AccountsSnapshot {
        let mut disabled = claude_account(3, "c@x.y", false, usage_entry(None, None));
        disabled.disabled = true;
        snapshot(
            vec![
                account(1, "a@x.y", false, usage_entry(None, None)),
                account(2, "b@x.y", true, usage_entry(None, None)),
                disabled,
            ],
            1000.0,
        )
    }

    #[test]
    fn root_menu_order_and_navigation() {
        let mut dash = DashboardScreen::new();
        let labels: Vec<&str> = dash.entries().iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Switch account…",
                "Watch accounts",
                "Auto-switch view",
                "Add account…",
                "Disable / enable account…",
                "Remove account…",
                "Theme…",
                "Quit"
            ]
        );
        dash.handle_key(key(KeyCode::Char('k')), None, ThemeName::Dark);
        assert_eq!(dash.cursor(), 0, "clamped at the top");
        dash.handle_key(key(KeyCode::Char('j')), None, ThemeName::Dark);
        dash.handle_key(key(KeyCode::Down), None, ThemeName::Dark);
        assert_eq!(dash.cursor(), 2);
        assert_eq!(
            dash.handle_key(key(KeyCode::Enter), None, ThemeName::Dark),
            vec![Effect::OpenAuto]
        );
        dash.handle_key(key(KeyCode::End), None, ThemeName::Dark);
        assert_eq!(
            dash.handle_key(key(KeyCode::Enter), None, ThemeName::Dark),
            vec![Effect::Quit]
        );
        assert!(!dash.pop(), "Esc at the root is a no-op");
        assert_eq!(dash.breadcrumb(), "menu");
    }

    #[test]
    fn submenus_breadcrumb_and_back() {
        let mut dash = DashboardScreen::new();
        let snap = sample();
        dash.cursor = 3;
        assert!(
            dash.handle_key(key(KeyCode::Enter), Some(&snap), ThemeName::Dark)
                .is_empty()
        );
        assert_eq!(dash.breadcrumb(), "menu › add account");
        assert_eq!(dash.cursor(), 0);
        let labels: Vec<&str> = dash.entries().iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Add new account",
                "From current logins",
                "From a token…",
                "← back"
            ]
        );
        let effects = dash.handle_key(key(KeyCode::Enter), Some(&snap), ThemeName::Dark);
        assert_eq!(effects, vec![Effect::Action(Action::AddNew)]);
        dash.cursor = 1;
        let effects = dash.handle_key(key(KeyCode::Enter), Some(&snap), ThemeName::Dark);
        assert!(matches!(effects[0], Effect::OpenModal(Modal::Confirm(_))));
        dash.cursor = 2;
        let effects = dash.handle_key(key(KeyCode::Enter), Some(&snap), ThemeName::Dark);
        assert!(matches!(effects[0], Effect::OpenModal(Modal::AddToken(_))));
        dash.cursor = 3;
        dash.handle_key(key(KeyCode::Enter), Some(&snap), ThemeName::Dark);
        assert_eq!(dash.breadcrumb(), "menu", "← back pops");
        dash.cursor = 5;
        dash.handle_key(key(KeyCode::Enter), Some(&snap), ThemeName::Dark);
        assert_eq!(dash.breadcrumb(), "menu › remove account");
        assert_eq!(dash.entries()[0].label, "codex");
        assert_eq!(dash.entries()[1].label, "1  a@x.y  [personal]");
        assert_eq!(dash.cursor(), 1);
        let effects = dash.handle_key(key(KeyCode::Enter), Some(&snap), ThemeName::Dark);
        match &effects[0] {
            Effect::OpenModal(Modal::Confirm(c)) => {
                assert_eq!(c.title, "Remove account");
                assert!(c.message.starts_with("Remove account 1 (a@x.y)?"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(dash.depth(), 2, "remove keeps the submenu open");
        dash.handle_key(key(KeyCode::Esc), Some(&snap), ThemeName::Dark);
        assert_eq!(dash.depth(), 1);
    }

    #[test]
    fn disable_toggles_without_confirm_and_returns_to_root() {
        let mut dash = DashboardScreen::new();
        let snap = sample();
        dash.cursor = 4;
        dash.handle_key(key(KeyCode::Enter), Some(&snap), ThemeName::Dark);
        assert_eq!(dash.breadcrumb(), "menu › disable / enable");
        let labels: Vec<&str> = dash.entries().iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "codex",
                "1  a@x.y   → disable",
                "2  b@x.y   → disable",
                "claude",
                "3  c@x.y  (disabled)   → enable",
                "← back"
            ]
        );
        assert_eq!(dash.cursor(), 1);
        dash.handle_key(key(KeyCode::Char('j')), Some(&snap), ThemeName::Dark);
        dash.handle_key(key(KeyCode::Char('j')), Some(&snap), ThemeName::Dark);
        assert_eq!(dash.cursor(), 4, "the claude header is skipped");
        dash.cursor = 4;
        let effects = dash.handle_key(key(KeyCode::Enter), Some(&snap), ThemeName::Dark);
        assert_eq!(
            effects,
            vec![Effect::Action(Action::SetDisabled {
                number: 3,
                disabled: false
            })]
        );
        assert_eq!(dash.breadcrumb(), "menu");
    }

    #[test]
    fn theme_menu_marks_current_and_applies() {
        let mut dash = DashboardScreen::new();
        dash.cursor = 6;
        dash.handle_key(key(KeyCode::Enter), None, ThemeName::Light);
        let labels: Vec<&str> = dash.entries().iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["  dark", "● light", "  auto", "← back"]);
        dash.cursor = 2;
        assert_eq!(
            dash.handle_key(key(KeyCode::Enter), None, ThemeName::Light),
            vec![Effect::ApplyTheme(ThemeName::Auto)]
        );
        assert_eq!(dash.depth(), 1);
    }

    #[test]
    fn hotkeys() {
        let mut dash = DashboardScreen::new();
        assert_eq!(
            dash.handle_key(key(KeyCode::Char('s')), None, ThemeName::Dark),
            vec![Effect::OpenSwitch]
        );
        assert_eq!(
            dash.handle_key(key(KeyCode::Char('w')), None, ThemeName::Dark),
            vec![Effect::OpenWatch]
        );
        assert_eq!(
            dash.handle_key(key(KeyCode::Char('g')), None, ThemeName::Dark),
            vec![Effect::OpenAuto]
        );
        assert_eq!(
            dash.handle_key(key(KeyCode::Char('f')), None, ThemeName::Dark),
            vec![Effect::RefreshFull]
        );
        assert_eq!(
            dash.handle_key(key(KeyCode::Char('q')), None, ThemeName::Dark),
            vec![Effect::Quit]
        );
        let lines = dash.menu_lines(&crate::tui::theme::DARK);
        let text =
            |i: usize| -> String { lines[i].spans.iter().map(|s| s.content.as_ref()).collect() };
        assert_eq!(text(0), "menu");
        assert_eq!(text(2), "▌ Switch account…");
        assert_eq!(text(3), "  Watch accounts");
    }
}
