//! Renders the dashboard and watch screens from an in-memory snapshot into
//! self-contained HTML pages, so the README screenshots can be produced
//! without a live login: `cargo run --example tui_screenshot -- out-dir`.

use std::fmt::Write as _;
use std::path::Path;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use cswitch::cli::tui::TuiStart;
use cswitch::model::{NormalizedUsage, ScopedWindow, Spend, WindowUsage, format_iso};
use cswitch::provider::Provider;
use cswitch::store::usage_store::UsageEntry;
use cswitch::tui::app::App;
use cswitch::tui::snapshot::{AccountSnapshot, AccountsSnapshot};
use cswitch::tui::theme::{DARK, ThemeName};

const NOW: f64 = 1_790_000_000.0;
const COLS: u16 = 96;
const ROWS: u16 = 36;

fn window(pct: f64, reset_in: i64) -> WindowUsage {
    WindowUsage {
        pct,
        resets_at: Some(format_iso(NOW as i64 + reset_in)),
    }
}

fn entry(age: f64, usage: NormalizedUsage) -> UsageEntry {
    UsageEntry {
        sentinel: None,
        last_good: Some(usage),
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

fn account(
    number: u32,
    email: &str,
    tag: &str,
    alias: Option<&str>,
    provider: Provider,
    usage: UsageEntry,
) -> AccountSnapshot {
    AccountSnapshot {
        number,
        provider,
        email: email.to_string(),
        tag: tag.to_string(),
        alias: alias.map(str::to_string),
        disabled: false,
        api_key: false,
        is_active: false,
        usage,
    }
}

fn fixture() -> AccountsSnapshot {
    let dev = account(
        1,
        "alice@corp.io",
        "Team",
        Some("dev"),
        Provider::Codex,
        entry(
            20.0,
            NormalizedUsage {
                five_hour: Some(window(12.0, 4 * 3600 + 2 * 60)),
                seven_day: Some(window(40.0, 4 * 86_400 + 21 * 3600)),
                reset_credits: Some(1),
                ..NormalizedUsage::default()
            },
        ),
    );
    let mut personal = account(
        2,
        "john.doe@gmail.com",
        "Pro 20×",
        None,
        Provider::Codex,
        entry(
            40.0,
            NormalizedUsage {
                five_hour: Some(window(76.0, 2 * 3600 + 47 * 60)),
                seven_day: Some(window(59.0, 5 * 86_400 + 3 * 3600)),
                scoped: vec![ScopedWindow {
                    name: "GPT-5.3-Codex-Spark".into(),
                    pct: 31.0,
                    resets_at: Some(format_iso(NOW as i64 + 5 * 86_400 + 3 * 3600)),
                }],
                reset_credits: Some(2),
                ..NormalizedUsage::default()
            },
        ),
    );
    personal.is_active = true;
    let mut work = account(
        3,
        "john.doe@company.com",
        "Plus",
        None,
        Provider::Codex,
        entry(
            15.0,
            NormalizedUsage {
                five_hour: Some(window(96.0, 2 * 3600 + 37 * 60)),
                seven_day: Some(window(40.0, 4 * 86_400 + 21 * 3600)),
                ..NormalizedUsage::default()
            },
        ),
    );
    work.disabled = true;
    let mut claude_personal = account(
        4,
        "bob@gmail.com",
        "Personal",
        None,
        Provider::Claude,
        entry(
            30.0,
            NormalizedUsage {
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
            },
        ),
    );
    claude_personal.is_active = true;
    let claude_work = account(
        5,
        "bob@work.com",
        "Work",
        None,
        Provider::Claude,
        entry(
            12.0,
            NormalizedUsage {
                five_hour: Some(window(3.0, 3600)),
                seven_day: Some(window(22.0, 86_400)),
                ..NormalizedUsage::default()
            },
        ),
    );
    AccountsSnapshot {
        active_number: Some(2),
        accounts: vec![dev, personal, work, claude_personal, claude_work],
        taken_at: NOW,
    }
}

fn render(start: TuiStart) -> Buffer {
    let mut app = App::new(start, ThemeName::Dark, 90.0, None);
    app.apply_snapshot(fixture(), 1, NOW);
    let backend = TestBackend::new(COLS, ROWS);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame, NOW)).unwrap();
    terminal.backend().buffer().clone()
}

fn css_color(color: Color, fallback: Color) -> String {
    let color = if color == Color::Reset {
        fallback
    } else {
        color
    };
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Black => "#141414".into(),
        Color::White | Color::Gray => "#e8e4de".into(),
        Color::DarkGray => "#8a8a8a".into(),
        Color::Red | Color::LightRed => "#d75f5f".into(),
        Color::Green | Color::LightGreen => "#87af87".into(),
        Color::Yellow | Color::LightYellow => "#d7af5f".into(),
        Color::Blue | Color::LightBlue => "#87afd7".into(),
        Color::Magenta | Color::LightMagenta => "#d787d7".into(),
        Color::Cyan | Color::LightCyan => "#87d7d7".into(),
        _ => css_color(fallback, DARK.fg),
    }
}

fn html(buffer: &Buffer, title: &str) -> String {
    let bg = css_color(DARK.bg, DARK.bg);
    let mut out = String::new();
    write!(
        out,
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title><style>\
         body{{margin:0;background:{bg};}} \
         pre{{margin:0;padding:22px 26px;font:15px/19px Menlo,\"DejaVu Sans Mono\",monospace;color:{fg};}} \
         .b{{font-weight:bold}} .d{{opacity:.55}}\
         </style></head><body><pre>",
        fg = css_color(DARK.fg, DARK.fg)
    )
    .unwrap();
    for y in 0..buffer.area.height {
        let mut run = String::new();
        let mut run_style: Option<(String, String, bool, bool)> = None;
        let flush =
            |out: &mut String, run: &mut String, style: &Option<(String, String, bool, bool)>| {
                if run.is_empty() {
                    return;
                }
                let (fg, cell_bg, bold, dim) = style.clone().unwrap();
                let mut classes = Vec::new();
                if bold {
                    classes.push("b");
                }
                if dim {
                    classes.push("d");
                }
                let class = if classes.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"{}\"", classes.join(" "))
                };
                let background = if cell_bg == bg {
                    String::new()
                } else {
                    format!("background:{cell_bg};")
                };
                write!(
                    out,
                    "<span{class} style=\"color:{fg};{background}\">{run}</span>"
                )
                .unwrap();
                run.clear();
            };
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            let style = (
                css_color(cell.fg, DARK.fg),
                css_color(cell.bg, DARK.bg),
                cell.modifier.contains(Modifier::BOLD),
                cell.modifier.contains(Modifier::DIM),
            );
            if run_style.as_ref() != Some(&style) {
                flush(&mut out, &mut run, &run_style);
                run_style = Some(style);
            }
            let symbol = cell.symbol();
            let escaped = symbol
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            run.push_str(if escaped.is_empty() { " " } else { &escaped });
        }
        flush(&mut out, &mut run, &run_style);
        out.push('\n');
    }
    out.push_str("</pre></body></html>\n");
    out
}

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .expect("usage: tui_screenshot <out-dir>");
    let out_dir = Path::new(&out_dir);
    std::fs::create_dir_all(out_dir).unwrap();
    for (name, start) in [
        ("tui-dashboard", TuiStart::Dashboard),
        ("tui-watch", TuiStart::Watch),
    ] {
        let page = html(&render(start), name);
        let path = out_dir.join(format!("{name}.html"));
        std::fs::write(&path, page).unwrap();
        println!("{}", path.display());
    }
}
