//! Colors and human formatting helpers.

use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use chrono::{Datelike, Local, TimeZone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

/// Should styled output be emitted? Decided once per process, in this order:
/// `NO_COLOR` present (even empty) → off; `FORCE_COLOR` present → on; stdout not a
/// terminal → off; `TERM=dumb` → off; else on.
pub fn colors_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        decide_colors(
            std::env::var_os("NO_COLOR").is_some(),
            std::env::var_os("FORCE_COLOR").is_some(),
            io::stdout().is_terminal(),
            std::env::var("TERM").is_ok_and(|term| term == "dumb"),
        )
    })
}

fn decide_colors(no_color: bool, force_color: bool, is_tty: bool, dumb: bool) -> bool {
    if no_color {
        return false;
    }
    if force_color {
        return true;
    }
    is_tty && !dumb
}

static THEME: AtomicU8 = AtomicU8::new(0);

pub fn set_theme(theme: Theme) {
    THEME.store(theme as u8, Ordering::Relaxed);
}

pub fn theme() -> Theme {
    match THEME.load(Ordering::Relaxed) {
        1 => Theme::Light,
        _ => Theme::Dark,
    }
}

struct Palette {
    accent: &'static str,
    muted: &'static str,
    red: &'static str,
    yellow: &'static str,
}

const DARK: Palette = Palette {
    accent: "\x1b[38;5;173m",
    muted: "\x1b[38;5;250m",
    red: "\x1b[31m",
    yellow: "\x1b[33m",
};

// Light palette: #954c2a, #635d55, #ad3128, #795911 as truecolor.
const LIGHT: Palette = Palette {
    accent: "\x1b[38;2;149;76;42m",
    muted: "\x1b[38;2;99;93;85m",
    red: "\x1b[38;2;173;49;40m",
    yellow: "\x1b[38;2;121;89;17m",
};

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn palette() -> &'static Palette {
    match theme() {
        Theme::Dark => &DARK,
        Theme::Light => &LIGHT,
    }
}

fn styled(codes: &str, text: &str) -> String {
    if colors_enabled() {
        format!("{codes}{text}{RESET}")
    } else {
        text.to_string()
    }
}

pub fn accent(text: &str) -> String {
    styled(palette().accent, text)
}

pub fn muted(text: &str) -> String {
    styled(palette().muted, text)
}

pub fn dimmed(text: &str) -> String {
    styled(DIM, text)
}

pub fn bolded(text: &str) -> String {
    styled(BOLD, text)
}

pub fn bold_accent(text: &str) -> String {
    styled(&format!("{BOLD}{}", palette().accent), text)
}

pub fn yellowed(text: &str) -> String {
    styled(palette().yellow, text)
}

pub fn reddened(text: &str) -> String {
    styled(palette().red, text)
}

/// Yellow line on stdout.
pub fn warning(message: &str) {
    println!("{}", yellowed(message));
}

/// Red line on stderr.
pub fn error(message: &str) {
    eprintln!("{}", reddened(message));
}

/// `just now` under a minute, then `Nm ago`, `Nh ago`, `Nd ago`.
pub fn format_age(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// `45s`, `12m`, `2h 13m` (or `2h`), `3d 4h` (or `3d`).
pub fn format_duration(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m", secs / 60);
    }
    if secs < 86400 {
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        return if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        };
    }
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    if hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d {hours}h")
    }
}

/// `(countdown, clock)` for a reset instant: countdown `Nd Nh` / `Nh Nm` / `Nm`
/// (never negative); clock as local `HH:MM` when the reset falls on today's local
/// date, else `Mon D HH:MM` with an unpadded day.
pub fn countdown_and_clock(resets_at_unix: i64, now_unix: i64) -> (String, String) {
    let remaining = (resets_at_unix - now_unix).max(0);
    let days = remaining / 86400;
    let hours = (remaining % 86400) / 3600;
    let minutes = (remaining % 3600) / 60;
    let countdown = if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    };
    (countdown, clock_in(&Local, resets_at_unix, now_unix))
}

fn clock_in<Tz: TimeZone>(zone: &Tz, resets_at_unix: i64, now_unix: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let (Some(reset), Some(now)) = (
        zone.timestamp_opt(resets_at_unix, 0).single(),
        zone.timestamp_opt(now_unix, 0).single(),
    ) else {
        return String::new();
    };
    if reset.date_naive() == now.date_naive() {
        reset.format("%H:%M").to_string()
    } else {
        format!(
            "{} {} {}",
            reset.format("%b"),
            reset.day(),
            reset.format("%H:%M")
        )
    }
}

