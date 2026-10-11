//! Renders the dashboard, switch, watch and auto screens through ratatui's
//! `TestBackend` from a hand-built snapshot: no terminal, no network, no
//! `Switcher`. Asserts the research notes' invariants (`cswap-tui.md` §12).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui_image::picker::{Picker, ProtocolType};

use ccsw::cli::tui::TuiStart;
use ccsw::model::{NormalizedUsage, ScopedWindow, Spend, WindowUsage, format_iso};
use ccsw::provider::Provider;
use ccsw::store::AutoSwitchSettings;
use ccsw::store::usage_store::{UsageEntry, UsageSentinel};
use ccsw::tui::app::{Action, ActionResult, App, Command, Inbound, ScreenKind};
use ccsw::tui::console::{self, PairingLink};
use ccsw::tui::snapshot::{AccountSnapshot, AccountsSnapshot};
use ccsw::tui::theme::{DARK, ThemeName};

/// 2026-09-21T14:13:20Z.
const NOW: f64 = 1_790_000_000.0;

#[test]
fn console_actions_click_pairing_expiry_and_small_viewports() {
    let mut app = app_with(TuiStart::Dashboard);
    app.handle_key(key(KeyCode::End), NOW);
    app.handle_key(key(KeyCode::Up), NOW);
    app.handle_key(key(KeyCode::Enter), NOW);
    let rows = screen_rows(&render(&mut app, 110, 28, NOW));
    let (y, _) = find_row(&rows, "Start and open browser");
    assert_eq!(
        app.handle_click(4, y as u16, NOW),
        vec![Command::Console(console::Request::Start {
            options: console::Options::default(),
            open_browser: true
        })]
    );
    let link = PairingLink {
        origin: "http://127.0.0.1:12345".into(),
        code: "a".repeat(64),
        expires_at: NOW + 300.0,
    };
    app.receive(Inbound::Console(console::Event::Link(link.clone())), NOW);
    let rows = screen_rows(&render(&mut app, 110, 28, NOW));
    find_row(&rows, &link.url());
    find_row(&rows, "Single use · expires in 5:00 if unused");
    let rows = screen_rows(&render(&mut app, 110, 28, NOW + 301.0));
    let (y, line) = find_row(&rows, "Link expired.");
    let x = line.find('L').unwrap() as u16;
    assert_eq!(fg(&render(&mut app, 110, 28, NOW + 301.0), x, y), DARK.crit);
    for (width, height) in [(40, 14), (20, 8), (8, 4), (1, 1)] {
        render(&mut app, width, height, NOW);
        app.handle_key(key(KeyCode::PageDown), NOW);
        render(&mut app, width, height, NOW);
    }
    // Mouse clicks only activate action rows, never the text of a pairing code.
    render(&mut app, 110, 28, NOW);
    assert!(app.handle_click(4, 10, NOW).is_empty());
}

fn paired_console(theme: ThemeName, origin: &str) -> (App, PairingLink) {
    let mut app = App::new(TuiStart::Dashboard, theme, 90.0, None);
    app.apply_snapshot(fixture(), 1, NOW);
    for code in [KeyCode::End, KeyCode::Up, KeyCode::Enter] {
        app.handle_key(key(code), NOW);
    }
    if !origin.contains("127.0.0.1") {
        for code in [KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
            app.handle_key(key(code), NOW);
        }
    }
    app.handle_key(key(KeyCode::Char('o')), NOW);
    let link = PairingLink {
        origin: origin.into(),
        code: "0123456789abcdef".repeat(4),
        expires_at: NOW + 300.0,
    };
    app.receive(Inbound::Console(console::Event::Link(link.clone())), NOW);
    (app, link)
}

