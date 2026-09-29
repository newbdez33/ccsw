//! Renders the dashboard, switch, watch and auto screens through ratatui's
//! `TestBackend` from a hand-built snapshot: no terminal, no network, no
//! `Switcher`. Asserts the research notes' invariants (`cswap-tui.md` §12).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use cswitch::cli::tui::TuiStart;
use cswitch::model::{NormalizedUsage, ScopedWindow, WindowUsage, format_iso};
use cswitch::store::AutoSwitchSettings;
use cswitch::store::usage_store::{UsageEntry, UsageSentinel};
use cswitch::tui::app::{Action, App, Command, ScreenKind};
use cswitch::tui::snapshot::{AccountSnapshot, AccountsSnapshot};
use cswitch::tui::theme::{DARK, ThemeName};

/// 2026-09-21T14:13:20Z.
const NOW: f64 = 1_790_000_000.0;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn window(pct: f64, reset_in: i64) -> WindowUsage {
    WindowUsage {
        pct,
        resets_at: Some(format_iso(NOW as i64 + reset_in)),
    }
}

fn entry(age: f64, usage: Option<NormalizedUsage>) -> UsageEntry {
    UsageEntry {
        sentinel: None,
        last_good: usage,
        fetched_at: Some(NOW - age),
        age_s: Some(age),
        last_attempt_at: None,
        consecutive_failures: 0,
        last_error: None,
        backoff_until: None,
        next_poll_at: None,
        poll_interval_s: None,
        last_429_at: None,
        auth_dead_strikes: 0,
        struck_fingerprint: None,
        trust_extended: false,
    }
}

fn account(number: u32, email: &str, tag: &str, usage: UsageEntry) -> AccountSnapshot {
    AccountSnapshot {
        number,
        email: email.to_string(),
        tag: tag.to_string(),
        alias: None,
        disabled: false,
        api_key: false,
        is_active: false,
        usage,
    }
}

/// Five accounts covering every branch: bars, pace, a maxed pool, a stale
/// active card, a disabled mini, an API key and a token-expired sentinel.
fn fixture() -> AccountsSnapshot {
    let alice = account(
        1,
        "alice@corp.io",
        "Acme",
        entry(
            10.0,
            Some(NormalizedUsage {
                five_hour: Some(window(12.0, 4 * 3600 + 2 * 60)),
                seven_day: Some(window(40.0, 4 * 86_400 + 21 * 3600)),
                ..NormalizedUsage::default()
            }),
        ),
    );
    let mut john = account(
        2,
        "john.doe@gmail.com",
        "Personal",
        entry(
            400.0,
            Some(NormalizedUsage {
                five_hour: Some(window(76.0, 2 * 3600 + 47 * 60)),
                seven_day: Some(window(80.0, 4 * 86_400)),
                scoped: vec![ScopedWindow {
                    name: "Fable".into(),
                    pct: 100.0,
                    resets_at: Some(format_iso(NOW as i64 + 5 * 86_400 + 3 * 3600)),
                }],
                ..NormalizedUsage::default()
            }),
        ),
    );
    john.is_active = true;
    let mut work = account(
        3,
        "john.doe@company.com",
        "Work",
        entry(
            10.0,
            Some(NormalizedUsage {
                five_hour: Some(window(96.0, 3600)),
                seven_day: Some(window(40.0, 4 * 86_400 + 21 * 3600)),
                ..NormalizedUsage::default()
            }),
        ),
    );
    work.disabled = true;
    let mut key_account = account(
        "4".parse().unwrap(),
        "api-key-4@token.local",
        "personal",
        entry(10.0, None),
    );
    key_account.api_key = true;
    key_account.usage.sentinel = Some(UsageSentinel::ApiKey);
    let mut expired = account(
        5,
        "expired@x.y",
        "personal",
        entry(
            720.0,
            Some(NormalizedUsage {
                five_hour: Some(window(61.0, 3600)),
                ..NormalizedUsage::default()
            }),
        ),
    );
    expired.usage.sentinel = Some(UsageSentinel::TokenExpired);
    AccountsSnapshot {
        active_number: Some(2),
        accounts: vec![alice, john, work, key_account, expired],
        taken_at: NOW,
    }
}

