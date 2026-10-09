//! The pure application state machine: screen stack, modal, toasts, snapshot
//! merging, action bookkeeping and rendering. The runtime in `mod.rs` feeds it
//! keys and worker messages and executes the [`Command`]s it returns, so
//! everything here runs unchanged against a `TestBackend`.

use chrono::{Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::autoswitch::Event;
use crate::cli::tui::TuiStart;
use crate::model::SwitchOutcome;
use crate::printer::format_duration;
use crate::provider::Provider;
use crate::store::AutoSwitchSettings;
use crate::switcher::{Line as UiLine, ListSnapshot};

use super::auto::AutoScreen;
use super::dashboard::DashboardScreen;
use super::modals::{
    ConfirmModal, LoginModal, Modal, ModalOutcome, OutputModal, PendingAction, TokenForm,
};
use super::snapshot::AccountsSnapshot;
use super::switch::SwitchScreen;
use super::theme::{Palette, ThemeName};
use super::watch::WatchScreen;
use super::widgets::{
    Severity, Toast, accounts_panel, footer_line, render_scrollbar, render_toasts,
};

pub const POLL_INTERVAL_S: f64 = 3.0;
pub const SNAPSHOT_AGE_NOTE_S: f64 = 60.0;
pub const TOAST_DEFAULT_S: f64 = 5.0;

/// A mutating switcher call run on a worker thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    SwitchTo(u32),
    SwitchBest(Provider),
    SetDisabled { number: u32, disabled: bool },
    Remove(u32),
    AddNew,
    AddCurrent,
    AddToken(TokenForm),
}

impl Action {
    pub fn label(&self) -> String {
        match self {
            Self::SwitchTo(n) => format!("Switch to account {n}"),
            Self::SwitchBest(provider) => format!("Switch (best, {provider})"),
            Self::SetDisabled {
                number,
                disabled: true,
            } => format!("Disable account {number}"),
            Self::SetDisabled { number, .. } => format!("Enable account {number}"),
            Self::Remove(n) => format!("Remove account {n}"),
            Self::AddNew => "Add new account".to_string(),
            Self::AddCurrent => "Add current login".to_string(),
            Self::AddToken(_) => "Add account from a token".to_string(),
        }
    }

    /// Add results open an output modal; everything else toasts.
    pub fn show_output(&self) -> bool {
        matches!(self, Self::AddNew | Self::AddCurrent | Self::AddToken(_))
    }
}

/// What a screen asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Quit,
    RefreshFull,
    Action(Action),
    StartEngine {
        settings: AutoSwitchSettings,
        dry_run: bool,
    },
    OpenSwitch,
    OpenWatch,
    OpenAuto,
    Pop,
    OpenModal(Modal),
    ApplyTheme(ThemeName),
    ThresholdTick(Option<f64>),
}

/// What the runtime must do after a key or message.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Quit,
    CancelLogin,
    Refresh {
        full: bool,
    },
    Action(Action),
    /// Load fresh settings and call [`App::open_auto`].
    OpenAuto,
    StartEngine {
        settings: AutoSwitchSettings,
        dry_run: bool,
    },
    StopEngine,
    PersistTheme(ThemeName),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Dashboard(DashboardScreen),
    Switch(SwitchScreen),
    Watch(WatchScreen),
    Auto(AutoScreen),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenKind {
    Dashboard,
    Switch,
    Watch,
    Auto,
}

impl Screen {
    pub fn kind(&self) -> ScreenKind {
        match self {
            Self::Dashboard(_) => ScreenKind::Dashboard,
            Self::Switch(_) => ScreenKind::Switch,
            Self::Watch(_) => ScreenKind::Watch,
            Self::Auto(_) => ScreenKind::Auto,
        }
    }
}

/// The outcome of an [`Action`] run by the runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionResult {
    pub action: Action,
    pub ok: bool,
    /// The human lines the switcher said, plus `Error: …` on failure.
    pub lines: Vec<UiLine>,
    pub switch: Option<SwitchOutcome>,
    /// What to do next after a switch (Claude Code's restart hint), spec §12.
    pub followup: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Normal,
    Store,
}

/// Messages from the worker threads.
#[derive(Debug, Clone)]
pub enum Inbound {
    Snapshot {
        lane: Lane,
        generation: u64,
        taken_at: f64,
        result: Result<Option<ListSnapshot>, String>,
    },
    ActionDone(ActionResult),
    LoginUrl(String),
    Engine(Event),
    EngineStopped(String),
}