fn decode_console_qr(buf: &Buffer) -> Vec<Vec<u8>> {
    // Rasterize terminal half-blocks to verify the displayed code with an independent decoder.
    let scale = 4;
    let width = buf.area.width as usize * scale;
    let height = buf.area.height as usize * 2 * scale;
    let mut pixels = vec![255; width * height];
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let background = match cell.bg {
                Color::Rgb(r, g, b) => ((r as u16 + g as u16 + b as u16) / 3) as u8,
                _ => 255,
            };
            let halves = if cell.fg == Color::Rgb(0, 0, 0) && cell.bg == Color::Rgb(255, 255, 255) {
                match cell.symbol() {
                    "█" => [0, 0],
                    "▀" => [0, 255],
                    "▄" => [255, 0],
                    " " => [255, 255],
                    other => panic!("unexpected QR cell: {other:?}"),
                }
            } else {
                [background; 2]
            };
            for (half, shade) in halves.into_iter().enumerate() {
                for dy in 0..scale {
                    let row = (y as usize * 2 + half) * scale + dy;
                    let start = row * width + x as usize * scale;
                    pixels[start..start + scale].fill(shade);
                }
            }
        }
    }
    quircs::Quirc::default()
        .identify(width, height, &pixels)
        .map(|code| code.unwrap().decode().unwrap().payload)
        .collect()
}

#[test]
fn console_qr_pairs_with_current_link_in_both_themes() {
    for theme in [ThemeName::Dark, ThemeName::Light] {
        for origin in ["http://127.0.0.1:12345", "http://100.101.102.103:12345"] {
            let (mut app, mut link) = paired_console(theme, origin);
            let buf = render(&mut app, 110, 60, NOW);
            assert_eq!(decode_console_qr(&buf), vec![link.url().into_bytes()]);
            let rows = screen_rows(&buf);
            let (code_y, _) = find_row(&rows, "Pairing code:");
            let qr_y = (0..buf.area.height)
                .find(|&y| buf[(2, y)].bg == Color::Rgb(255, 255, 255))
                .expect("QR code background");
            assert!(qr_y as usize > code_y);
            if origin.contains("127.0.0.1") {
                find_row(&rows, "Access: Tailscale");
            }

            app.handle_key(key(KeyCode::Char('n')), NOW);
            link.code = "fedcba9876543210".repeat(4);
            app.receive(Inbound::Console(console::Event::Link(link.clone())), NOW);
            assert_eq!(
                decode_console_qr(&render(&mut app, 110, 60, NOW)),
                vec![link.url().into_bytes()]
            );
            assert!(decode_console_qr(&render(&mut app, 110, 60, NOW + 301.0)).is_empty());

            app.handle_key(key(KeyCode::Char('s')), NOW);
            assert!(decode_console_qr(&render(&mut app, 110, 60, NOW)).is_empty());
        }
    }
}

#[test]
fn console_address_stays_visible_while_pairing_details_scroll() {
    for (width, height) in [(80, 36), (40, 24)] {
        let (mut app, link) = paired_console(ThemeName::Dark, "http://100.101.102.103:12345");
        let address = format!("{}/", link.origin);
        let initial = render(&mut app, width, height, NOW);
        let rows = screen_rows(&initial);
        find_row(&rows, "Tailscale URL:");
        let (address_y, _) = find_row(&rows, &address);
        for _ in 0..12 {
            app.handle_key(key(KeyCode::PageDown), NOW);
        }
        let scrolled = render(&mut app, width, height, NOW);
        assert_eq!(find_row(&screen_rows(&scrolled), &address).0, address_y);
    }
}

fn iterm_qr(buf: &Buffer) -> Option<(u16, u16, image::GrayImage)> {
    use base64::Engine;
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let symbol = buf[(x, y)].symbol();
            if let Some((_, encoded)) = symbol.split_once("doNotMoveCursor=1:") {
                let encoded = encoded.split('\x07').next().unwrap();
                let png = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap();
                return Some((x, y, image::load_from_memory(&png).unwrap().into_luma8()));
            }
        }
    }
    None
}

