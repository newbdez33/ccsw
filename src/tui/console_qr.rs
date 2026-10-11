//! Character QR rows with fixed colors and an intact quiet zone.

use qrcode::{QrCode, render::unicode::Dense1x2};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

pub(super) fn lines(url: &str) -> Vec<Line<'static>> {
    let Ok(code) = QrCode::new(url) else {
        return Vec::new();
    };
    let light = Style::reset()
        .fg(Color::Rgb(0, 0, 0))
        .bg(Color::Rgb(255, 255, 255));
    let dark = Style::reset()
        .fg(Color::Rgb(255, 255, 255))
        .bg(Color::Rgb(0, 0, 0));
    code.render::<Dense1x2>()
        .quiet_zone(true)
        .build()
        .lines()
        .map(|row| {
            // Backgrounds fill line spacing that block glyphs can leave above each row.
            Line::from(
                row.chars()
                    .map(|cell| match cell {
                        '█' => Span::styled(" ", dark),
                        '▄' => Span::styled("▄", light),
                        '▀' => Span::styled("▄", dark),
                        _ => Span::styled(" ", light),
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}
