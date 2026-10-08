//! The auto-switch view (research notes `cswap-tui.md` §7): hosts the engine
//! in dry-run first, ranks the next best candidate, logs events, and lets the
//! session threshold be adjusted without ever writing settings.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::autoswitch::{Event, pct_label};
use crate::store::AutoSwitchSettings;
use crate::usage_math::binding_pct;

use super::app::Effect;
use super::data::sentinel_label;
use super::modals::{ConfirmModal, Modal};
use super::snapshot::AccountsSnapshot;
use super::theme::Palette;
use super::widgets::wrap_text;

pub const THRESHOLD_MIN: f64 = 50.0;
pub const THRESHOLD_MAX: f64 = 99.9;
const LOG_CAP: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Switch,
    Warn,
    Crit,
    Muted,
    Plain,
}

impl LogKind {
    pub fn of_event(event: &Event) -> Self {
        match event.kind() {
            "switch" => Self::Switch,
            "error" | "account-quarantined" => Self::Warn,
            "all-exhausted" => Self::Crit,
            "poll" | "no-switch" | "sleep" | "account-unquarantined" => Self::Muted,
            _ => Self::Plain,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub stamp: String,
    pub kind: LogKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoScreen {
    settings: AutoSwitchSettings,
    configured_threshold: f64,
    dry_run: bool,
    adjusting: bool,
    adjust_start: f64,
    log: Vec<LogLine>,
    engine_available: bool,
}

impl AutoScreen {
    /// Always opens in dry-run; the caller starts the engine with `settings`.
    pub fn new(settings: AutoSwitchSettings, stamp: &str) -> Self {
        let mut screen = Self {
            configured_threshold: settings.threshold,
            settings,
            dry_run: true,
            adjusting: false,
            adjust_start: 0.0,
            log: Vec::new(),
            engine_available: true,
        };
        screen.push_system("— engine started: DRY-RUN (watching only) —", stamp);
        screen
    }

    /// The view for a roster without a Claude Code account: a notice, no engine.
    pub fn without_claude(settings: AutoSwitchSettings, stamp: &str) -> Self {
        let mut screen = Self {
            configured_threshold: settings.threshold,
            settings,
            dry_run: true,
            adjusting: false,
            adjust_start: 0.0,
            log: Vec::new(),
            engine_available: false,
        };
        screen.push_system(
            "— no Claude Code account: auto-switch covers Claude Code only (ccsw add claude) —",
            stamp,
        );
        screen
    }

    pub fn engine_available(&self) -> bool {
        self.engine_available
    }

    pub fn settings(&self) -> &AutoSwitchSettings {
        &self.settings
    }

    pub fn threshold(&self) -> f64 {
        self.settings.threshold
    }

    pub fn configured_threshold(&self) -> f64 {
        self.configured_threshold
    }

    pub fn dry_run(&self) -> bool {
        self.dry_run
    }

    pub fn adjusting(&self) -> bool {
        self.adjusting
    }

    pub fn log(&self) -> &[LogLine] {
        &self.log
    }

    fn push_log(&mut self, line: LogLine) {
        self.log.push(line);
        if self.log.len() > LOG_CAP {
            self.log.drain(..self.log.len() - LOG_CAP);
        }
    }

    fn push_system(&mut self, text: &str, stamp: &str) {
        self.push_log(LogLine {
            stamp: stamp.to_string(),
            kind: LogKind::Muted,
            text: text.to_string(),
        });
    }

    pub fn on_event(&mut self, event: &Event, stamp: &str) {
        self.push_log(LogLine {
            stamp: stamp.to_string(),
            kind: LogKind::of_event(event),
            text: event.human(),
        });
    }

    fn restart_engine(&self) -> Effect {
        Effect::StartEngine {
            settings: self.settings.clone(),
            dry_run: self.dry_run,
        }
    }

    /// The confirmed `Go live`: a new engine from the session settings.
    pub fn go_live(&mut self, stamp: &str) -> Vec<Effect> {
        self.dry_run = false;
        self.push_system("— engine started: LIVE (will switch accounts) —", stamp);
        vec![self.restart_engine()]
    }

    fn back_to_dry_run(&mut self, stamp: &str) -> Vec<Effect> {
        self.dry_run = true;
        self.push_system("— engine started: DRY-RUN (watching only) —", stamp);
        vec![self.restart_engine()]
    }

    fn step_threshold(&mut self, delta: f64) -> Vec<Effect> {
        let next = (self.settings.threshold + delta).clamp(THRESHOLD_MIN, THRESHOLD_MAX);
        self.settings.threshold = (next * 10.0).round() / 10.0;
        vec![Effect::ThresholdTick(Some(self.settings.threshold))]
    }

    fn finish_adjust(&mut self, stamp: &str) -> Vec<Effect> {
        self.adjusting = false;
        if self.settings.threshold == self.adjust_start {
            return Vec::new();
        }
        self.push_system(
            &format!(
                "— threshold set to {}% for this session —",
                pct_label(self.settings.threshold)
            ),
            stamp,
        );
        vec![self.restart_engine()]
    }

    pub fn handle_key(&mut self, key: KeyEvent, stamp: &str) -> Vec<Effect> {
        if !self.engine_available {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q') => vec![Effect::Pop],
                _ => Vec::new(),
            };
        }
        match key.code {
            KeyCode::Char('l') => {
                if self.dry_run {
                    vec![Effect::OpenModal(Modal::Confirm(ConfirmModal::go_live()))]
                } else {
                    self.back_to_dry_run(stamp)
                }
            }
            KeyCode::Char('t') => {
                if self.adjusting {
                    self.finish_adjust(stamp)
                } else {
                    self.adjusting = true;
                    self.adjust_start = self.settings.threshold;
                    Vec::new()
                }
            }
            KeyCode::Left if self.adjusting => self.step_threshold(-1.0),
            KeyCode::Right if self.adjusting => self.step_threshold(1.0),
            KeyCode::Enter if self.adjusting => self.finish_adjust(stamp),
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.adjusting {
                    self.finish_adjust(stamp)
                } else {
                    vec![Effect::Pop]
                }
            }
            _ => Vec::new(),
        }
    }

    pub fn badge(&self, p: &Palette) -> Span<'static> {
        if !self.engine_available {
            return Span::styled(" OFF ", p.muted_style());
        }
        if self.dry_run {
            Span::styled(
                " DRY-RUN ",
                Style::new()
                    .fg(p.warn)
                    .bg(p.panel)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                " LIVE ",
                Style::new()
                    .fg(p.bg)
                    .bg(p.accent)
                    .add_modifier(Modifier::BOLD),
            )
        }
    }