#[test]
fn console_image_qr_is_compact_and_contains_the_current_pairing_link() {
    for theme in [ThemeName::Dark, ThemeName::Light] {
        let (mut app, mut link) = paired_console(theme, "http://100.101.102.103:12345");
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Iterm2);
        app.set_image_picker(Some(picker));
        let buf = render(&mut app, 80, 40, NOW);
        let (_, y, pixels) = iterm_qr(&buf).expect("inline PNG");
        // A 49 x 25 character QR becomes a 25 x 13 cell image at 10 x 20 pixels per cell.
        assert_eq!(pixels.dimensions(), (250, 260));
        assert!(y as usize > find_row(&screen_rows(&buf), "Pairing code:").0);
        let decode = |pixels: &image::GrayImage| {
            quircs::Quirc::default()
                .identify(
                    pixels.width() as usize,
                    pixels.height() as usize,
                    pixels.as_raw(),
                )
                .map(|code| code.unwrap().decode().unwrap().payload)
                .collect::<Vec<_>>()
        };
        assert_eq!(decode(&pixels), vec![link.url().into_bytes()]);
        app.handle_key(key(KeyCode::Char('n')), NOW);
        link.code = "fedcba9876543210".repeat(4);
        app.receive(Inbound::Console(console::Event::Link(link.clone())), NOW);
        let (_, _, pixels) = iterm_qr(&render(&mut app, 80, 40, NOW)).unwrap();
        assert_eq!(decode(&pixels), vec![link.url().into_bytes()]);
        assert!(iterm_qr(&render(&mut app, 80, 40, NOW + 301.0)).is_none());
        app.handle_key(key(KeyCode::Char('s')), NOW);
        assert!(iterm_qr(&render(&mut app, 80, 40, NOW)).is_none());
    }
}

#[test]
fn console_image_qr_scrolls_without_clipping_and_falls_back_to_text() {
    let (mut app, link) = paired_console(ThemeName::Dark, "http://100.101.102.103:12345");
    let mut picker = Picker::halfblocks();
    picker.set_protocol_type(ProtocolType::Iterm2);
    app.set_image_picker(Some(picker));
    assert!(iterm_qr(&render(&mut app, 40, 28, NOW)).is_none());
    for _ in 0..12 {
        app.handle_key(key(KeyCode::PageDown), NOW);
    }
    let buf = render(&mut app, 40, 28, NOW);
    let (_, y, _) = iterm_qr(&buf).expect("scroll reveals the full image");
    assert!(y + 13 <= 27, "image must fit above the footer");
    find_row(&screen_rows(&buf), "Tailscale URL:");
    for (width, height) in [(28, 40), (40, 20), (1, 1)] {
        assert!(iterm_qr(&render(&mut app, width, height, NOW)).is_none());
    }
    app.set_image_picker(Some(Picker::halfblocks()));
    assert_eq!(
        decode_console_qr(&render(&mut app, 110, 60, NOW)),
        vec![link.url().into_bytes()]
    );
}

#[test]
fn console_graphics_clear_every_covered_cell_on_navigation_expiry_and_stop() {
    use ratatui::layout::Rect;
    for protocol in [
        ProtocolType::Iterm2,
        ProtocolType::Kitty,
        ProtocolType::Sixel,
    ] {
        for action in [KeyCode::Esc, KeyCode::Char('s'), KeyCode::Null] {
            let (mut app, _) = paired_console(ThemeName::Dark, "http://100.101.102.103:12345");
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(protocol);
            app.set_image_picker(Some(picker));
            let area = Rect::new(0, 0, 80, 40);
            let mut before = Buffer::empty(area);
            app.draw(area, &mut before, NOW);
            let top = (0..area.height)
                .find(|&y| before[(2, y)].symbol().contains('\x1b'))
                .expect("image protocol data");
            app.handle_key(key(action), NOW);
            let mut after = Buffer::empty(area);
            app.draw(
                area,
                &mut after,
                if action == KeyCode::Null {
                    NOW + 301.0
                } else {
                    NOW
                },
            );
            assert!(
                after
                    .content
                    .iter()
                    .all(|cell| !cell.symbol().contains('\x1b'))
            );
            let changed: Vec<_> = before
                .diff(&after)
                .into_iter()
                .map(|(x, y, _)| (x, y))
                .collect();
            for y in top..top + 13 {
                for x in 2..27 {
                    assert!(
                        changed.contains(&(x, y)),
                        "{protocol:?} {action:?}: stale image at {x},{y}"
                    );
                }
            }
        }
    }
}