fn app_with(start: TuiStart) -> App {
    let mut app = App::new(start, ThemeName::Dark, 90.0, None);
    app.apply_snapshot(fixture(), 1, NOW);
    app
}

fn render(app: &mut App, width: u16, height: u16, now: f64) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame, now)).unwrap();
    terminal.backend().buffer().clone()
}

fn screen_rows(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn find_row<'a>(rows: &'a [String], needle: &str) -> (usize, &'a str) {
    rows.iter()
        .enumerate()
        .find(|(_, r)| r.contains(needle))
        .map(|(i, r)| (i, r.as_str()))
        .unwrap_or_else(|| panic!("no row contains {needle:?}:\n{}", rows.join("\n")))
}

/// Column of the first `needle` character in the row (in cells, not bytes).
fn col(row: &str, needle: char) -> u16 {
    row.chars().position(|c| c == needle).unwrap() as u16
}

fn fg(buf: &Buffer, x: u16, y: usize) -> Color {
    buf[(x, y as u16)].fg
}

#[test]
fn dashboard_active_card_minis_menu_and_footer() {
    let mut app = app_with(TuiStart::Dashboard);
    let buf = render(&mut app, 100, 30, NOW);
    let rows = screen_rows(&buf);

    let (header_y, header) = find_row(&rows, "john.doe@gmail.com");
    assert_eq!(
        header,
        "    2  john.doe@gmail.com  [Personal]   ● active   · 6m ago"
    );
    let dot = col(header, '●');
    assert_eq!(fg(&buf, dot, header_y), DARK.accent);

    let (five_y, five) = find_row(&rows, "5h    ");
    assert!(
        five.starts_with("       5h    ━━━━━━━━━━━━━━━━━━━━━━╸────┃──  76%  resets 2h 47m · "),
        "{five}"
    );
    let tick = col(five, '┃');
    assert_eq!(tick, 7 + 6 + 27, "tick at round(0.9 * 30) inside the bar");
    assert_eq!(fg(&buf, tick, five_y), DARK.warn);
    let first_fill = col(five, '━');
    assert_eq!(fg(&buf, first_fill, five_y), DARK.warn, "76% is amber");
    assert!(
        buf[(first_fill, five_y as u16)]
            .modifier
            .contains(Modifier::DIM),
        "dim at age > 300 s"
    );
    let track = col(five, '─');
    assert_eq!(fg(&buf, track, five_y), DARK.track);

    let (_, seven) = find_row(&rows, "7d    ");
    assert!(seven.contains("  80%  resets 4d"), "{seven}");
    assert!(seven.ends_with("(ahead of pace)"), "{seven}");
    let (fable_y, fable) = find_row(&rows, "Fable ");
    assert!(fable.contains("┃━━ 100%  resets 5d 3h · "), "{fable}");
    assert!(fable.ends_with("  (!)"), "{fable}");
    let fable_fill = col(fable, '━');
    assert_eq!(fg(&buf, fable_fill, fable_y), DARK.crit);

    assert_eq!(rows[header_y - 1], "", "blank line before the active card");
    assert_eq!(rows[fable_y + 1], "", "blank line after the active card");
    let (alice_y, alice) = find_row(&rows, "alice@corp.io");
    assert_eq!(alice, "    1  alice@corp.io  [Acme]   5h 12% · 7d 40%");
    assert_eq!(alice_y, header_y - 2);
    let pct = alice.find("12%").unwrap() as u16;
    assert_eq!(fg(&buf, pct, alice_y), DARK.ok);
    let (work_y, work) = find_row(&rows, "john.doe@company.com");
    assert_eq!(
        work,
        "    3  john.doe@company.com  [Work]  (disabled)   5h 96% · 7d 40%"
    );
    assert_eq!(work_y, fable_y + 2);
    let ninety_six = work.find("96%").unwrap() as u16;
    assert_eq!(fg(&buf, ninety_six, work_y), DARK.crit);
    let (_, key_row) = find_row(&rows, "api-key-4@token.local");
    assert!(key_row.ends_with("   API key (no quota)"), "{key_row}");
    let (_, expired_row) = find_row(&rows, "expired@x.y");
    assert!(
        expired_row
            .ends_with("   token expired — refresh deferred this pass; retries automatically"),
        "{expired_row}"
    );

    let (menu_y, _) = find_row(&rows, "   menu");
    assert_eq!(rows[menu_y], "   menu");
    assert_eq!(rows[menu_y + 2], " ▌ Switch account…");
    assert_eq!(rows[menu_y + 3], "   Watch accounts");
    assert_eq!(rows[menu_y + 9], "   Quit");
    assert_eq!(
        rows[29],
        " s  Switch accounts   w  Watch   q  Quit   ^t  Theme"
    );
    assert!(
        rows.iter().all(|r| !r.contains("loading")),
        "a snapshot replaces the loading placeholder"
    );
}

