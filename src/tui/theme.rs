//! Palettes and theme resolution (research notes `cswap-tui.md` §10.1, §10.3).

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Dark,
    Light,
    Auto,
}

impl ThemeName {
    /// The `ui.theme` setting; anything unknown reads as `auto`.
    pub fn parse(value: &str) -> Self {
        match value {
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => Self::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Auto => "auto",
        }
    }

    /// `ctrl+t`: dark → light → auto → dark.
    pub fn next(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Auto,
            Self::Auto => Self::Dark,
        }
    }

    /// `auto` follows the terminal background reported by `COLORFGBG`, else dark.
    pub fn resolve(self, colorfgbg: Option<&str>) -> Resolved {
        match self {
            Self::Dark => Resolved::Dark,
            Self::Light => Resolved::Light,
            Self::Auto => {
                if colorfgbg.is_some_and(is_light_background) {
                    Resolved::Light
                } else {
                    Resolved::Dark
                }
            }
        }
    }
}

/// `COLORFGBG` is `fg;bg` or `fg;default;bg`; the last field is the background
/// (0-6 and 8 are dark, 7 and 9-15 light, as in vim's heuristic).
fn is_light_background(value: &str) -> bool {
    value
        .rsplit(';')
        .next()
        .and_then(|bg| bg.trim().parse::<u8>().ok())
        .is_some_and(|bg| bg == 7 || (9..=15).contains(&bg))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub accent: Color,
    pub fg: Color,
    pub muted: Color,
    pub bg: Color,
    pub surface: Color,
    pub panel: Color,
    pub ok: Color,
    pub warn: Color,
    pub crit: Color,
    pub track: Color,
}

pub const DARK: Palette = Palette {
    accent: Color::Rgb(0xd7, 0x87, 0x5f),
    fg: Color::Rgb(0xe8, 0xe4, 0xde),
    muted: Color::Rgb(0x8a, 0x8a, 0x8a),
    bg: Color::Rgb(0x14, 0x14, 0x14),
    surface: Color::Rgb(0x1e, 0x1e, 0x1e),
    panel: Color::Rgb(0x26, 0x26, 0x26),
    ok: Color::Rgb(0x87, 0xaf, 0x87),
    warn: Color::Rgb(0xd7, 0xaf, 0x5f),
    crit: Color::Rgb(0xd7, 0x5f, 0x5f),
    track: Color::Rgb(0x3a, 0x3a, 0x3a),
};

pub const LIGHT: Palette = Palette {
    accent: Color::Rgb(0x95, 0x4c, 0x2a),
    fg: Color::Rgb(0x2b, 0x27, 0x23),
    muted: Color::Rgb(0x63, 0x5d, 0x55),
    bg: Color::Rgb(0xfa, 0xf7, 0xf2),
    surface: Color::Rgb(0xef, 0xea, 0xe1),
    panel: Color::Rgb(0xe2, 0xdb, 0xcf),
    ok: Color::Rgb(0x3d, 0x6b, 0x3d),
    warn: Color::Rgb(0x79, 0x59, 0x11),
    crit: Color::Rgb(0xad, 0x31, 0x28),
    track: Color::Rgb(0xce, 0xc7, 0xba),
};

pub const WARN_PCT: f64 = 70.0;
pub const CRIT_PCT: f64 = 90.0;

impl Palette {
    pub fn for_theme(resolved: Resolved) -> Self {
        match resolved {
            Resolved::Dark => DARK,
            Resolved::Light => LIGHT,
        }
    }

    /// `None` → muted; ≥ 90 → crit; ≥ 70 → warn; else ok.
    pub fn severity(&self, pct: Option<f64>) -> Color {
        match pct {
            None => self.muted,
            Some(p) if p >= CRIT_PCT => self.crit,
            Some(p) if p >= WARN_PCT => self.warn,
            Some(_) => self.ok,
        }
    }

    pub fn fg_style(&self) -> Style {
        Style::new().fg(self.fg)
    }

    pub fn muted_style(&self) -> Style {
        Style::new().fg(self.muted)
    }

    pub fn accent_style(&self) -> Style {
        Style::new().fg(self.accent)
    }

    pub fn bold_accent(&self) -> Style {
        Style::new().fg(self.accent).add_modifier(Modifier::BOLD)
    }

    pub fn bold_fg(&self) -> Style {
        Style::new().fg(self.fg).add_modifier(Modifier::BOLD)
    }

    pub fn track_style(&self) -> Style {
        Style::new().fg(self.track)
    }

    pub fn warn_style(&self) -> Style {
        Style::new().fg(self.warn)
    }

    pub fn crit_style(&self) -> Style {
        Style::new().fg(self.crit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_colorfgbg() {
        assert_eq!(ThemeName::Auto.resolve(None), Resolved::Dark);
        assert_eq!(ThemeName::Auto.resolve(Some("0;15")), Resolved::Light);
        assert_eq!(ThemeName::Auto.resolve(Some("15;0")), Resolved::Dark);
        assert_eq!(
            ThemeName::Auto.resolve(Some("0;default;7")),
            Resolved::Light
        );
        assert_eq!(ThemeName::Auto.resolve(Some("7;8")), Resolved::Dark);
        assert_eq!(ThemeName::Auto.resolve(Some("garbage")), Resolved::Dark);
        assert_eq!(ThemeName::Dark.resolve(Some("0;15")), Resolved::Dark);
        assert_eq!(ThemeName::Light.resolve(None), Resolved::Light);
    }

    #[test]
    fn cycle_and_parse() {
        assert_eq!(ThemeName::parse("dark").next(), ThemeName::Light);
        assert_eq!(ThemeName::Light.next(), ThemeName::Auto);
        assert_eq!(ThemeName::Auto.next(), ThemeName::Dark);
        assert_eq!(ThemeName::parse("bogus"), ThemeName::Auto);
        assert_eq!(ThemeName::Auto.as_str(), "auto");
    }

    #[test]
    fn severity_bands() {
        let p = DARK;
        assert_eq!(p.severity(None), p.muted);
        assert_eq!(p.severity(Some(69.9)), p.ok);
        assert_eq!(p.severity(Some(70.0)), p.warn);
        assert_eq!(p.severity(Some(89.9)), p.warn);
        assert_eq!(p.severity(Some(90.0)), p.crit);
        assert_eq!(LIGHT.severity(Some(95.0)), LIGHT.crit);
    }
}
