//! The Switch screen and the card list it shares with Watch (research notes
//! `cswap-tui.md` §5): full cards, a cursor that survives unchanged snapshots,
//! and a 1.5 s flash when a card's measurement advances.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::app::{Action, Effect};
use super::snapshot::AccountsSnapshot;
use super::theme::Palette;
use super::widgets::account_card;

pub const FLASH_S: f64 = 1.5;

#[derive(Debug, Clone, PartialEq)]
pub struct CardList {
    numbers: Vec<u32>,
    cursor: Option<usize>,
    scroll: usize,
    flash_until: HashMap<u32, f64>,
    fetched: HashMap<u32, Option<f64>>,
    built: bool,
    viewport: usize,
    total_lines: usize,
}

impl Default for CardList {
    fn default() -> Self {
        Self::new()
    }
}

impl CardList {
    pub fn new() -> Self {
        Self {
            numbers: Vec::new(),
            cursor: None,
            scroll: 0,
            flash_until: HashMap::new(),
            fetched: HashMap::new(),
            built: false,
            viewport: 0,
            total_lines: 0,
        }
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn selected(&self) -> Option<u32> {
        self.cursor.and_then(|i| self.numbers.get(i).copied())
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn is_flashing(&self, number: u32, now: f64) -> bool {
        self.flash_until
            .get(&number)
            .is_some_and(|until| *until > now)
    }

    /// Unchanged account set → cursor untouched; changed → cursor clamped;
    /// first build → cursor on the active account when `with_cursor`.
    pub fn sync(&mut self, snapshot: &AccountsSnapshot, now: f64, with_cursor: bool) {
        let numbers = snapshot.numbers();
        for acc in &snapshot.accounts {
            let previous = self.fetched.insert(acc.number, acc.usage.fetched_at);
            if let Some(Some(before)) = previous
                && acc.usage.fetched_at.is_some_and(|after| after > before)
            {
                self.flash_until.insert(acc.number, now + FLASH_S);
            }
        }
        if !self.built {
            self.built = true;
            self.numbers = numbers;
            if with_cursor {
                self.cursor_to_active(snapshot);
            }
            return;
        }
        if numbers == self.numbers {
            return;
        }
        self.numbers = numbers;
        if let Some(cursor) = self.cursor {
            self.cursor = if self.numbers.is_empty() {
                None
            } else {
                Some(cursor.min(self.numbers.len() - 1))
            };
        }
    }

    pub fn cursor_to_active(&mut self, snapshot: &AccountsSnapshot) {
        if self.numbers.is_empty() {
            self.cursor = None;
            return;
        }
        let active = snapshot
            .active_number
            .and_then(|n| self.numbers.iter().position(|x| *x == n))
            .unwrap_or(0);
        self.cursor = Some(active);
    }

    pub fn clear_cursor(&mut self) {
        self.cursor = None;
    }

    pub fn move_cursor(&mut self, delta: i64) {
        let Some(cursor) = self.cursor else {
            return;
        };
        if self.numbers.is_empty() {
            return;
        }
        self.cursor =
            Some((cursor as i64 + delta).clamp(0, self.numbers.len() as i64 - 1) as usize);
    }

    /// Monitor-mode scrolling, clamped to what the last render measured.
    pub fn scroll_by(&mut self, delta: i64) {
        let max = self.total_lines.saturating_sub(self.viewport) as i64;
        self.scroll = (self.scroll as i64 + delta).clamp(0, max.max(0)) as usize;
    }

    /// The visible lines for a viewport of `height` rows and `width` columns.
    pub fn render_lines(
        &mut self,
        snapshot: Option<&AccountsSnapshot>,
        width: usize,
        height: usize,
        now: f64,
        p: &Palette,
    ) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut cursor_range = None;
        if let Some(snapshot) = snapshot {
            for (i, acc) in snapshot.accounts.iter().enumerate() {
                let highlighted = self.cursor == Some(i);
                let flashing = self.is_flashing(acc.number, now);
                let start = lines.len();
                let bg = if highlighted {
                    Some(p.surface)
                } else if flashing {
                    Some(p.panel)
                } else {
                    None
                };
                for card_line in account_card(acc, width.saturating_sub(3), None, now, p) {
                    let mut spans = vec![if highlighted {
                        Span::styled("▌", p.accent_style())
                    } else {
                        Span::raw(" ")
                    }];
                    spans.push(Span::raw(" "));
                    spans.extend(card_line.spans);
                    let mut line = Line::from(spans);
                    if let Some(bg) = bg {
                        line = line.style(Style::new().bg(bg));
                    }
                    lines.push(line);
                }
                lines.push(Line::default());
                if highlighted {
                    cursor_range = Some((start, lines.len() - 1));
                }
            }
        }
        self.total_lines = lines.len();
        self.viewport = height;
        if let Some((start, end)) = cursor_range {
            if start < self.scroll {
                self.scroll = start;
            } else if end > self.scroll + height {
                self.scroll = end.saturating_sub(height);
            }
        }
        let max = self.total_lines.saturating_sub(height);
        self.scroll = self.scroll.min(max);
        lines.into_iter().skip(self.scroll).take(height).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwitchScreen {
    pub list: CardList,
}

impl Default for SwitchScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl SwitchScreen {
    pub fn new() -> Self {
        Self {
            list: CardList::new(),
        }
    }

    pub fn sync(&mut self, snapshot: &AccountsSnapshot, now: f64) {
        self.list.sync(snapshot, now, true);
    }

    pub fn title(&self) -> String {
        "switch to which account?".to_string()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.list.move_cursor(-1);
                Vec::new()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.list.move_cursor(1);
                Vec::new()
            }
            KeyCode::Enter => match self.list.selected() {
                Some(number) => vec![Effect::Action(Action::SwitchTo(number)), Effect::Pop],
                None => Vec::new(),
            },
            KeyCode::Char('b') => vec![Effect::Action(Action::SwitchBest)],
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('s') => vec![Effect::Pop],
            _ => Vec::new(),
        }
    }

    pub fn footer(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("enter", "Switch"),
            ("b", "Best pick"),
            ("esc", "Back"),
            ("^t", "Theme"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::{account, entry, snapshot};
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn three() -> AccountsSnapshot {
        snapshot(
            vec![
                account(1, "a@x.y", false, entry(Some(900.0), Some(10.0))),
                account(2, "b@x.y", true, entry(Some(900.0), Some(20.0))),
                account(3, "c@x.y", false, entry(Some(900.0), Some(30.0))),
            ],
            1000.0,
        )
    }

    #[test]
    fn cursor_starts_on_active_and_survives_unchanged_sets() {
        let mut screen = SwitchScreen::new();
        let snap = three();
        screen.sync(&snap, 1000.0);
        assert_eq!(screen.list.cursor(), Some(1));
        screen.handle_key(key(KeyCode::Char('j')));
        assert_eq!(screen.list.selected(), Some(3));
        let mut same = snap.clone();
        same.active_number = Some(1);
        same.accounts[0].is_active = true;
        same.accounts[1].is_active = false;
        screen.sync(&same, 1003.0);
        assert_eq!(
            screen.list.cursor(),
            Some(2),
            "unchanged set keeps the cursor"
        );
        let mut fewer = snap.clone();
        fewer.accounts.pop();
        screen.sync(&fewer, 1006.0);
        assert_eq!(screen.list.cursor(), Some(1), "clamped after a rebuild");
        screen.handle_key(key(KeyCode::Char('k')));
        screen.handle_key(key(KeyCode::Char('k')));
        assert_eq!(screen.list.cursor(), Some(0));
        let empty = snapshot(Vec::new(), 1010.0);
        screen.sync(&empty, 1010.0);
        assert_eq!(screen.list.cursor(), None);
        assert!(screen.handle_key(key(KeyCode::Enter)).is_empty());
    }

    #[test]
    fn enter_switches_and_pops_best_and_back() {
        let mut screen = SwitchScreen::new();
        screen.sync(&three(), 1000.0);
        assert_eq!(
            screen.handle_key(key(KeyCode::Enter)),
            vec![Effect::Action(Action::SwitchTo(2)), Effect::Pop]
        );
        assert_eq!(
            screen.handle_key(key(KeyCode::Char('b'))),
            vec![Effect::Action(Action::SwitchBest)]
        );
        for code in [KeyCode::Esc, KeyCode::Char('q'), KeyCode::Char('s')] {
            assert_eq!(screen.handle_key(key(code)), vec![Effect::Pop]);
        }
    }

    #[test]
    fn flash_follows_fetched_at_advances() {
        let mut list = CardList::new();
        let snap = three();
        list.sync(&snap, 1000.0, true);
        assert!(!list.is_flashing(1, 1000.0));
        let mut newer = snap.clone();
        newer.accounts[0].usage.fetched_at = Some(950.0);
        list.sync(&newer, 1001.0, true);
        assert!(list.is_flashing(1, 1002.0));
        assert!(!list.is_flashing(1, 1002.6));
        assert!(!list.is_flashing(2, 1002.0));
    }

    #[test]
    fn rendering_keeps_the_cursor_visible_and_omits_ticks() {
        let mut list = CardList::new();
        let snap = three();
        list.sync(&snap, 1000.0, true);
        list.cursor = Some(2);
        let lines = list.render_lines(Some(&snap), 80, 4, 1000.0, &crate::tui::theme::DARK);
        assert_eq!(lines.len(), 4);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            text.iter().any(|t| t.starts_with("▌  3  c@x.y")),
            "{text:?}"
        );
        assert!(
            text.iter().all(|t| !t.contains('┃')),
            "list cards pass no threshold"
        );
        assert_eq!(list.scroll(), 4, "scrolled just enough to show the cursor");
        list.scroll_by(-10);
        assert_eq!(list.scroll(), 0);
        list.scroll_by(100);
        assert_eq!(list.scroll(), 5, "clamped to total - viewport");
    }
}