#[test]
fn dashboard_menu_navigation_and_breadcrumb() {
    let mut app = app_with(TuiStart::Dashboard);
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Char('j')), NOW);
    }
    assert!(app.handle_key(key(KeyCode::Enter), NOW).is_empty());
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    let (y, _) = find_row(&rows, "menu › add account");
    assert_eq!(rows[y], "   menu › add account");
    assert_eq!(rows[y + 2], " ▌ From current Codex login");
    assert_eq!(rows[y + 3], "   From an API key…");
    assert_eq!(rows[y + 4], "   ← back");
    app.handle_key(key(KeyCode::Esc), NOW);
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    let (y, _) = find_row(&rows, "   menu");
    assert_eq!(rows[y], "   menu");
    assert_eq!(
        rows[y + 2],
        " ▌ Switch account…",
        "cursor resets after a pop"
    );
    assert_eq!(app.screen_kind(), ScreenKind::Dashboard);
}

#[test]
fn dashboard_loading_and_empty_states() {
    let mut app = App::new(TuiStart::Dashboard, ThemeName::Dark, 90.0, None);
    let rows_loading = screen_rows(&render(&mut app, 100, 20, NOW));
    assert_eq!(rows_loading[1], "   loading…");
    app.apply_snapshot(AccountsSnapshot::empty(NOW), 1, NOW);
    let rows_empty = screen_rows(&render(&mut app, 100, 20, NOW));
    assert_eq!(rows_empty[1], "   No managed accounts yet.");
    assert!(rows_empty[2].contains("from your current Codex login, or from an API key"));
}

