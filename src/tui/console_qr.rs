//! Cached QR renderers. Image protocols use half the width and height of the text QR.

use image::{DynamicImage, GrayImage, Luma};
use qrcode::{Color as Module, QrCode, render::unicode::Dense1x2};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::{FontSize, Image, Resize, protocol::Protocol};

#[derive(Clone, Default)]
pub(super) struct PairingQr {
    pub lines: Vec<Line<'static>>,
    pub image: Option<Protocol>,
}

impl PairingQr {
    pub fn new(url: &str, picker: Option<&Picker>) -> Self {
        let Ok(code) = QrCode::new(url) else {
            return Self::default();
        };
        let light = Style::reset()
            .fg(Color::Rgb(0, 0, 0))
            .bg(Color::Rgb(255, 255, 255));
        let dark = Style::reset()
            .fg(Color::Rgb(255, 255, 255))
            .bg(Color::Rgb(0, 0, 0));
        let lines = code
            .render::<Dense1x2>()
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
            .collect();
        let image = picker
            .filter(|picker| picker.protocol_type() != ProtocolType::Halfblocks)
            .and_then(|picker| {
                let (image, size) = compact_image(&code, picker.font_size())?;
                picker.new_protocol(image, size, Resize::Fit(None)).ok()
            });
        Self { lines, image }
    }

    pub fn size(&self) -> Size {
        self.image.as_ref().map_or_else(
            || {
                Size::new(
                    self.lines.first().map_or(0, Line::width) as u16,
                    self.lines.len() as u16,
                )
            },
            Protocol::size,
        )
    }

    pub fn render_image(&self, area: Rect, buf: &mut Buffer) {
        if let Some(image) = &self.image {
            Image::new(image).render(area, buf);
        }
    }
}

fn compact_image(code: &QrCode, font: FontSize) -> Option<(DynamicImage, Size)> {
    // Keep four white modules on each side. Integer scaling keeps all module edges sharp.
    let modules = code.width() as u32 + 8;
    let size = Size::new(modules.div_ceil(2) as u16, modules.div_ceil(4) as u16);
    let width = u32::from(size.width) * u32::from(font.width);
    let height = u32::from(size.height) * u32::from(font.height);
    let scale = width.min(height) / modules;
    if scale < 2 || width > 4096 || height > 4096 {
        return None;
    }
    let mut image = GrayImage::from_pixel(width, height, Luma([255]));
    let left = (width - modules * scale) / 2 + 4 * scale;
    let top = (height - modules * scale) / 2 + 4 * scale;
    for y in 0..code.width() {
        for x in 0..code.width() {
            if code[(x, y)] == Module::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        image.put_pixel(
                            left + x as u32 * scale + dx,
                            top + y as u32 * scale + dy,
                            Luma([0]),
                        );
                    }
                }
            }
        }
    }
    Some((image.into(), size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_qr_decodes_at_different_cell_sizes() {
        let url = format!(
            "http://100.101.102.103:49152/#pair={}",
            "0123456789abcdef".repeat(4)
        );
        let code = QrCode::new(&url).unwrap();
        for font in [
            FontSize::new(8, 16),
            FontSize::new(9, 18),
            FontSize::new(10, 20),
            FontSize::new(9, 20),
        ] {
            let (image, size) = compact_image(&code, font).unwrap();
            assert_eq!(size, Size::new(25, 13));
            let pixels = image.into_luma8();
            assert!(pixels.pixels().all(|pixel| matches!(pixel.0, [0] | [255])));
            let decoded: Vec<_> = quircs::Quirc::default()
                .identify(
                    pixels.width() as usize,
                    pixels.height() as usize,
                    pixels.as_raw(),
                )
                .map(|code| code.unwrap().decode().unwrap().payload)
                .collect();
            assert_eq!(decoded, vec![url.as_bytes()], "cell size: {font:?}");
        }
    }

    #[test]
    fn invalid_pixel_sizes_keep_the_character_qr() {
        for font in [
            FontSize::new(0, 0),
            FontSize::new(1, 2),
            FontSize::new(u16::MAX, u16::MAX),
        ] {
            #[allow(deprecated)]
            let mut picker = Picker::from_fontsize(font);
            picker.set_protocol_type(ProtocolType::Iterm2);
            let qr = PairingQr::new("http://127.0.0.1:12345/#pair=abcd", Some(&picker));
            assert!(qr.image.is_none());
            assert!(!qr.lines.is_empty());
        }
    }
}