/// Print `prompt` and read one line; only `y` / `yes` (case-insensitive) is a yes.
/// EOF or a read error is a no.
pub fn confirm(prompt: &str) -> bool {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    match io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => false,
        Ok(_) => is_yes(&line),
    }
}

fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn color_precedence() {
        // (no_color, force_color, tty, dumb)
        assert!(!decide_colors(true, true, true, false), "NO_COLOR wins");
        assert!(
            decide_colors(false, true, false, true),
            "FORCE_COLOR beats tty and dumb"
        );
        assert!(!decide_colors(false, false, false, false), "not a tty");
        assert!(!decide_colors(false, false, true, true), "TERM=dumb");
        assert!(decide_colors(false, false, true, false));
    }

    #[test]
    fn theme_switches_palette() {
        set_theme(Theme::Light);
        assert_eq!(theme(), Theme::Light);
        assert_eq!(palette().accent, LIGHT.accent);
        set_theme(Theme::Dark);
        assert_eq!(theme(), Theme::Dark);
        assert_eq!(palette().accent, "\x1b[38;5;173m");
        assert_eq!(DARK.muted, "\x1b[38;5;250m");
        assert_eq!(LIGHT.red, "\x1b[38;2;173;49;40m");
        assert_eq!(LIGHT.yellow, "\x1b[38;2;121;89;17m");
        // Under `cargo test` stdout is not a terminal, so styling is a no-op.
        if !colors_enabled() {
            assert_eq!(accent("x"), "x");
            assert_eq!(bold_accent("x"), "x");
            assert_eq!(dimmed("x"), "x");
        }
    }

    #[test]
    fn ages_and_durations() {
        assert_eq!(format_age(0.0), "just now");
        assert_eq!(format_age(59.9), "just now");
        assert_eq!(format_age(60.0), "1m ago");
        assert_eq!(format_age(400.0), "6m ago");
        assert_eq!(format_age(3600.0), "1h ago");
        assert_eq!(format_age(90000.0), "1d ago");
        assert_eq!(format_age(-5.0), "just now");

        assert_eq!(format_duration(42.0), "42s");
        assert_eq!(format_duration(180.0), "3m");
        assert_eq!(format_duration(7980.0), "2h 13m");
        assert_eq!(format_duration(7200.0), "2h");
        assert_eq!(format_duration(93600.0), "1d 2h");
        assert_eq!(format_duration(259200.0), "3d");
        assert_eq!(format_duration(-1.0), "0s");
    }

    #[test]
    fn countdown_shapes() {
        let now = 1_790_000_000;
        assert_eq!(
            countdown_and_clock(now + 3 * 86400 + 4 * 3600 + 59 * 60, now).0,
            "3d 4h"
        );
        assert_eq!(
            countdown_and_clock(now + 4 * 3600 + 12 * 60, now).0,
            "4h 12m"
        );
        assert_eq!(countdown_and_clock(now + 59 * 60 + 30, now).0, "59m");
        assert_eq!(countdown_and_clock(now, now).0, "0m");
        assert_eq!(countdown_and_clock(now - 100, now).0, "0m");
    }

    #[test]
    fn clock_same_day_versus_other_day() {
        let utc = FixedOffset::east_opt(0).unwrap();
        // 2026-09-21T14:13:20Z
        let now = 1_790_000_000;
        assert_eq!(clock_in(&utc, now + 3600, now), "15:13");
        assert_eq!(clock_in(&utc, now + 86400, now), "Sep 22 14:13");
        assert_eq!(
            clock_in(&utc, 1_791_000_000, now),
            "Oct 3 04:00",
            "day is not zero-padded"
        );
        // A zone offset moves the date boundary.
        let tokyo = FixedOffset::east_opt(9 * 3600).unwrap();
        assert_eq!(clock_in(&tokyo, now + 3600, now), "Sep 22 00:13");
    }

    #[test]
    fn yes_answers() {
        assert!(is_yes("y\n"));
        assert!(is_yes("YES"));
        assert!(is_yes("  Yes  "));
        assert!(!is_yes("n"));
        assert!(!is_yes(""));
        assert!(!is_yes("yeah"));
    }
}