#[test]
fn switch_screen_cards_cursor_and_pop_on_enter() {
    let mut app = app_with(TuiStart::Dashboard);
    app.handle_key(key(KeyCode::Char('s')), NOW);
    assert_eq!(app.screen_kind(), ScreenKind::Switch);
    let buf = render(&mut app, 100, 40, NOW);
    let rows = screen_rows(&buf);
    assert_eq!(rows[1], "   switch to which account?");
    let (active_y, active) = find_row(&rows, "john.doe@gmail.com");
    assert!(
        active.starts_with(" ▌  2  john.doe@gmail.com  [Personal]   ● active"),
        "{active}"
    );
    assert_eq!(
        fg(&buf, 1, active_y),
        DARK.accent,
        "cursor starts on the active account"
    );
    assert!(
        rows.iter().all(|r| !r.contains('┃')),
        "list cards never draw the threshold tick"
    );
    let (_, alice) = find_row(&rows, "alice@corp.io");
    assert!(alice.starts_with("    1  alice@corp.io  [Acme]"), "{alice}");
    let (_, expired) = find_row(&rows, "⚠ token expired");
    assert_eq!(
        expired.trim_start(),
        "⚠ token expired — refresh deferred this pass; retries automatically"
    );
    let (_, last_seen) = find_row(&rows, "└ last seen");
    assert_eq!(last_seen.trim_start(), "└ last seen 61% used · 12m ago");
    let (_, key_row) = find_row(&rows, "· API key (no quota)");
    assert!(
        !rows
            .iter()
            .any(|r| r.contains("last seen") && r != last_seen),
        "{key_row}"
    );
    assert_eq!(
        rows[39],
        " enter  Switch   b  Best pick   esc  Back   ^t  Theme"
    );

    app.handle_key(key(KeyCode::Char('j')), NOW);
    let same = fixture();
    app.apply_snapshot(same, 2, NOW + 3.0);
    let rows = screen_rows(&render(&mut app, 100, 40, NOW + 3.0));
    let (_, work) = find_row(&rows, "john.doe@company.com");
    assert!(
        work.starts_with(" ▌  3  "),
        "cursor survives an unchanged snapshot: {work}"
    );
    let commands = app.handle_key(key(KeyCode::Enter), NOW + 3.0);
    assert_eq!(commands, vec![Command::Action(Action::SwitchTo(3))]);
    assert_eq!(
        app.screen_kind(),
        ScreenKind::Dashboard,
        "Switch pops immediately"
    );
}

#[test]
fn watch_screen_title_status_arming_and_stay() {
    let mut app = app_with(TuiStart::Watch);
    assert_eq!(app.screen_kind(), ScreenKind::Watch);
    let rows_fresh = screen_rows(&render(&mut app, 100, 40, NOW));
    assert_eq!(rows_fresh[1], "   watching all accounts");
    assert_eq!(rows_fresh[39], " s  Switch   esc  Back   ^t  Theme");
    assert!(
        rows_fresh.iter().all(|r| !r.starts_with(" ▌")),
        "no cursor in monitor mode"
    );
    app.set_refreshing_since(Some(NOW + 85.0));
    let rows_stale = screen_rows(&render(&mut app, 100, 40, NOW + 90.0));
    assert_eq!(
        rows_stale[1],
        "   watching all accounts · snapshot 1m ago · refreshing 5s"
    );
    app.set_refreshing_since(None);
    app.handle_key(key(KeyCode::Char('s')), NOW);
    let rows_armed = screen_rows(&render(&mut app, 100, 40, NOW));
    assert_eq!(
        rows_armed[1],
        "   switch to which account? · enter confirm · esc cancel"
    );
    assert_eq!(
        rows_armed[39],
        " s  Switch   enter  Confirm   esc  Back   ^t  Theme"
    );
    let (_, active) = find_row(&rows_armed, "john.doe@gmail.com");
    assert!(active.starts_with(" ▌  2  "), "{active}");
    let commands = app.handle_key(key(KeyCode::Enter), NOW);
    assert_eq!(commands, vec![Command::Action(Action::SwitchTo(2))]);
    assert_eq!(
        app.screen_kind(),
        ScreenKind::Watch,
        "Watch stays after a switch"
    );
    assert!(!app.watch().unwrap().armed());
    app.handle_key(key(KeyCode::Esc), NOW);
    assert_eq!(
        app.screen_kind(),
        ScreenKind::Dashboard,
        "Esc from watch lands on the dashboard"
    );
}