/// Local `HH:MM:SS` for the event log.
pub fn clock_stamp(now: f64) -> String {
    Local
        .timestamp_opt(now as i64, 0)
        .single()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

pub struct App {
    screens: Vec<Screen>,
    modal: Option<Modal>,
    toasts: Vec<Toast>,
    snapshot: Option<AccountsSnapshot>,
    applied_generation: u64,
    threshold_pct: Option<f64>,
    theme: ThemeName,
    palette: Palette,
    colorfgbg: Option<String>,
    busy: bool,
    quit: bool,
    refreshing_since: Option<f64>,
    last_refresh_error: Option<String>,
}

impl App {
    pub fn new(
        start: TuiStart,
        theme: ThemeName,
        threshold: f64,
        colorfgbg: Option<String>,
    ) -> Self {
        let mut screens = vec![Screen::Dashboard(DashboardScreen::new())];
        if start == TuiStart::Watch {
            screens.push(Screen::Watch(WatchScreen::new()));
        }
        Self {
            screens,
            modal: None,
            toasts: Vec::new(),
            snapshot: None,
            applied_generation: 0,
            threshold_pct: Some(threshold),
            theme,
            palette: Palette::for_theme(theme.resolve(colorfgbg.as_deref())),
            colorfgbg,
            busy: false,
            quit: false,
            refreshing_since: None,
            last_refresh_error: None,
        }
    }

    // -- accessors ------------------------------------------------------------

    pub fn screen_kind(&self) -> ScreenKind {
        self.screens.last().expect("dashboard").kind()
    }

    pub fn screens(&self) -> &[Screen] {
        &self.screens
    }

    pub fn modal(&self) -> Option<&Modal> {
        self.modal.as_ref()
    }

    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    pub fn snapshot(&self) -> Option<&AccountsSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn threshold_pct(&self) -> Option<f64> {
        self.threshold_pct
    }

    pub fn theme(&self) -> ThemeName {
        self.theme
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    /// While the auto view is open the engine is the only fetcher.
    pub fn store_only(&self) -> bool {
        self.screens.iter().any(|s| s.kind() == ScreenKind::Auto)
    }

    pub fn dashboard(&self) -> &DashboardScreen {
        match &self.screens[0] {
            Screen::Dashboard(d) => d,
            _ => unreachable!("the dashboard is always at the bottom"),
        }
    }

    pub fn auto(&self) -> Option<&AutoScreen> {
        self.screens.iter().rev().find_map(|s| match s {
            Screen::Auto(a) => Some(a),
            _ => None,
        })
    }

    pub fn watch(&self) -> Option<&WatchScreen> {
        self.screens.iter().rev().find_map(|s| match s {
            Screen::Watch(w) => Some(w),
            _ => None,
        })
    }

    pub fn switch_screen(&self) -> Option<&SwitchScreen> {
        self.screens.iter().rev().find_map(|s| match s {
            Screen::Switch(w) => Some(w),
            _ => None,
        })
    }

    /// `snapshot 1m ago · refreshing 5s`; empty while polling is healthy.
    pub fn refresh_status(&self, now: f64) -> String {
        let mut parts = Vec::new();
        if let Some(snapshot) = &self.snapshot {
            let age = now - snapshot.taken_at;
            if age >= SNAPSHOT_AGE_NOTE_S {
                parts.push(format!("snapshot {} ago", format_duration(age)));
            }
        }
        if let Some(since) = self.refreshing_since {
            let elapsed = now - since;
            if elapsed >= POLL_INTERVAL_S {
                parts.push(format!("refreshing {}", format_duration(elapsed)));
            }
        }
        parts.join(" · ")
    }

    pub fn set_refreshing_since(&mut self, since: Option<f64>) {
        self.refreshing_since = since;
    }

    // -- toasts ---------------------------------------------------------------

    pub fn toast(
        &mut self,
        text: impl Into<String>,
        title: Option<&str>,
        severity: Severity,
        timeout_s: f64,
        now: f64,
    ) {
        self.toasts.push(Toast {
            title: title.map(str::to_string),
            text: text.into(),
            severity,
            expires_at: now + timeout_s,
        });
    }

    /// Drop expired toasts.
    pub fn tick(&mut self, now: f64) {
        self.toasts.retain(|t| t.expires_at > now);
    }

    // -- snapshots ------------------------------------------------------------

    /// Generation-ordered apply: a newer result replaces the snapshot (usage
    /// reconciled against the previous one); an older result only merges its
    /// usage rows.
    pub fn apply_snapshot(&mut self, incoming: AccountsSnapshot, generation: u64, now: f64) {
        if generation > self.applied_generation {
            self.applied_generation = generation;
            let reconciled = incoming.reconcile(self.snapshot.as_ref());
            self.snapshot = Some(reconciled);
        } else if let Some(current) = &mut self.snapshot {
            current.merge_usage(&incoming);
        } else {
            self.snapshot = Some(incoming);
        }
        let snapshot = self.snapshot.clone().expect("just set");
        for screen in &mut self.screens {
            match screen {
                Screen::Switch(s) => s.sync(&snapshot, now),
                Screen::Watch(w) => w.sync(&snapshot, now),
                _ => {}
            }
        }
    }

    // -- theme ----------------------------------------------------------------

    fn apply_theme(&mut self, name: ThemeName, now: f64) -> Command {
        self.theme = name;
        self.palette = Palette::for_theme(name.resolve(self.colorfgbg.as_deref()));
        self.toast(
            format!("Theme: {}", name.as_str()),
            None,
            Severity::Info,
            TOAST_DEFAULT_S,
            now,
        );
        Command::PersistTheme(name)
    }

    /// The runtime reports a failed `ui.theme` write.
    pub fn theme_save_failed(&mut self, error: &str, now: f64) {
        self.toast(
            format!("Could not save theme: {error}"),
            None,
            Severity::Warning,
            TOAST_DEFAULT_S,
            now,
        );
    }

    // -- navigation -----------------------------------------------------------

    fn push_unless_open(&mut self, screen: Screen) {
        if self.screen_kind() != screen.kind() {
            if let Some(snapshot) = self.snapshot.clone() {
                let mut screen = screen;
                match &mut screen {
                    Screen::Switch(s) => s.sync(&snapshot, snapshot.taken_at),
                    Screen::Watch(w) => w.sync(&snapshot, snapshot.taken_at),
                    _ => {}
                }
                self.screens.push(screen);
            } else {
                self.screens.push(screen);
            }
        }
    }

    /// Push the auto view with freshly loaded settings; starts a dry-run engine.
    pub fn open_auto(&mut self, settings: AutoSwitchSettings, now: f64) -> Vec<Command> {
        if self.screen_kind() == ScreenKind::Auto {
            return Vec::new();
        }
        self.threshold_pct = Some(settings.threshold);
        let stamp = clock_stamp(now);
        let has_claude = self
            .snapshot
            .as_ref()
            .is_some_and(|s| s.accounts.iter().any(|a| a.provider == Provider::Claude));
        if !has_claude {
            self.screens
                .push(Screen::Auto(AutoScreen::without_claude(settings, &stamp)));
            return vec![Command::Refresh { full: false }];
        }
        let screen = AutoScreen::new(settings.clone(), &stamp);
        self.screens.push(Screen::Auto(screen));
        vec![
            Command::StartEngine {
                settings,
                dry_run: true,
            },
            Command::Refresh { full: false },
        ]
    }

    fn pop_screen(&mut self) -> Vec<Command> {
        if self.screens.len() <= 1 {
            return Vec::new();
        }
        let popped = self.screens.pop().expect("more than one screen");
        match popped {
            Screen::Auto(auto) => {
                self.threshold_pct = Some(auto.configured_threshold());
                vec![Command::StopEngine, Command::Refresh { full: false }]
            }
            _ => Vec::new(),
        }
    }

    // -- actions --------------------------------------------------------------

    fn start_action(&mut self, action: Action, now: f64) -> Option<Command> {
        if self.busy {
            self.toast(
                "Another action is still running",
                None,
                Severity::Warning,
                TOAST_DEFAULT_S,
                now,
            );
            return None;
        }
        self.busy = true;
        if action == Action::AddNew {
            self.modal = Some(Modal::Login(LoginModal {
                url: None,
                cancelling: false,
                scroll: 0,
            }));
        }
        Some(Command::Action(action))
    }

    fn action_done(&mut self, result: ActionResult, now: f64) -> Vec<Command> {
        self.busy = false;
        let commands = vec![Command::Refresh { full: false }];
        let label = result.action.label();
        if !result.ok {
            self.modal = Some(Modal::Output(OutputModal::new(
                format!("{label} — failed"),
                result.lines,
            )));
            return commands;
        }
        if let Some(outcome) = result.switch {
            if outcome.switched {
                let target = outcome
                    .to
                    .as_ref()
                    .map(|to| {
                        if to.email.is_empty() {
                            format!("account {}", to.number.unwrap_or(0))
                        } else {
                            to.email.clone()
                        }
                    })
                    .unwrap_or_else(|| "account".to_string());
                self.toast(
                    format!("Switched to {target}"),
                    Some("Switch"),
                    Severity::Info,
                    TOAST_DEFAULT_S,
                    now,
                );
                // Spec §12: the provider that acted says what happens next.
                if outcome.provider == Provider::Claude
                    && let Some(followup) = result.followup
                {
                    self.toast(followup, None, Severity::Info, TOAST_DEFAULT_S, now);
                }
            } else {
                let reason = if outcome.message.is_empty() {
                    if outcome.reason.is_empty() {
                        "no switch performed".to_string()
                    } else {
                        outcome.reason.clone()
                    }
                } else {
                    outcome.message.clone()
                };
                self.toast(
                    reason,
                    Some("No switch"),
                    Severity::Warning,
                    TOAST_DEFAULT_S,
                    now,
                );
            }
            return commands;
        }
        let first = result
            .lines
            .iter()
            .map(UiLine::text)
            .find(|t| !t.trim().is_empty());
        if result.action.show_output() && first.is_some() {
            self.modal = Some(Modal::Output(OutputModal::new(label, result.lines)));
        } else if let Some(first) = first {
            self.toast(first, None, Severity::Info, TOAST_DEFAULT_S, now);
        }
        commands
    }

    fn submit_token_form(&mut self, form: TokenForm, now: f64) -> Vec<Command> {
        if let Some(slot) = form.slot
            && let Some(occupant) = self.snapshot.as_ref().and_then(|s| s.account(slot))
        {
            let email = occupant.email.clone();
            self.modal = Some(Modal::Confirm(ConfirmModal::overwrite_slot(
                slot, &email, form,
            )));
            return Vec::new();
        }
        self.start_action(Action::AddToken(form), now)
            .into_iter()
            .collect()
    }

    fn confirmed(&mut self, action: PendingAction, now: f64) -> Vec<Command> {
        match action {
            PendingAction::AddCurrent => self
                .start_action(Action::AddCurrent, now)
                .into_iter()
                .collect(),
            PendingAction::Remove(number) => self
                .start_action(Action::Remove(number), now)
                .into_iter()
                .collect(),
            PendingAction::AddToken(form) => self
                .start_action(Action::AddToken(form), now)
                .into_iter()
                .collect(),
            PendingAction::GoLive => {
                let stamp = clock_stamp(now);
                let effects = match self.screens.last_mut() {
                    Some(Screen::Auto(auto)) => auto.go_live(&stamp),
                    _ => Vec::new(),
                };
                self.fold(effects, now)
            }
        }
    }

    // -- keys -----------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent, now: f64) -> Vec<Command> {
        if matches!(self.modal, Some(Modal::Login(_)))
            && key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.quit = true;
            return vec![Command::Quit];
        }
        if let Some(modal) = &mut self.modal {
            return match modal.handle_key(key) {
                ModalOutcome::Open => Vec::new(),
                ModalOutcome::Closed => {
                    self.modal = None;
                    Vec::new()
                }
                ModalOutcome::Confirmed(action) => {
                    self.modal = None;
                    self.confirmed(action, now)
                }
                ModalOutcome::Submitted(form) => {
                    self.modal = None;
                    self.submit_token_form(form, now)
                }
                ModalOutcome::CancelLogin => {
                    if let Some(Modal::Login(login)) = &mut self.modal {
                        login.cancelling = true;
                    }
                    vec![Command::CancelLogin]
                }
            };
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('t') => {
                    let next = self.theme.next();
                    return vec![self.apply_theme(next, now)];
                }
                // Raw mode swallows SIGINT; ctrl+c quits like `q`.
                KeyCode::Char('c') => {
                    self.quit = true;
                    return vec![Command::Quit];
                }
                _ => {}
            }
        }
        let snapshot = self.snapshot.clone();
        let theme = self.theme;
        let stamp = clock_stamp(now);
        let effects = match self.screens.last_mut().expect("dashboard") {
            Screen::Dashboard(d) => d.handle_key(key, snapshot.as_ref(), theme),
            Screen::Switch(s) => s.handle_key(key, snapshot.as_ref()),
            Screen::Watch(w) => w.handle_key(key, snapshot.as_ref()),
            Screen::Auto(a) => a.handle_key(key, &stamp),
        };
        self.fold(effects, now)
    }

    /// The mouse wheel: the dashboard scrolls its accounts panel, the switch
    /// and watch screens their card lists. Modals swallow it.
    pub fn handle_scroll(&mut self, delta: i64) {
        if self.modal.is_some() {
            return;
        }
        match self.screens.last_mut().expect("dashboard") {
            Screen::Dashboard(d) => d.scroll_panel(delta),
            Screen::Switch(s) => s.list.scroll_by(delta),
            Screen::Watch(w) => w.list.scroll_by(delta),
            Screen::Auto(_) => {}
        }
    }

    fn fold(&mut self, effects: Vec<Effect>, now: f64) -> Vec<Command> {
        let mut commands = Vec::new();
        for effect in effects {
            match effect {
                Effect::Quit => {
                    self.quit = true;
                    commands.push(Command::Quit);
                }
                Effect::RefreshFull => {
                    self.toast("Refreshing usage…", None, Severity::Info, 2.0, now);
                    commands.push(Command::Refresh { full: true });
                }
                Effect::Action(action) => commands.extend(self.start_action(action, now)),
                Effect::StartEngine { settings, dry_run } => {
                    commands.push(Command::StartEngine { settings, dry_run });
                }
                Effect::OpenSwitch => self.push_unless_open(Screen::Switch(SwitchScreen::new())),
                Effect::OpenWatch => self.push_unless_open(Screen::Watch(WatchScreen::new())),
                Effect::OpenAuto => {
                    if self.screen_kind() != ScreenKind::Auto {
                        commands.push(Command::OpenAuto);
                    }
                }
                Effect::Pop => commands.extend(self.pop_screen()),
                Effect::OpenModal(modal) => self.modal = Some(modal),
                Effect::ApplyTheme(name) => commands.push(self.apply_theme(name, now)),
                Effect::ThresholdTick(value) => self.threshold_pct = value,
            }
        }
        commands
    }

    // -- worker messages ------------------------------------------------------

    pub fn receive(&mut self, message: Inbound, now: f64) -> Vec<Command> {
        match message {
            Inbound::Snapshot {
                lane,
                generation,
                taken_at,
                result,
            } => {
                match result {
                    Ok(list) => {
                        let snapshot = match list {
                            Some(list) => AccountsSnapshot::from_list(list, taken_at),
                            None => AccountsSnapshot::empty(taken_at),
                        };
                        self.apply_snapshot(snapshot, generation, now);
                        self.last_refresh_error = None;
                    }
                    Err(message) => {
                        let text = match lane {
                            Lane::Normal => format!("Refresh failed: {message}"),
                            Lane::Store => format!("Store refresh failed: {message}"),
                        };
                        if self.last_refresh_error.as_deref() != Some(&text) {
                            self.toast(text.clone(), None, Severity::Warning, 6.0, now);
                            self.last_refresh_error = Some(text);
                        }
                    }
                }
                Vec::new()
            }
            Inbound::ActionDone(result) => self.action_done(result, now),
            Inbound::LoginUrl(url) => {
                if let Some(Modal::Login(login)) = &mut self.modal {
                    login.url = Some(url);
                }
                Vec::new()
            }
            Inbound::Engine(event) => {
                let stamp = clock_stamp(now);
                if let Some(Screen::Auto(auto)) = self.screens.last_mut() {
                    auto.on_event(&event, &stamp);
                }
                match event {
                    Event::Switch { .. } => vec![Command::Refresh { full: false }],
                    _ => Vec::new(),
                }
            }
            Inbound::EngineStopped(error) => {
                self.toast(
                    format!("Auto-switch engine stopped: {error}"),
                    None,
                    Severity::Error,
                    TOAST_DEFAULT_S,
                    now,
                );
                Vec::new()
            }
        }
    }

    // -- rendering ------------------------------------------------------------

    pub fn render(&mut self, frame: &mut Frame, now: f64) {
        let area = frame.area();
        self.draw(area, frame.buffer_mut(), now);
    }

    pub fn draw(&mut self, area: Rect, buf: &mut Buffer, now: f64) {
        let p = self.palette;
        buf.set_style(area, Style::new().bg(p.bg).fg(p.fg));
        let [body, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
        let snapshot = self.snapshot.clone();
        let threshold = self.threshold_pct;
        let status = self.refresh_status(now);
        let chips: Vec<(&str, &str)>;
        match self.screens.last_mut().expect("dashboard") {
            Screen::Dashboard(dash) => {
                chips = dash.footer();
                draw_dashboard(dash, snapshot.as_ref(), threshold, body, buf, now, &p);
            }
            Screen::Switch(switch) => {
                chips = switch.footer();
                let title = switch.title();
                draw_list(
                    &mut switch.list,
                    &title,
                    snapshot.as_ref(),
                    body,
                    buf,
                    now,
                    &p,
                );
            }
            Screen::Watch(watch) => {
                chips = watch.footer();
                let title = watch.title(&status);
                draw_list(
                    &mut watch.list,
                    &title,
                    snapshot.as_ref(),
                    body,
                    buf,
                    now,
                    &p,
                );
            }
            Screen::Auto(auto) => {
                chips = auto.footer();
                draw_auto(auto, snapshot.as_ref(), threshold, body, buf, now, &p);
            }
        }
        Paragraph::new(footer_line(&chips, &p))
            .style(Style::new().bg(p.surface))
            .render(footer, buf);
        if let Some(modal) = &mut self.modal {
            super::modals::render_modal(buf, area, modal, &p);
        }
        render_toasts(buf, body, &self.toasts, &p);
    }
}

