//! The Watch screen (research notes `cswap-tui.md` §6): a read-only monitor
//! until `s` arms selection; a switch stays on the screen.

use crossterm::event::{KeyCode, KeyEvent};

use super::app::{Action, Effect};
use super::snapshot::AccountsSnapshot;
use super::switch::CardList;

#[derive(Debug, Clone, PartialEq)]
pub struct WatchScreen {
    pub list: CardList,
    armed: bool,
}

impl Default for WatchScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchScreen {
    pub fn new() -> Self {
        Self {
            list: CardList::new(),
            armed: false,
        }
    }

    pub fn armed(&self) -> bool {
        self.armed
    }

    pub fn sync(&mut self, snapshot: &AccountsSnapshot, now: f64) {
        self.list.sync(snapshot, now, false);
    }

    /// `watching all accounts · snapshot 1m ago · refreshing 5s` or the
    /// selection prompt while armed.
    pub fn title(&self, refresh_status: &str) -> String {
        if self.armed {
            return "switch to which account? · enter confirm · esc cancel".to_string();
        }
        if refresh_status.is_empty() {
            "watching all accounts".to_string()
        } else {
            format!("watching all accounts · {refresh_status}")
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
        self.list.clear_cursor();
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        snapshot: Option<&AccountsSnapshot>,
    ) -> Vec<Effect> {
        match key.code {
            KeyCode::Char('s') => {
                if self.armed {
                    self.disarm();
                } else if let Some(snapshot) = snapshot {
                    self.armed = true;
                    self.list.cursor_to_active(snapshot);
                }
                Vec::new()
            }
            KeyCode::Enter => {
                if !self.armed {
                    return Vec::new();
                }
                let selected = self.list.selected();
                self.disarm();
                match selected {
                    Some(number) => vec![Effect::Action(Action::SwitchTo(number))],
                    None => Vec::new(),
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.armed {
                    self.list.move_cursor(1);
                } else {
                    self.list.scroll_by(1);
                }
                Vec::new()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.armed {
                    self.list.move_cursor(-1);
                } else {
                    self.list.scroll_by(-1);
                }
                Vec::new()
            }
            KeyCode::Char('f') => vec![Effect::RefreshFull],
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.armed {
                    self.disarm();
                    Vec::new()
                } else {
                    vec![Effect::Pop]
                }
            }
            _ => Vec::new(),
        }
    }

    pub fn footer(&self) -> Vec<(&'static str, &'static str)> {
        let mut chips = vec![("s", "Switch")];
        if self.armed {
            chips.push(("enter", "Confirm"));
        }
        chips.push(("esc", "Back"));
        chips.push(("^t", "Theme"));
        chips
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

    fn two() -> AccountsSnapshot {
        snapshot(
            vec![
                account(1, "a@x.y", false, entry(None, None)),
                account(2, "b@x.y", true, entry(None, None)),
            ],
            1000.0,
        )
    }

    #[test]
    fn monitor_mode_is_read_only_until_armed() {
        let mut watch = WatchScreen::new();
        let snap = two();
        watch.sync(&snap, 1000.0);
        assert_eq!(watch.list.cursor(), None);
        assert!(
            watch
                .handle_key(key(KeyCode::Enter), Some(&snap))
                .is_empty()
        );
        assert_eq!(watch.title(""), "watching all accounts");
        assert_eq!(
            watch.title("snapshot 1m ago · refreshing 5s"),
            "watching all accounts · snapshot 1m ago · refreshing 5s"
        );
        watch.handle_key(key(KeyCode::Char('s')), Some(&snap));
        assert!(watch.armed());
        assert_eq!(
            watch.list.cursor(),
            Some(1),
            "cursor jumps to the active account"
        );
        assert_eq!(
            watch.title("snapshot 1m ago"),
            "switch to which account? · enter confirm · esc cancel"
        );
        assert_eq!(watch.footer()[1], ("enter", "Confirm"));
        watch.handle_key(key(KeyCode::Char('k')), Some(&snap));
        assert_eq!(
            watch.handle_key(key(KeyCode::Enter), Some(&snap)),
            vec![Effect::Action(Action::SwitchTo(1))]
        );
        assert!(!watch.armed(), "a switch disarms but stays");
        assert_eq!(watch.list.cursor(), None);
    }

    #[test]
    fn esc_disarms_then_leaves() {
        let mut watch = WatchScreen::new();
        let snap = two();
        watch.sync(&snap, 1000.0);
        watch.handle_key(key(KeyCode::Char('s')), Some(&snap));
        assert!(watch.handle_key(key(KeyCode::Esc), Some(&snap)).is_empty());
        assert!(!watch.armed());
        assert_eq!(
            watch.handle_key(key(KeyCode::Esc), Some(&snap)),
            vec![Effect::Pop]
        );
        assert_eq!(
            watch.handle_key(key(KeyCode::Char('q')), Some(&snap)),
            vec![Effect::Pop]
        );
        assert_eq!(
            watch.handle_key(key(KeyCode::Char('f')), Some(&snap)),
            vec![Effect::RefreshFull]
        );
        watch.handle_key(key(KeyCode::Char('s')), Some(&snap));
        watch.handle_key(key(KeyCode::Char('s')), Some(&snap));
        assert!(!watch.armed(), "s toggles");
    }
}