    /// `auto-switch · threshold 90% (session) · poll every 60s`.
    pub fn summary(&self, p: &Palette) -> Line<'static> {
        let threshold_style = if self.adjusting {
            p.accent_style()
        } else {
            p.muted_style()
        };
        let mut spans = vec![
            Span::styled("auto-switch · ", p.muted_style()),
            Span::styled(
                format!("threshold {}%", pct_label(self.settings.threshold)),
                threshold_style,
            ),
        ];
        if self.settings.threshold != self.configured_threshold {
            spans.push(Span::styled(" (session)", p.muted_style()));
        }
        spans.push(Span::styled(
            format!(" · poll every {:.0}s", self.settings.interval_seconds),
            p.muted_style(),
        ));
        if self.adjusting {
            spans.push(Span::styled("   ← → adjust · enter done", p.muted_style()));
        }
        Line::from(spans)
    }

    /// `Next best` plus one line per switchable non-active account, best
    /// headroom first, on the same window axis the engine uses.
    pub fn candidate_lines(
        &self,
        snapshot: Option<&AccountsSnapshot>,
        p: &Palette,
    ) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(Span::styled("Next best", p.muted_style()))];
        let models = self.settings.model_names();
        let mut rows: Vec<(f64, u32, Line<'static>)> = Vec::new();
        if let Some(snapshot) = snapshot {
            for acc in snapshot
                .accounts
                .iter()
                .filter(|a| !a.is_active && a.switchable())
            {
                let head =
                    Span::styled(format!("  {:>2}  {}", acc.number, acc.email), p.fg_style());
                let (key, tail) = if let Some(sentinel) = acc.usage.sentinel {
                    (
                        998.0,
                        Span::styled(format!("  {}", sentinel_label(sentinel)), p.muted_style()),
                    )
                } else {
                    match acc
                        .usage
                        .last_good
                        .as_ref()
                        .and_then(|last| binding_pct(last, &models))
                    {
                        Some(pct) => (
                            pct,
                            Span::styled(
                                format!("  {pct:>3.0}% used"),
                                Style::new().fg(p.severity(Some(pct))),
                            ),
                        ),
                        None => (999.0, Span::styled("  usage unknown", p.muted_style())),
                    }
                };
                rows.push((key, acc.number, Line::from(vec![head, tail])));
            }
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        if rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no other switchable accounts",
                p.muted_style(),
            )));
        } else {
            lines.extend(rows.into_iter().map(|(_, _, line)| line));
        }
        lines
    }

    /// The last `height` wrapped log rows at `width`.
    pub fn log_lines(&self, width: usize, height: usize, p: &Palette) -> Vec<Line<'static>> {
        let indent = 10usize;
        let body_width = width.saturating_sub(indent).max(8);
        let mut rows: Vec<Line<'static>> = Vec::new();
        for entry in &self.log {
            let style = match entry.kind {
                LogKind::Switch => p.accent_style(),
                LogKind::Warn => p.warn_style(),
                LogKind::Crit => p.crit_style(),
                LogKind::Muted => p.muted_style(),
                LogKind::Plain => p.fg_style(),
            };
            for (i, chunk) in wrap_text(&entry.text, body_width).into_iter().enumerate() {
                let prefix = if i == 0 {
                    format!("{}  ", entry.stamp)
                } else {
                    " ".repeat(indent)
                };
                rows.push(Line::from(vec![
                    Span::styled(prefix, p.muted_style()),
                    Span::styled(chunk, style),
                ]));
            }
        }
        let skip = rows.len().saturating_sub(height);
        rows.into_iter().skip(skip).collect()
    }

    pub fn footer(&self) -> Vec<(&'static str, &'static str)> {
        if !self.engine_available {
            return vec![("esc", "Back"), ("^t", "Theme")];
        }
        let mut chips = vec![("l", "Go live / dry-run"), ("t", "Threshold")];
        if self.adjusting {
            chips.push(("←", "-1%"));
            chips.push(("→", "+1%"));
            chips.push(("enter", "Done"));
        }
        chips.push(("esc", "Back"));
        chips.push(("^t", "Theme"));
        chips
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AccountRef;
    use crate::store::usage_store::UsageSentinel;
    use crate::tui::test_support::{account, entry, snapshot};
    use crate::tui::theme::DARK;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn settings() -> AutoSwitchSettings {
        AutoSwitchSettings::default()
    }

    #[test]
    fn opens_in_dry_run_and_needs_confirm_to_go_live() {
        let mut auto = AutoScreen::new(settings(), "20:39:01");
        assert!(auto.dry_run());
        assert_eq!(
            auto.log()[0].text,
            "— engine started: DRY-RUN (watching only) —"
        );
        let effects = auto.handle_key(key(KeyCode::Char('l')), "20:39:02");
        assert!(matches!(effects[0], Effect::OpenModal(Modal::Confirm(_))));
        assert!(auto.dry_run(), "no switch to live without confirmation");
        let effects = auto.go_live("20:39:03");
        assert_eq!(
            effects,
            vec![Effect::StartEngine {
                settings: settings(),
                dry_run: false
            }]
        );
        assert!(!auto.dry_run());
        assert_eq!(
            auto.log().last().unwrap().text,
            "— engine started: LIVE (will switch accounts) —"
        );
        let effects = auto.handle_key(key(KeyCode::Char('l')), "20:39:04");
        assert!(auto.dry_run(), "back to dry-run without confirmation");
        assert_eq!(
            effects,
            vec![Effect::StartEngine {
                settings: settings(),
                dry_run: true
            }]
        );
        assert_eq!(
            text(&auto.summary(&DARK)),
            "auto-switch · threshold 90% · poll every 60s"
        );
        assert_eq!(auto.badge(&DARK).content, " DRY-RUN ");
    }

    #[test]
    fn threshold_adjust_is_session_only_and_restarts_on_net_change() {
        let mut auto = AutoScreen::new(settings(), "x");
        assert!(
            auto.handle_key(key(KeyCode::Left), "x").is_empty(),
            "no-op unless adjusting"
        );
        auto.handle_key(key(KeyCode::Char('t')), "x");
        assert!(auto.adjusting());
        assert_eq!(
            auto.handle_key(key(KeyCode::Right), "x"),
            vec![Effect::ThresholdTick(Some(91.0))]
        );
        assert_eq!(
            text(&auto.summary(&DARK)),
            "auto-switch · threshold 91% (session) · poll every 60s   ← → adjust · enter done"
        );
        assert_eq!(auto.footer().len(), 7);
        for _ in 0..20 {
            auto.handle_key(key(KeyCode::Right), "x");
        }
        assert_eq!(auto.threshold(), 99.9, "clamped at 99.9");
        assert_eq!(
            auto.handle_key(key(KeyCode::Left), "x"),
            vec![Effect::ThresholdTick(Some(98.9))]
        );
        let effects = auto.handle_key(key(KeyCode::Enter), "x");
        assert!(!auto.adjusting());
        assert_eq!(
            effects,
            vec![Effect::StartEngine {
                settings: AutoSwitchSettings {
                    threshold: 98.9,
                    ..settings()
                },
                dry_run: true
            }]
        );
        assert_eq!(
            auto.log().last().unwrap().text,
            "— threshold set to 98.9% for this session —"
        );
        assert_eq!(auto.configured_threshold(), 90.0);

        // No net change: silent.
        auto.handle_key(key(KeyCode::Char('t')), "x");
        auto.handle_key(key(KeyCode::Right), "x");
        auto.handle_key(key(KeyCode::Left), "x");
        let before = auto.log().len();
        assert!(auto.handle_key(key(KeyCode::Esc), "x").is_empty());
        assert_eq!(auto.log().len(), before);
        assert_eq!(auto.handle_key(key(KeyCode::Esc), "x"), vec![Effect::Pop]);
        let mut low = AutoScreen::new(
            AutoSwitchSettings {
                threshold: 50.0,
                ..settings()
            },
            "x",
        );
        low.handle_key(key(KeyCode::Char('t')), "x");
        low.handle_key(key(KeyCode::Left), "x");
        assert_eq!(low.threshold(), 50.0, "clamped at 50");
    }

    #[test]
    fn candidates_rank_by_binding_pct() {
        let auto = AutoScreen::new(settings(), "x");
        let mut api = account(1, "alice@corp.io", false, entry(None, None));
        api.api_key = true;
        api.usage.sentinel = Some(UsageSentinel::ApiKey);
        let mut disabled = account(5, "off@x.y", false, entry(Some(900.0), Some(1.0)));
        disabled.disabled = true;
        let snap = snapshot(
            vec![
                api,
                account(2, "me@x.y", true, entry(Some(900.0), Some(76.0))),
                account(3, "john@company.com", false, entry(Some(900.0), Some(12.0))),
                account(4, "unknown@x.y", false, entry(None, None)),
                disabled,
            ],
            1000.0,
        );
        let lines = auto.candidate_lines(Some(&snap), &DARK);
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(
            texts,
            [
                "Next best",
                "   3  john@company.com   12% used",
                "   1  alice@corp.io  API key (no quota)",
                "   4  unknown@x.y  usage unknown",
            ]
        );
        let none = auto.candidate_lines(
            Some(&snapshot(
                vec![account(2, "me@x.y", true, entry(None, None))],
                0.0,
            )),
            &DARK,
        );
        assert_eq!(text(&none[1]), "  no other switchable accounts");
    }

    #[test]
    fn log_lines_wrap_and_color_by_kind() {
        let mut auto = AutoScreen::new(settings(), "20:39:01");
        auto.on_event(
            &Event::Switch {
                trigger: crate::autoswitch::Trigger::Proactive,
                from: Some(AccountRef {
                    number: Some(2),
                    email: "a@b.c".into(),
                }),
                to: Some(AccountRef {
                    number: Some(3),
                    email: "d@e.f".into(),
                }),
                warnings: Vec::new(),
                dry_run: true,
            },
            "20:41:03",
        );
        auto.on_event(
            &Event::AllExhausted {
                earliest_reset_at: None,
            },
            "20:42:00",
        );
        assert_eq!(auto.log()[1].kind, LogKind::Switch);
        assert_eq!(auto.log()[2].kind, LogKind::Crit);
        let lines = auto.log_lines(40, 10, &DARK);
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(texts[0], "20:39:01  — engine started: DRY-RUN");
        assert_eq!(texts[1], "          (watching only) —");
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("20:41:03  [dry-run] would switch"))
        );
        assert_eq!(lines[2].spans[1].style.fg, Some(DARK.accent));
        let tail = auto.log_lines(200, 1, &DARK);
        assert_eq!(tail.len(), 1);
        assert_eq!(
            text(&tail[0]),
            "20:42:00  all accounts exhausted; no reset time known"
        );
    }
    #[test]
    fn without_a_claude_account_there_is_no_engine_to_start() {
        let mut auto = AutoScreen::without_claude(settings(), "20:39:01");
        assert!(!auto.engine_available());
        assert_eq!(
            auto.log()[0].text,
            "— no Claude Code account: auto-switch covers Claude Code only (ccsw add claude) —"
        );
        assert_eq!(auto.badge(&DARK).content, " OFF ");
        assert!(
            auto.handle_key(key(KeyCode::Char('l')), "20:39:02")
                .is_empty(),
            "Go live does nothing"
        );
        assert!(
            auto.handle_key(key(KeyCode::Char('t')), "20:39:02")
                .is_empty(),
            "the threshold is not adjustable without an engine"
        );
        assert!(!auto.adjusting());
        assert_eq!(auto.footer(), vec![("esc", "Back"), ("^t", "Theme")]);
        assert_eq!(
            auto.handle_key(key(KeyCode::Esc), "20:39:03"),
            vec![Effect::Pop]
        );
    }
}