#[test]
fn console_qr_survives_scrolling_and_resizing_without_wrapping() {
    let (mut app, link) = paired_console(ThemeName::Dark, "http://100.101.102.103:12345");
    for (width, height) in [(40, 60), (110, 24)] {
        let buf = render(&mut app, width, height, NOW);
        find_row(&screen_rows(&buf), "Enlarge terminal");
        assert!(decode_console_qr(&buf).is_empty());
    }

    for (width, height) in [(80, 36), (53, 37)] {
        render(&mut app, width, height, NOW);
        for _ in 0..12 {
            app.handle_key(key(KeyCode::PageDown), NOW);
        }
        let buf = render(&mut app, width, height, NOW);
        assert_eq!(
            decode_console_qr(&buf),
            vec![link.url().into_bytes()],
            "{width}x{height}:\n{}",
            screen_rows(&buf).join("\n")
        );
    }
    assert!(app.handle_click(4, 12, NOW).is_empty());
}

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
        provider: ccsw::provider::Provider::Codex,
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
                reset_credits: Some(1),
                spend: None,
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
                reset_credits: Some(2),
                spend: None,
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

fn cell(buf: &Buffer, x: u16, y: usize) -> String {
    buf[(x, y as u16)].symbol().to_string()
}

#[test]
fn dashboard_active_card_minis_menu_and_footer() {
    let mut app = app_with(TuiStart::Dashboard);
    let buf = render(&mut app, 100, 30, NOW);
    let rows = screen_rows(&buf);

    let (header_y, header) = find_row(&rows, "john.doe@gmail.com");
    assert_eq!(
        header,
        "    2  john.doe@gmail.com  [Personal]   ● active   · 6m ago   ♥ 2"
    );
    let dot = col(header, '●');
    assert_eq!(fg(&buf, dot, header_y), DARK.accent);
    let heart = col(header, '♥');
    assert_eq!(
        fg(&buf, heart, header_y),
        DARK.crit,
        "the reset-card heart is red"
    );
    assert_eq!(
        fg(&buf, heart + 2, header_y),
        DARK.ok,
        "the reset-card count is green"
    );
    assert!(
        buf[(heart + 2, header_y as u16)]
            .modifier
            .contains(Modifier::BOLD),
        "the reset-card count is bold"
    );

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
    assert_eq!(
        alice,
        "    1  alice@corp.io  [Acme]   5h 12% · 7d 40% · ♥ 1"
    );
    assert_eq!(alice_y, header_y - 2);
    let pct = alice.find("12%").unwrap() as u16;
    assert_eq!(fg(&buf, pct, alice_y), DARK.ok);
    let (work_y, work) = find_row(&rows, "john.doe@company.com");
    assert_eq!(
        work, "    3  john.doe@company.com  [Work]  (disabled)   5h 96% · 7d 40%",
        "no icon when the count is unknown"
    );
    assert_eq!(work_y, fable_y + 2);
    let ninety_six = work.find("96%").unwrap() as u16;
    assert_eq!(fg(&buf, ninety_six, work_y), DARK.crit);
    let (_, key_row) = find_row(&rows, "api-key-4@token.local");
    assert!(key_row.ends_with("   API key (no quota)"), "{key_row}");
    let (expired_y, expired_row) = find_row(&rows, "expired@x.y");
    assert!(
        expired_row.ends_with("   token expired — refresh deferred this pass; retries"),
        "{expired_row}"
    );
    assert_eq!(
        rows[expired_y + 1],
        "   automatically",
        "the 96-column note wraps at the 92-column card width"
    );

    let (menu_y, _) = find_row(&rows, "   menu");
    assert_eq!(rows[menu_y], "   menu");
    assert_eq!(rows[menu_y + 2], " ▌ Switch account…");
    assert_eq!(rows[menu_y + 3], "   Watch accounts");
    assert_eq!(rows[menu_y + 9], "   Remote Console…");
    assert_eq!(rows[menu_y + 10], "   Quit");
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
fn dashboard_keeps_every_account_and_scrolls_the_menu_when_the_terminal_is_short() {
    let mut app = app_with(TuiStart::Dashboard);
    let rows = screen_rows(&render(&mut app, 110, 20, NOW));
    find_row(&rows, "expired@x.y");
    assert_eq!(rows[14], "   menu");
    assert_eq!(rows[15], "");
    assert_eq!(rows[16], " ▌ Switch account…");
    assert_eq!(rows[17], "   Watch accounts");
    assert_eq!(rows[18], "   Auto-switch view");
    assert_eq!(
        rows[19],
        " s  Switch accounts   w  Watch   q  Quit   ^t  Theme"
    );
    assert!(!rows.iter().any(|r| r == "   Quit"), "{}", rows.join("\n"));

    app.handle_key(key(KeyCode::End), NOW);
    let rows = screen_rows(&render(&mut app, 110, 20, NOW));
    find_row(&rows, "expired@x.y");
    assert_eq!(rows[16], "   Theme…");
    assert_eq!(rows[17], "   Remote Console…");
    assert_eq!(rows[18], " ▌ Quit");
}

#[test]
fn dashboard_menu_never_shrinks_below_the_breadcrumb_and_cursor_row() {
    let mut app = app_with(TuiStart::Dashboard);
    let rows = screen_rows(&render(&mut app, 110, 16, NOW));
    assert_eq!(rows[13], "   menu");
    assert_eq!(rows[14], " ▌ Switch account…");
    assert_eq!(
        rows[15],
        " s  Switch accounts   w  Watch   q  Quit   ^t  Theme"
    );
    find_row(&rows, "api-key-4@token.local");
    assert!(
        !rows.iter().any(|r| r.contains("expired@x.y")),
        "the panel gives up its last row instead:\n{}",
        rows.join("\n")
    );
}

#[test]
fn dashboard_wraps_long_rows_on_a_narrow_terminal() {
    let mut app = app_with(TuiStart::Dashboard);
    let rows = screen_rows(&render(&mut app, 60, 40, NOW));
    let (header_y, header) = find_row(&rows, "john.doe@gmail.com");
    assert_eq!(
        header,
        "    2  john.doe@gmail.com  [Personal]   ● active   · 6m"
    );
    assert_eq!(
        rows[header_y + 1],
        "   ago   ♥ 2",
        "continuation rows keep the panel indent"
    );
    let (expired_y, expired) = find_row(&rows, "expired@x.y");
    assert_eq!(expired, "    5  expired@x.y  [personal]   token expired —");
    assert_eq!(
        rows[expired_y + 1],
        "   refresh deferred this pass; retries automatically"
    );
}

#[test]
fn dashboard_panel_scrolls_with_a_scrollbar_when_it_overflows() {
    let mut app = app_with(TuiStart::Dashboard);
    // 15 body rows: 4 chrome + 2 menu floor leave 9 for the 10-row panel.
    let buf = render(&mut app, 110, 16, NOW);
    let rows = screen_rows(&buf);
    find_row(&rows, "alice@corp.io");
    assert!(
        !rows.iter().any(|r| r.contains("expired@x.y")),
        "{}",
        rows.join("\n")
    );
    assert_eq!(rows[13], "   menu");
    assert_eq!(rows[14], " ▌ Switch account…");
    assert_eq!(cell(&buf, 109, 1), "█", "thumb at the top of the panel");
    assert_eq!(cell(&buf, 109, 9), "│", "track below the thumb");
    assert_eq!(cell(&buf, 109, 0), " ");
    assert_eq!(cell(&buf, 109, 10), " ");

    app.handle_key(key(KeyCode::PageDown), NOW);
    let buf = render(&mut app, 110, 16, NOW);
    let rows = screen_rows(&buf);
    find_row(&rows, "expired@x.y");
    assert!(
        !rows.iter().any(|r| r.contains("alice@corp.io")),
        "{}",
        rows.join("\n")
    );
    assert_eq!(cell(&buf, 109, 1), "│");
    assert_eq!(cell(&buf, 109, 9), "█", "thumb at the bottom");

    app.handle_scroll(-1);
    let rows = screen_rows(&render(&mut app, 110, 16, NOW));
    find_row(&rows, "alice@corp.io");

    let buf = render(&mut app, 110, 30, NOW);
    assert!(
        (0..30).all(|y| !matches!(cell(&buf, 109, y).as_str(), "█" | "│")),
        "no scrollbar when the panel fits"
    );
}

#[test]
fn lists_show_a_scrollbar_and_scroll_freely_until_the_cursor_moves() {
    let mut app = app_with(TuiStart::Watch);
    let buf = render(&mut app, 110, 14, NOW);
    let rows = screen_rows(&buf);
    find_row(&rows, "alice@corp.io");
    assert!(
        !rows.iter().any(|r| r.contains("expired@x.y")),
        "{}",
        rows.join("\n")
    );
    assert_eq!(cell(&buf, 109, 3), "█", "thumb at the top of the card rows");
    assert_eq!(cell(&buf, 109, 2), " ");
    app.handle_scroll(1);
    app.handle_scroll(1);
    let rows = screen_rows(&render(&mut app, 110, 14, NOW));
    assert!(
        !rows.iter().any(|r| r.contains("alice@corp.io")),
        "{}",
        rows.join("\n")
    );

    let mut app = app_with(TuiStart::Dashboard);
    app.handle_key(key(KeyCode::Char('s')), NOW);
    let rows = screen_rows(&render(&mut app, 110, 14, NOW));
    find_row(&rows, "▌  2  john.doe@gmail.com");
    app.handle_scroll(5);
    let rows = screen_rows(&render(&mut app, 110, 14, NOW));
    assert!(
        !rows.iter().any(|r| r.contains("john.doe@gmail.com")),
        "the wheel scrolls the cursor out of view:\n{}",
        rows.join("\n")
    );
    app.handle_key(key(KeyCode::Up), NOW);
    let rows = screen_rows(&render(&mut app, 110, 14, NOW));
    find_row(&rows, "▌  1  alice@corp.io");
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
    assert_eq!(rows[y + 2], " ▌ Add new account");
    assert_eq!(rows[y + 3], "   From current logins");
    assert_eq!(rows[y + 4], "   From a token…");
    assert_eq!(rows[y + 5], "   ← back");
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
    assert!(
        rows_empty[2].contains("from your current Codex or Claude Code login, or from a token")
    );
}

#[test]
fn browser_login_starts_immediately_and_can_be_cancelled() {
    let mut app = app_with(TuiStart::Dashboard);
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Down), NOW);
    }
    app.handle_key(key(KeyCode::Enter), NOW);
    assert_eq!(
        app.handle_key(key(KeyCode::Enter), NOW),
        vec![Command::Action(Action::AddNew)]
    );
    assert!(app.busy());
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    find_row(&rows, "Opening your browser…");
    app.receive(
        Inbound::LoginUrl("https://auth.openai.com/oauth/authorize?state=test".into()),
        NOW,
    );
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    find_row(&rows, "Complete sign-in in your browser.");
    find_row(&rows, "https://auth.openai.com/oauth/authorize?state=test");
    app.receive(
        Inbound::LoginUrl(format!(
            "https://auth.openai.com/oauth/authorize?state={}&end=visible",
            "x".repeat(1200)
        )),
        NOW,
    );
    let rows = screen_rows(&render(&mut app, 80, 20, NOW));
    assert!(rows.iter().all(|row| !row.contains("end=visible")));
    for _ in 0..40 {
        app.handle_key(key(KeyCode::Down), NOW);
        render(&mut app, 80, 20, NOW);
    }
    let rows = screen_rows(&render(&mut app, 80, 20, NOW));
    let visible_url: String = rows
        .iter()
        .filter_map(|row| row.split('│').nth(1))
        .map(str::trim)
        .collect();
    assert!(visible_url.contains("end=visible"));
    assert!(app.handle_key(key(KeyCode::Enter), NOW).is_empty());
    assert_eq!(
        app.handle_key(key(KeyCode::Esc), NOW),
        vec![Command::CancelLogin]
    );
    assert!(app.busy(), "wait for worker cleanup before another login");
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    find_row(&rows, "Cancelling login…");
    assert_eq!(
        app.receive(
            Inbound::ActionDone(ActionResult {
                action: Action::AddNew,
                ok: true,
                lines: vec![ccsw::switcher::Line::plain("Login cancelled.")],
                switch: None,
                followup: None,
            }),
            NOW
        ),
        vec![Command::Refresh { full: false }]
    );
    assert!(!app.busy());
    app.handle_key(key(KeyCode::Esc), NOW);
    assert_eq!(
        app.handle_key(key(KeyCode::Enter), NOW),
        vec![Command::Action(Action::AddNew)]
    );
    assert_eq!(
        app.handle_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            NOW
        ),
        vec![Command::Quit]
    );
    assert!(app.quit_requested());
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
    let mut app = App::new(TuiStart::Dashboard, ThemeName::Dark, 90.0, None);
    app.apply_snapshot(mixed_fixture(), 1, NOW);
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
    let (_, header) = find_row(&rows, "bob@gmail.com");
    assert!(header.starts_with("    6  bob@gmail.com"), "{header}");
    assert!(header.contains("● active"), "{header}");
    assert!(
        rows.iter()
            .all(|r| !r.contains("john.doe@gmail.com") && !r.contains("alice@corp.io")),
        "no Codex account on the auto view"
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
    assert!(
        rows[next_y + 1].starts_with("     7  bob@work.com"),
        "{}",
        rows[next_y + 1]
    );
    assert!(rows[next_y + 1].ends_with("% used"), "{}", rows[next_y + 1]);
    assert!(
        rows.get(next_y + 2).is_none_or(|r| !r.contains("@")),
        "bob@work.com is the only candidate: {:?}",
        rows.get(next_y + 2)
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
    assert!(
        rows[title_y + 2].contains("Go live? ccsw will switch your active Claude Code account")
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("(Same behavior as running `ccsw auto` in a terminal.)"))
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
    assert_eq!(buf[(0, 0)].bg, ccsw::tui::theme::LIGHT.bg);
    let rows = screen_rows(&buf);
    let (y, header) = find_row(&rows, "john.doe@gmail.com");
    assert_eq!(
        fg(&buf, col(header, '●'), y),
        ccsw::tui::theme::LIGHT.accent
    );
    let ctrl_t = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert_eq!(
        app.handle_key(ctrl_t, NOW),
        vec![Command::PersistTheme(ThemeName::Auto)]
    );
    let buf = render(&mut app, 100, 30, NOW);
    assert_eq!(buf[(0, 0)].bg, DARK.bg, "auto without COLORFGBG is dark");
}

fn mixed_fixture() -> AccountsSnapshot {
    let mut snapshot = fixture();
    let mut bob = account(
        6,
        "bob@gmail.com",
        "Personal",
        entry(
            10.0,
            Some(NormalizedUsage {
                five_hour: Some(window(40.0, 70 * 60)),
                seven_day: Some(window(100.0, 2 * 86_400 + 4 * 3600)),
                scoped: vec![ScopedWindow {
                    name: "Fable".into(),
                    pct: 100.0,
                    resets_at: Some(format_iso(NOW as i64 + 2 * 86_400 + 4 * 3600)),
                }],
                spend: Some(Spend {
                    used: 12.5,
                    limit: 50.0,
                    pct: 25.0,
                    currency: "USD".into(),
                    resets_at: None,
                }),
                ..NormalizedUsage::default()
            }),
        ),
    );
    bob.provider = Provider::Claude;
    bob.is_active = true;
    let mut work = account(
        7,
        "bob@work.com",
        "Work",
        entry(
            10.0,
            Some(NormalizedUsage {
                five_hour: Some(window(3.0, 3600)),
                seven_day: Some(window(22.0, 86_400)),
                reset_credits: Some(1),
                ..NormalizedUsage::default()
            }),
        ),
    );
    work.provider = Provider::Claude;
    snapshot.accounts.push(bob);
    snapshot.accounts.push(work);
    snapshot
}

#[test]
fn mixed_roster_shows_a_section_per_provider() {
    let mut app = App::new(TuiStart::Dashboard, ThemeName::Dark, 90.0, None);
    app.apply_snapshot(mixed_fixture(), 1, NOW);
    let buf = render(&mut app, 100, 44, NOW);
    let rows = screen_rows(&buf);
    let (codex_y, codex) = find_row(&rows, "   codex");
    assert_eq!(codex, "   codex");
    assert_eq!(fg(&buf, 3, codex_y), DARK.muted);
    let (alice_y, _) = find_row(&rows, "alice@corp.io");
    assert_eq!(alice_y, codex_y + 1, "the first account follows its header");
    let (claude_y, claude) = find_row(&rows, "   claude");
    assert_eq!(claude, "   claude");
    assert_eq!(rows[claude_y - 1], "");
    let (bob_y, bob) = find_row(&rows, "bob@gmail.com");
    assert_eq!(bob_y, claude_y + 1);
    assert!(
        bob.starts_with("    6  bob@gmail.com  [Personal]   ● active"),
        "{bob}"
    );
    let (_, spend) = find_row(&rows, "$$    ");
    assert!(spend.contains("  25%  "), "{spend}");
    assert!(spend.ends_with("$12.50 / $50.00"), "{spend}");
    let (_, work) = find_row(&rows, "bob@work.com");
    assert_eq!(
        work, "    7  bob@work.com  [Work]   5h 3% · 7d 22% · ♥ 1",
        "a Claude account's limit resets show like Codex's"
    );

    app.handle_key(key(KeyCode::Char('s')), NOW);
    let rows = screen_rows(&render(&mut app, 100, 50, NOW));
    find_row(&rows, "   codex");
    find_row(&rows, "   claude");
    let (_, active) = find_row(&rows, "john.doe@gmail.com");
    assert!(
        active.starts_with(" ▌  2  "),
        "cursor on the lowest active: {active}"
    );
    for _ in 0..4 {
        app.handle_key(key(KeyCode::Char('j')), NOW);
    }
    let rows = screen_rows(&render(&mut app, 100, 50, NOW));
    let (_, bob) = find_row(&rows, "bob@gmail.com");
    assert!(bob.starts_with(" ▌  6  "), "{bob}");
    assert_eq!(
        app.handle_key(key(KeyCode::Char('b')), NOW),
        vec![Command::Action(Action::SwitchBest(Provider::Claude))]
    );
}

#[test]
fn auto_view_without_a_claude_account_shows_the_notice_and_starts_no_engine() {
    let mut app = app_with(TuiStart::Dashboard);
    let commands = app.open_auto(AutoSwitchSettings::default(), NOW);
    assert_eq!(
        commands,
        vec![Command::Refresh { full: false }],
        "no engine: {commands:?}"
    );
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    assert!(rows.iter().any(|r| r.contains(" OFF ")), "{rows:?}");
    assert!(
        rows.iter().any(|r| r.contains(
            "— no Claude Code account: auto-switch covers Claude Code only (ccsw add claude) —"
        )),
        "{rows:?}"
    );
    let commands = app.handle_key(key(KeyCode::Char('l')), NOW);
    assert!(commands.is_empty(), "Go live is inert: {commands:?}");
}