fn indent(line: Line<'static>, by: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(by))];
    spans.extend(line.spans);
    Line::from(spans).style(line.style)
}

/// Pad a styled line with spaces so its background fills the row.
fn fill(mut line: Line<'static>, width: usize) -> Line<'static> {
    let used = line.width();
    if used < width {
        line.push_span(Span::raw(" ".repeat(width - used)));
    }
    line
}

fn rule(width: usize, p: &Palette) -> Line<'static> {
    Line::from(Span::styled("─".repeat(width), Style::new().fg(p.panel)))
}

fn draw_dashboard(
    dash: &mut DashboardScreen,
    snapshot: Option<&AccountsSnapshot>,
    threshold: Option<f64>,
    area: Rect,
    buf: &mut Buffer,
    now: f64,
    p: &Palette,
) {
    let width = area.width as usize;
    let content_width = width.saturating_sub(6);
    let panel = accounts_panel(
        snapshot,
        content_width.saturating_sub(2),
        threshold,
        true,
        now,
        p,
    );
    // Blank above the panel; blank, rule and blank above the menu.
    const CHROME: usize = 4;
    let height = area.height as usize;
    // The panel keeps its full height: the menu takes what is left and
    // scrolls, giving up rows down to its minimum. A panel that still does
    // not fit scrolls (wheel, PgUp/PgDn) behind a scrollbar.
    let spare = height.saturating_sub(CHROME + panel.len());
    let menu = dash.menu_lines(spare, p);
    let room = height.saturating_sub(CHROME + menu.len());
    let panel = dash.panel_window(panel, room);
    let mut lines: Vec<Line<'static>> = vec![Line::default()];
    lines.extend(panel.into_iter().map(|l| indent(l, 3)));
    lines.push(Line::default());
    lines.push(rule(width, p));
    lines.push(Line::default());
    for (i, line) in menu.into_iter().enumerate() {
        let line = if i == 0 {
            indent(line, 3)
        } else if line.spans.is_empty() {
            line
        } else {
            let highlighted = line.style.bg.is_some();
            let line = indent(line, 1);
            if highlighted { fill(line, width) } else { line }
        };
        lines.push(line);
    }
    Paragraph::new(lines).render(area, buf);
    let bar =
        Rect::new(area.right().saturating_sub(1), area.y + 1, 1, room as u16).intersection(area);
    render_scrollbar(buf, bar, dash.panel_scroll_info(), p);
}