#[test]
fn auto_screen_badge_summary_candidates_log_and_threshold() {
    let mut app = app_with(TuiStart::Dashboard);
    assert_eq!(
        app.handle_key(key(KeyCode::Char('g')), NOW),
        vec![Command::OpenAuto]
    );
    let settings = AutoSwitchSettings::default();
    let commands = app.open_auto(settings.clone(), NOW);
    assert_eq!(
        commands[0],
        Command::StartEngine {
            settings: settings.clone(),
            dry_run: true
        },
        "opening never switches: dry-run first"
    );
    let buf = render(&mut app, 100, 30, NOW);
    let rows = screen_rows(&buf);
    let (_, header) = find_row(&rows, "john.doe@gmail.com");
    assert!(
        header.starts_with("    2  john.doe@gmail.com  [Personal]   ● active"),
        "{header}"
    );
    assert!(
        rows.iter().all(|r| !r.contains("alice@corp.io  [Acme]")),
        "no minis on the auto panel"
    );
    let (five_y, five) = find_row(&rows, "5h    ");
    assert!(
        five.contains('┃'),
        "the active card keeps the threshold tick: {five}"
    );
    assert_eq!(fg(&buf, col(five, '┃'), five_y), DARK.warn);
    let (badge_y, badge_row) = find_row(&rows, "DRY-RUN");
    assert_eq!(
        badge_row,
        "   DRY-RUN    auto-switch · threshold 90% · poll every 60s"
    );
    let badge_x = badge_row.find("DRY-RUN").unwrap() as u16;
    assert_eq!(buf[(badge_x, badge_y as u16)].bg, DARK.panel);
    assert_eq!(buf[(badge_x, badge_y as u16)].fg, DARK.warn);
    let (next_y, _) = find_row(&rows, "Next best");
    assert_eq!(rows[next_y + 1], "     1  alice@corp.io   40% used");
    assert_eq!(
        rows[next_y + 2],
        "     4  api-key-4@token.local  API key (no quota)"
    );
    assert_eq!(
        rows[next_y + 3],
        "     5  expired@x.y  token expired — refresh deferred this pass; retries automatically"
    );
    assert!(
        rows.iter().all(|r| !r.contains("john.doe@company.com")),
        "disabled accounts are not candidates"
    );
    let (_, started) = find_row(&rows, "engine started");
    assert!(
        started.ends_with("— engine started: DRY-RUN (watching only) —"),
        "{started}"
    );
    assert_eq!(
        rows[29],
        " l  Go live / dry-run   t  Threshold   esc  Back   ^t  Theme"
    );

    app.handle_key(key(KeyCode::Char('t')), NOW);
    app.handle_key(key(KeyCode::Right), NOW);
    let buf = render(&mut app, 100, 30, NOW);
    let rows = screen_rows(&buf);
    let (_, summary) = find_row(&rows, "DRY-RUN");
    assert_eq!(
        summary,
        "   DRY-RUN    auto-switch · threshold 91% (session) · poll every 60s   ← → adjust · enter done"
    );
    let (five_y, five) = find_row(&rows, "5h    ");
    assert_eq!(col(five, '┃'), 7 + 6 + 27, "91% still rounds to cell 27");
    assert_eq!(fg(&buf, col(five, '┃'), five_y), DARK.warn);
    assert_eq!(
        rows[29],
        " l  Go live / dry-run   t  Threshold   ←  -1%   →  +1%   enter  Done   esc  Back   ^t  Theme"
    );
    for _ in 0..9 {
        app.handle_key(key(KeyCode::Right), NOW);
    }
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    let (_, five) = find_row(&rows, "5h    ");
    assert_eq!(
        col(five, '┃'),
        7 + 6 + 29,
        "99.9% pins the tick to the last cell"
    );
    let commands = app.handle_key(key(KeyCode::Enter), NOW);
    assert!(
        matches!(&commands[0], Command::StartEngine { settings, dry_run: true } if settings.threshold == 99.9),
        "{commands:?}"
    );
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    assert!(
        rows.iter()
            .any(|r| r.ends_with("— threshold set to 99.9% for this session —"))
    );

    app.handle_key(key(KeyCode::Char('l')), NOW);
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    let (title_y, _) = find_row(&rows, "Go live");
    assert!(rows[title_y + 2].contains("Go live? cswitch will switch your active account"));
    assert!(
        rows.iter()
            .any(|r| r.contains("(Same behavior as running `cswitch auto` in a terminal.)"))
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("Go live") && r.contains("Cancel"))
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("← → · enter  ·  y go live  ·  n / esc cancel"))
    );
    app.handle_key(key(KeyCode::Char('n')), NOW);
    assert!(app.auto().unwrap().dry_run(), "declining keeps dry-run");
    let commands = app.handle_key(key(KeyCode::Char('l')), NOW);
    assert!(commands.is_empty());
    let commands = app.handle_key(key(KeyCode::Char('y')), NOW);
    assert!(matches!(
        commands[0],
        Command::StartEngine { dry_run: false, .. }
    ));
    let buf = render(&mut app, 100, 30, NOW);
    let rows = screen_rows(&buf);
    let (live_y, live_row) = find_row(&rows, " LIVE ");
    let live_x = live_row.find("LIVE").unwrap() as u16;
    assert_eq!(buf[(live_x, live_y as u16)].bg, DARK.accent);
    assert!(
        rows.iter()
            .any(|r| r.ends_with("— engine started: LIVE (will switch accounts) —"))
    );

    let commands = app.handle_key(key(KeyCode::Esc), NOW);
    assert_eq!(
        commands,
        vec![Command::StopEngine, Command::Refresh { full: false }]
    );
    assert_eq!(
        app.threshold_pct(),
        Some(90.0),
        "leaving restores the file threshold"
    );
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    let (_, five) = find_row(&rows, "5h    ");
    assert_eq!(col(five, '┃'), 7 + 6 + 27);
}

#[test]
fn remove_confirm_modal_and_toasts_render() {
    let mut app = app_with(TuiStart::Dashboard);
    for _ in 0..5 {
        app.handle_key(key(KeyCode::Char('j')), NOW);
    }
    app.handle_key(key(KeyCode::Enter), NOW);
    app.handle_key(key(KeyCode::Enter), NOW);
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    let (y, _) = find_row(&rows, "Remove account 1 (alice@corp.io)?");
    assert!(
        rows[y - 2].contains("Remove account"),
        "title above the message"
    );
    assert!(rows[y + 2].contains("Its stored credentials are deleted."));
    assert!(rows[y + 4].contains("Remove") && rows[y + 4].contains("Cancel"));
    assert!(rows[y + 5].contains("← → · enter  ·  y remove  ·  n / esc cancel"));
    let commands = app.handle_key(key(KeyCode::Char('y')), NOW);
    assert_eq!(commands, vec![Command::Action(Action::Remove(1))]);
    assert!(app.modal().is_none());
    assert_eq!(app.dashboard().breadcrumb(), "menu › remove account");

    let commands = app.handle_key(key(KeyCode::Char('f')), NOW);
    assert_eq!(commands, vec![Command::Refresh { full: true }]);
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    assert!(
        rows.iter().any(|r| r.contains("Refreshing usage…")),
        "{rows:?}"
    );
    app.tick(NOW + 3.0);
    let rows = screen_rows(&render(&mut app, 100, 30, NOW + 3.0));
    assert!(
        rows.iter().all(|r| !r.contains("Refreshing usage…")),
        "2 s toast expired"
    );
}

#[test]
fn light_theme_changes_the_palette() {
    let mut app = App::new(TuiStart::Dashboard, ThemeName::Light, 90.0, None);
    app.apply_snapshot(fixture(), 1, NOW);
    let buf = render(&mut app, 100, 30, NOW);
    assert_eq!(buf[(0, 0)].bg, cswitch::tui::theme::LIGHT.bg);
    let rows = screen_rows(&buf);
    let (y, header) = find_row(&rows, "john.doe@gmail.com");
    assert_eq!(
        fg(&buf, col(header, '●'), y),
        cswitch::tui::theme::LIGHT.accent
    );
    let ctrl_t = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert_eq!(
        app.handle_key(ctrl_t, NOW),
        vec![Command::PersistTheme(ThemeName::Auto)]
    );
    let buf = render(&mut app, 100, 30, NOW);
    assert_eq!(buf[(0, 0)].bg, DARK.bg, "auto without COLORFGBG is dark");
}