fn draw_list(
    list: &mut super::switch::CardList,
    title: &str,
    snapshot: Option<&AccountsSnapshot>,
    area: Rect,
    buf: &mut Buffer,
    now: f64,
    p: &Palette,
) {
    let width = area.width as usize;
    let mut lines: Vec<Line<'static>> = vec![
        Line::default(),
        Line::from(Span::styled(format!("   {title}"), p.muted_style())),
        Line::default(),
    ];
    let height = (area.height as usize).saturating_sub(lines.len());
    let cards = list.render_lines(snapshot, width.saturating_sub(3), height, now, p);
    for line in cards {
        let styled = line.style.bg.is_some();
        let line = indent(line, 1);
        lines.push(if styled { fill(line, width) } else { line });
    }
    Paragraph::new(lines).render(area, buf);
    let bar =
        Rect::new(area.right().saturating_sub(1), area.y + 3, 1, height as u16).intersection(area);
    render_scrollbar(buf, bar, list.scroll_info(), p);
}

fn draw_auto(
    auto: &AutoScreen,
    snapshot: Option<&AccountsSnapshot>,
    threshold: Option<f64>,
    area: Rect,
    buf: &mut Buffer,
    now: f64,
    p: &Palette,
) {
    let claude = snapshot.map(|s| s.only(Provider::Claude));
    let snapshot = claude.as_ref();
    let width = area.width as usize;
    let content_width = width.saturating_sub(6);
    let mut lines: Vec<Line<'static>> = vec![Line::default()];
    lines.extend(
        accounts_panel(
            snapshot,
            content_width.saturating_sub(2),
            threshold,
            false,
            now,
            p,
        )
        .into_iter()
        .map(|l| indent(l, 3)),
    );
    lines.push(Line::default());
    let mut title = vec![auto.badge(p), Span::raw("   ")];
    title.extend(auto.summary(p).spans);
    lines.push(indent(Line::from(title), 2));
    lines.push(Line::default());
    lines.extend(
        auto.candidate_lines(snapshot, p)
            .into_iter()
            .map(|l| indent(l, 2)),
    );
    let min_log = 4usize;
    let top_max = (area.height as usize).saturating_sub(min_log + 1);
    lines.truncate(top_max);
    lines.push(rule(width, p));
    let log_height = (area.height as usize).saturating_sub(lines.len());
    lines.extend(
        auto.log_lines(width.saturating_sub(2), log_height, p)
            .into_iter()
            .map(|l| indent(l, 1)),
    );
    Paragraph::new(lines).render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AccountRef;
    use crate::provider::Provider;
    use crate::tui::test_support::{account, entry, snapshot};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn app() -> App {
        App::new(TuiStart::Dashboard, ThemeName::Dark, 90.0, None)
    }

    fn two() -> AccountsSnapshot {
        snapshot(
            vec![
                account(1, "a@x.y", false, entry(Some(900.0), Some(10.0))),
                account(2, "b@x.y", true, entry(Some(900.0), Some(20.0))),
            ],
            1000.0,
        )
    }

    #[test]
    fn watch_start_stacks_on_the_dashboard() {
        let mut app = App::new(TuiStart::Watch, ThemeName::Dark, 90.0, None);
        assert_eq!(app.screen_kind(), ScreenKind::Watch);
        assert_eq!(app.screens().len(), 2);
        app.handle_key(key(KeyCode::Esc), 0.0);
        assert_eq!(app.screen_kind(), ScreenKind::Dashboard);
        app.handle_key(key(KeyCode::Esc), 0.0);
        assert_eq!(app.screens().len(), 1, "Esc at the root stays");
    }

    #[test]
    fn actions_are_single_flight_and_end_with_a_refresh() {
        let mut app = app();
        app.apply_snapshot(two(), 1, 1000.0);
        app.handle_key(key(KeyCode::Char('s')), 1000.0);
        assert_eq!(app.screen_kind(), ScreenKind::Switch);
        let commands = app.handle_key(key(KeyCode::Enter), 1000.0);
        assert_eq!(commands, vec![Command::Action(Action::SwitchTo(2))]);
        assert_eq!(
            app.screen_kind(),
            ScreenKind::Dashboard,
            "Switch pops at once"
        );
        assert!(app.busy());
        app.handle_key(key(KeyCode::Char('s')), 1001.0);
        assert!(app.handle_key(key(KeyCode::Char('b')), 1001.0).is_empty());
        assert_eq!(app.toasts()[0].text, "Another action is still running");
        app.handle_key(key(KeyCode::Esc), 1001.0);
        let commands = app.receive(
            Inbound::ActionDone(ActionResult {
                action: Action::SwitchTo(2),
                ok: true,
                lines: Vec::new(),
                switch: Some(SwitchOutcome {
                    switched: true,
                    provider: Provider::Codex,
                    from: None,
                    to: Some(AccountRef {
                        number: Some(2),
                        email: "b@x.y".into(),
                    }),
                    strategy: "direct".into(),
                    reason: "switched".into(),
                    message: String::new(),
                    warnings: Vec::new(),
                }),
                followup: None,
            }),
            1002.0,
        );
        assert_eq!(commands, vec![Command::Refresh { full: false }]);
        assert!(!app.busy());
        let toast = app.toasts().last().unwrap();
        assert_eq!(toast.text, "Switched to b@x.y");
        assert_eq!(toast.title.as_deref(), Some("Switch"));
        assert!(app.modal().is_none(), "switch results never open a modal");
    }

    #[test]
    fn a_claude_switch_toasts_its_followup() {
        let mut app = app();
        let outcome = |provider| SwitchOutcome {
            switched: true,
            provider,
            from: None,
            to: Some(AccountRef {
                number: Some(2),
                email: "c@x.y".into(),
            }),
            strategy: "direct".into(),
            reason: "switched".into(),
            message: String::new(),
            warnings: Vec::new(),
        };
        app.receive(
            Inbound::ActionDone(ActionResult {
                action: Action::SwitchTo(2),
                ok: true,
                lines: Vec::new(),
                switch: Some(outcome(Provider::Claude)),
                followup: Some(crate::claude::live::FILE_FOLLOWUP.to_string()),
            }),
            1.0,
        );
        let texts: Vec<&str> = app.toasts().iter().map(|t| t.text.as_str()).collect();
        assert_eq!(
            texts,
            ["Switched to c@x.y", crate::claude::live::FILE_FOLLOWUP]
        );
    }

    #[test]
    fn no_switch_failure_and_add_results() {
        let mut app = app();
        app.receive(
            Inbound::ActionDone(ActionResult {
                action: Action::SwitchBest(Provider::Codex),
                ok: true,
                lines: Vec::new(),
                switch: Some(SwitchOutcome {
                    switched: false,
                    provider: Provider::Codex,
                    from: None,
                    to: None,
                    strategy: "best".into(),
                    reason: "already-best".into(),
                    message: "Already on the best account".into(),
                    warnings: Vec::new(),
                }),
                followup: None,
            }),
            1.0,
        );
        let toast = app.toasts().last().unwrap();
        assert_eq!(toast.text, "Already on the best account");
        assert_eq!(toast.title.as_deref(), Some("No switch"));
        assert_eq!(toast.severity, Severity::Warning);
        assert_eq!(
            Action::SwitchBest(Provider::Claude).label(),
            "Switch (best, claude)"
        );

        app.receive(
            Inbound::ActionDone(ActionResult {
                action: Action::Remove(3),
                ok: false,
                lines: vec![UiLine::plain("Error: boom")],
                switch: None,
                followup: None,
            }),
            2.0,
        );
        match app.modal() {
            Some(Modal::Output(o)) => assert_eq!(o.title, "Remove account 3 — failed"),
            other => panic!("unexpected {other:?}"),
        }
        app.handle_key(key(KeyCode::Esc), 2.0);
        assert!(app.modal().is_none());

        app.receive(
            Inbound::ActionDone(ActionResult {
                action: Action::AddCurrent,
                ok: true,
                lines: vec![UiLine::plain("Added Account-4 (x@y.z)")],
                switch: None,
                followup: None,
            }),
            3.0,
        );
        assert!(matches!(app.modal(), Some(Modal::Output(o)) if o.title == "Add current login"));
        app.handle_key(key(KeyCode::Enter), 3.0);

        app.receive(
            Inbound::ActionDone(ActionResult {
                action: Action::SetDisabled {
                    number: 1,
                    disabled: true,
                },
                ok: true,
                lines: vec![
                    UiLine::plain(""),
                    UiLine::plain("Disabled Account-1 (a@x.y)."),
                ],
                switch: None,
                followup: None,
            }),
            4.0,
        );
        assert_eq!(
            app.toasts().last().unwrap().text,
            "Disabled Account-1 (a@x.y)."
        );
        assert!(app.modal().is_none());
    }

    #[test]
    fn token_form_checks_the_occupied_slot() {
        let mut app = app();
        app.apply_snapshot(two(), 1, 1000.0);
        app.handle_key(key(KeyCode::Char('j')), 1000.0);
        app.handle_key(key(KeyCode::Char('j')), 1000.0);
        app.handle_key(key(KeyCode::Char('j')), 1000.0);
        app.handle_key(key(KeyCode::Enter), 1000.0);
        assert_eq!(app.dashboard().breadcrumb(), "menu › add account");
        app.handle_key(key(KeyCode::Char('j')), 1000.0);
        app.handle_key(key(KeyCode::Char('j')), 1000.0);
        app.handle_key(key(KeyCode::Enter), 1000.0);
        assert!(matches!(app.modal(), Some(Modal::AddToken(_))));
        for c in "sk-x".chars() {
            app.handle_key(key(KeyCode::Char(c)), 1000.0);
        }
        app.handle_key(key(KeyCode::Tab), 1000.0);
        app.handle_key(key(KeyCode::Tab), 1000.0);
        app.handle_key(key(KeyCode::Char('2')), 1000.0);
        assert!(app.handle_key(key(KeyCode::Enter), 1000.0).is_empty());
        match app.modal() {
            Some(Modal::Confirm(c)) => {
                assert_eq!(c.message, "Slot 2 is occupied by b@x.y. Overwrite?");
            }
            other => panic!("unexpected {other:?}"),
        }
        let commands = app.handle_key(key(KeyCode::Char('y')), 1000.0);
        assert_eq!(
            commands,
            vec![Command::Action(Action::AddToken(TokenForm {
                token: "sk-x".into(),
                email: None,
                slot: Some(2)
            }))]
        );
    }

    #[test]
    fn auto_view_lifecycle_restores_the_file_threshold() {
        let mut app = app();
        let mut supported = two();
        for account in &mut supported.accounts {
            account.provider = Provider::Claude;
        }
        app.apply_snapshot(supported, 1, 1000.0);
        let commands = app.handle_key(key(KeyCode::Char('g')), 1000.0);
        assert_eq!(commands, vec![Command::OpenAuto]);
        let settings = AutoSwitchSettings {
            threshold: 85.0,
            ..AutoSwitchSettings::default()
        };
        let commands = app.open_auto(settings.clone(), 1000.0);
        assert_eq!(
            commands,
            vec![
                Command::StartEngine {
                    settings: settings.clone(),
                    dry_run: true
                },
                Command::Refresh { full: false }
            ]
        );
        assert!(app.store_only());
        assert_eq!(app.threshold_pct(), Some(85.0));
        app.handle_key(key(KeyCode::Char('t')), 1001.0);
        app.handle_key(key(KeyCode::Right), 1001.0);
        assert_eq!(app.threshold_pct(), Some(86.0), "the tick moves live");
        let commands = app.handle_key(key(KeyCode::Enter), 1001.0);
        assert_eq!(
            commands,
            vec![Command::StartEngine {
                settings: AutoSwitchSettings {
                    threshold: 86.0,
                    ..settings.clone()
                },
                dry_run: true
            }]
        );
        app.handle_key(key(KeyCode::Char('l')), 1002.0);
        assert!(matches!(app.modal(), Some(Modal::Confirm(c)) if c.title == "Go live"));
        let commands = app.handle_key(key(KeyCode::Enter), 1002.0);
        assert_eq!(
            commands,
            vec![Command::StartEngine {
                settings: AutoSwitchSettings {
                    threshold: 86.0,
                    ..settings
                },
                dry_run: false
            }]
        );
        assert!(!app.auto().unwrap().dry_run());
        let commands = app.receive(
            Inbound::Engine(Event::Switch {
                trigger: crate::autoswitch::Trigger::AtLimit,
                from: None,
                to: None,
                warnings: Vec::new(),
                dry_run: false,
            }),
            1003.0,
        );
        assert_eq!(commands, vec![Command::Refresh { full: false }]);
        assert_eq!(app.auto().unwrap().log().len(), 4);
        let commands = app.handle_key(key(KeyCode::Esc), 1004.0);
        assert_eq!(
            commands,
            vec![Command::StopEngine, Command::Refresh { full: false }]
        );
        assert_eq!(app.screen_kind(), ScreenKind::Dashboard);
        assert_eq!(app.threshold_pct(), Some(85.0), "file threshold restored");
        assert!(!app.store_only());
    }

    #[test]
    fn snapshot_generations_and_refresh_status() {
        let mut app = app();
        assert_eq!(app.refresh_status(1000.0), "");
        app.receive(
            Inbound::Snapshot {
                lane: Lane::Normal,
                generation: 2,
                taken_at: 1000.0,
                result: Ok(None),
            },
            1000.0,
        );
        assert_eq!(app.snapshot().unwrap().accounts.len(), 0);
        app.apply_snapshot(two(), 3, 1000.0);
        let mut stale = two();
        stale.active_number = Some(1);
        stale.accounts[0].usage.fetched_at = Some(990.0);
        app.apply_snapshot(stale, 1, 1001.0);
        let current = app.snapshot().unwrap();
        assert_eq!(
            current.active_number,
            Some(2),
            "older generation keeps metadata"
        );
        assert_eq!(
            current.accounts[0].usage.fetched_at,
            Some(990.0),
            "but merges usage"
        );
        assert_eq!(app.refresh_status(1060.0), "snapshot 1m ago");
        app.set_refreshing_since(Some(1055.0));
        assert_eq!(
            app.refresh_status(1060.0),
            "snapshot 1m ago · refreshing 5s"
        );
        app.set_refreshing_since(None);

        app.receive(
            Inbound::Snapshot {
                lane: Lane::Normal,
                generation: 4,
                taken_at: 1002.0,
                result: Err("boom".into()),
            },
            1002.0,
        );
        app.receive(
            Inbound::Snapshot {
                lane: Lane::Normal,
                generation: 5,
                taken_at: 1003.0,
                result: Err("boom".into()),
            },
            1003.0,
        );
        let failures: Vec<&Toast> = app
            .toasts()
            .iter()
            .filter(|t| t.text == "Refresh failed: boom")
            .collect();
        assert_eq!(failures.len(), 1, "deduplicated");
        assert_eq!(failures[0].expires_at, 1008.0);
        app.tick(1009.0);
        assert!(app.toasts().is_empty());
    }

    #[test]
    fn theme_cycles_and_persists() {
        let mut app = app();
        let ctrl_t = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert_eq!(
            app.handle_key(ctrl_t, 0.0),
            vec![Command::PersistTheme(ThemeName::Light)]
        );
        assert_eq!(app.theme(), ThemeName::Light);
        assert_eq!(app.palette().accent, crate::tui::theme::LIGHT.accent);
        assert_eq!(app.toasts()[0].text, "Theme: light");
        app.handle_key(ctrl_t, 0.0);
        app.handle_key(ctrl_t, 0.0);
        assert_eq!(app.theme(), ThemeName::Dark);
        app.theme_save_failed("read-only", 0.0);
        assert_eq!(
            app.toasts().last().unwrap().text,
            "Could not save theme: read-only"
        );
    }
}
