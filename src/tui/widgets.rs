//! Shared rendering: usage bars, account cards, mini lines, the accounts
//! panel, footer chips and toasts (research notes `cswap-tui.md` §3, §10.4–§10.5).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    StatefulWidget, Widget, Wrap,
};

use crate::provider::Provider;
use crate::store::usage_store::{UsageEntry, UsageSentinel};

use super::data::{
    DisplayRow, age_note, credits_text, display_rows, is_stale, last_seen_note, reset_cards_text,
    reset_text, sentinel_label, spend_row,
};
use super::snapshot::{AccountSnapshot, AccountsSnapshot};
use super::theme::Palette;

pub const BAR_FILLED: &str = "━";
pub const BAR_HALF: &str = "╸";
pub const BAR_EMPTY: &str = "─";
pub const BAR_TICK: &str = "┃";
pub const BAR_MIN: usize = 12;
pub const BAR_MAX: usize = 30;

/// One cell per column: `━` filled, `╸` half, `─` track, `┃` at the threshold
/// (drawn even inside the filled region). Fill and number dim when stale.
pub fn bar_cells(
    pct: Option<f64>,
    width: usize,
    stale: bool,
    threshold: Option<f64>,
    p: &Palette,
) -> Vec<Span<'static>> {
    let track = p.track_style();
    let Some(pct) = pct else {
        return vec![Span::styled(BAR_EMPTY.repeat(width), track)];
    };
    let cells = pct.clamp(0.0, 100.0) / 100.0 * width as f64;
    let full = cells.floor() as usize;
    let half = cells - full as f64 >= 0.5 && full < width;
    let tick_at = threshold
        .map(|t| ((t / 100.0 * width as f64).round() as usize).clamp(0, width.saturating_sub(1)));
    let mut fill = Style::new().fg(p.severity(Some(pct)));
    if stale {
        fill = fill.add_modifier(Modifier::DIM);
    }
    (0..width)
        .map(|i| {
            if tick_at == Some(i) {
                Span::styled(BAR_TICK, p.warn_style())
            } else if i < full {
                Span::styled(BAR_FILLED, fill)
            } else if i == full && half {
                Span::styled(BAR_HALF, fill)
            } else {
                Span::styled(BAR_EMPTY, track)
            }
        })
        .collect()
}

/// `5h ━━━━╸────┃──  47%  resets 2h 13m · 20:39`.
pub fn usage_bar(
    label: &str,
    pct: Option<f64>,
    suffix: &str,
    width: usize,
    stale: bool,
    threshold: Option<f64>,
    p: &Palette,
) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label} "), p.muted_style())];
    spans.extend(bar_cells(pct, width, stale, threshold, p));
    match pct {
        None => spans.push(Span::styled("  usage unknown", p.muted_style())),
        Some(pct) => {
            let mut style = Style::new().fg(p.severity(Some(pct)));
            if stale {
                style = style.add_modifier(Modifier::DIM);
            }
            spans.push(Span::styled(format!(" {pct:>3.0}%"), style));
        }
    }
    if !suffix.is_empty() {
        spans.push(Span::styled(format!("  {suffix}"), p.muted_style()));
    }
    Line::from(spans)
}

fn header_line(acc: &AccountSnapshot, number_style: Style, p: &Palette) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{:>2}  ", acc.number), number_style)];
    match acc.alias.as_deref().filter(|a| !a.is_empty()) {
        Some(alias) => {
            spans.push(Span::styled(alias.to_string(), p.bold_accent()));
            spans.push(Span::styled(format!(" ({})", acc.email), p.fg_style()));
        }
        None => spans.push(Span::styled(acc.email.clone(), p.fg_style())),
    }
    spans.push(Span::styled(format!("  [{}]", acc.tag), p.muted_style()));
    Line::from(spans)
}

/// The full card: header plus bars, a sentinel branch, or `usage unavailable`.
/// `threshold` draws the `┃` tick (dashboard and auto panel only). Rows wider
/// than `width` wrap like cswap's cards.
pub fn account_card(
    acc: &AccountSnapshot,
    width: usize,
    threshold: Option<f64>,
    now: f64,
    p: &Palette,
) -> Vec<Line<'static>> {
    card_lines(acc, width, threshold, now, p)
        .into_iter()
        .flat_map(|line| wrap_line(line, width))
        .collect()
}

/// The card's rows before wrapping; see [`account_card`].
fn card_lines(
    acc: &AccountSnapshot,
    width: usize,
    threshold: Option<f64>,
    now: f64,
    p: &Palette,
) -> Vec<Line<'static>> {
    let mut header = header_line(acc, p.bold_fg(), p);
    if acc.is_active {
        header.push_span(Span::styled("   ● active", p.bold_accent()));
    }
    if acc.disabled {
        header.push_span(Span::styled("   (disabled)", p.muted_style()));
    }
    if let Some(note) = age_note(acc.usage.age_s) {
        header.push_span(Span::styled(format!("   {note}"), p.muted_style()));
    }
    if let Some(cards) = reset_cards_spans(reset_credits(&acc.usage), p) {
        header.push_span(Span::raw("   "));
        header.spans.extend(cards);
    }
    let mut lines = vec![header];
    let usage = &acc.usage;
    if let Some(sentinel) = usage.sentinel {
        let (marker, style) = if sentinel == UsageSentinel::ApiKey {
            ("·", p.muted_style())
        } else {
            ("⚠", p.warn_style())
        };
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(marker, style),
            Span::raw(" "),
            Span::styled(sentinel_label(sentinel), style),
        ]));
        if sentinel != UsageSentinel::ApiKey
            && let Some(note) = last_seen_note(usage, now)
        {
            lines.push(Line::from(Span::styled(
                format!("    └ {note}"),
                p.muted_style(),
            )));
        }
        return lines;
    }
    let rows: Vec<DisplayRow> = usage
        .last_good
        .as_ref()
        .map(|last| {
            spend_row(last, now as i64)
                .into_iter()
                .chain(display_rows(last, usage.fetched_at, now as i64))
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        let mut text = "    usage unavailable".to_string();
        if let Some(error) = &usage.last_error {
            text.push_str(&format!(" · {error}"));
        }
        lines.push(Line::from(Span::styled(text, p.muted_style())));
        return lines;
    }
    let stale = is_stale(usage);
    let label_width = rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(2);
    let bar_width =
        (width as i64 - 42 - label_width as i64).clamp(BAR_MIN as i64, BAR_MAX as i64) as usize;
    let row_overhead = 4 + label_width + 1 + bar_width + 5 + 2;
    for row in &rows {
        let suffix = if row_overhead + row.suffix_full.chars().count() <= width {
            &row.suffix_full
        } else {
            &row.suffix
        };
        let label = format!("{:<label_width$}", row.label);
        let mut line = usage_bar(
            &label,
            Some(row.pct),
            suffix,
            bar_width,
            stale,
            threshold,
            p,
        );
        line.spans.insert(0, Span::raw("    "));
        lines.push(line);
    }
    if let Some(credits) = credits_text(usage.last_good.as_ref().and_then(|l| l.credits.as_ref())) {
        lines.push(Line::from(Span::styled(
            format!("    {credits}"),
            p.muted_style(),
        )));
    }
    lines
}

fn reset_credits(usage: &UsageEntry) -> Option<u32> {
    usage.last_good.as_ref().and_then(|last| last.reset_credits)
}

/// `♥ 2` as spans: a red heart and the green count.
fn reset_cards_spans(count: Option<u32>, p: &Palette) -> Option<Vec<Span<'static>>> {
    let text = reset_cards_text(count)?;
    let (heart, count) = text.split_once(' ')?;
    Some(vec![
        Span::styled(heart.to_string(), p.bold_crit()),
        Span::styled(format!(" {count}"), p.bold_ok()),
    ])
}

/// The one-line form used for inactive accounts on the dashboard:
/// ` 2  work@acme.dev  [personal]   5h 92% · 7d 63% (ahead) · Fable (!)`.
pub fn mini_line(acc: &AccountSnapshot, now: f64, p: &Palette) -> Line<'static> {
    let mut line = header_line(
        acc,
        Style::new().fg(p.muted).add_modifier(Modifier::BOLD),
        p,
    );
    if acc.disabled {
        line.push_span(Span::styled("  (disabled)", p.muted_style()));
    }
    line.push_span(Span::raw("   "));
    let usage = &acc.usage;
    if let Some(sentinel) = usage.sentinel {
        let style = if sentinel == UsageSentinel::ApiKey {
            p.muted_style()
        } else {
            p.warn_style()
        };
        line.push_span(Span::styled(sentinel_label(sentinel), style));
        return line;
    }
    let stale = is_stale(usage);
    let rows = usage
        .last_good
        .as_ref()
        .map(|last| display_rows(last, usage.fetched_at, now as i64))
        .unwrap_or_default();
    let mut parts: Vec<Vec<Span<'static>>> = Vec::new();
    for row in &rows {
        let scoped = row.label != "5h" && row.label != "7d";
        if scoped {
            if row.maxed_pool {
                parts.push(vec![Span::styled(
                    format!("{} (!)", row.label),
                    p.crit_style(),
                )]);
            }
            continue;
        }
        let mut pct_style = Style::new().fg(p.severity(Some(row.pct)));
        if stale {
            pct_style = pct_style.add_modifier(Modifier::DIM);
        }
        let mut part = vec![
            Span::styled(format!("{} ", row.label), p.muted_style()),
            Span::styled(format!("{:.0}%", row.pct), pct_style),
        ];
        if row.pct >= 100.0 {
            if let Some(reset) = reset_text(row.resets_at, now as i64) {
                part.push(Span::styled(format!(" ({reset})"), p.muted_style()));
            }
        } else if row.label == "7d" && row.ahead {
            part.push(Span::styled(" (ahead)", p.warn_style()));
        }
        parts.push(part);
    }
    if let Some(cards) = reset_cards_spans(reset_credits(usage), p) {
        parts.push(cards);
    }
    if parts.is_empty() {
        line.push_span(Span::styled("usage unknown", p.muted_style()));
        return line;
    }
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            line.push_span(Span::styled(" · ", p.track_style()));
        }
        for span in part {
            line.push_span(span);
        }
    }
    line
}

/// `codex` / `claude` above a provider's accounts when the roster is mixed.
pub fn section_header(provider: Provider, p: &Palette) -> Line<'static> {
    Line::from(Span::styled(provider.as_str().to_string(), p.muted_style()))
}

/// The dashboard monitor (`show_minis`) and the auto screen's active card.
pub fn accounts_panel(
    snapshot: Option<&AccountsSnapshot>,
    width: usize,
    threshold: Option<f64>,
    show_minis: bool,
    now: f64,
    p: &Palette,
) -> Vec<Line<'static>> {
    let muted = |text: &str| Line::from(Span::styled(text.to_string(), p.muted_style()));
    let Some(snapshot) = snapshot else {
        return vec![muted("loading…")];
    };
    if snapshot.accounts.is_empty() {
        return vec![
            muted("No managed accounts yet."),
            muted(
                "Use the menu below: Add account — from your current Codex or Claude Code login, or from a token.",
            ),
        ];
    }
    let mixed = snapshot.is_mixed();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (provider, group) in snapshot.grouped() {
        let mut rows: Vec<Line<'static>> = Vec::new();
        let mut last_was_card = false;
        for acc in group {
            if acc.is_active {
                let card = account_card(acc, width, threshold, now, p);
                if !rows.is_empty() {
                    rows.push(Line::default());
                }
                rows.extend(card);
                last_was_card = true;
            } else if show_minis {
                if last_was_card {
                    rows.push(Line::default());
                }
                rows.extend(wrap_line(mini_line(acc, now, p), width));
                last_was_card = false;
            }
        }
        if rows.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        if mixed {
            lines.push(section_header(provider, p));
        }
        lines.extend(rows);
    }
    if lines.is_empty() {
        return vec![muted("no active managed login")];
    }
    lines
}

/// Footer chips: ` key ` in accent plus the description, two spaces apart.
pub fn footer_line(chips: &[(&str, &str)], p: &Palette) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, label)) in chips.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(format!(" {key} "), p.bold_accent()));
        spans.push(Span::styled(format!(" {label}"), p.fg_style()));
    }
    Line::from(spans)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub title: Option<String>,
    pub text: String,
    pub severity: Severity,
    pub expires_at: f64,
}

/// Bottom-right stack, newest at the bottom, each a bordered box.
pub fn render_toasts(buf: &mut Buffer, area: Rect, toasts: &[Toast], p: &Palette) {
    let max_width = (area.width as usize * 2 / 5).clamp(24, 56) as u16;
    let mut bottom = area.bottom();
    for toast in toasts.iter().rev() {
        let inner_width = max_width.saturating_sub(4) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();
        if let Some(title) = &toast.title {
            lines.push(Line::from(Span::styled(title.clone(), p.bold_fg())));
        }
        let body_rows = wrap_count(&toast.text, inner_width);
        lines.push(Line::from(Span::styled(toast.text.clone(), p.fg_style())));
        let height = (lines.len() - 1 + body_rows) as u16 + 2;
        if bottom < area.y + height {
            break;
        }
        let rect = Rect::new(
            area.right().saturating_sub(max_width + 1),
            bottom - height,
            max_width,
            height,
        );
        let border = match toast.severity {
            Severity::Info => p.accent,
            Severity::Warning => p.warn,
            Severity::Error => p.crit,
        };
        Clear.render(rect, buf);
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(Style::new().bg(p.surface))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(border).bg(p.surface))
                    .padding(ratatui::widgets::Padding::horizontal(1)),
            )
            .render(rect, buf);
        bottom = rect.y.saturating_sub(1);
    }
}

/// What a scrollbar needs to know about a scrolled region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollInfo {
    pub total: usize,
    pub viewport: usize,
    pub position: usize,
}

impl ScrollInfo {
    pub fn overflows(&self) -> bool {
        self.total > self.viewport
    }
}

/// A one-column vertical scrollbar in `area`, drawn only when the region
/// overflows (the thin bar cswap's lists and screen show).
pub fn render_scrollbar(buf: &mut Buffer, area: Rect, info: ScrollInfo, p: &Palette) {
    if area.is_empty() || !info.overflows() {
        return;
    }
    // ratatui treats `position` as an index into `content_length`, so the
    // content is the scroll range: the thumb then reaches the bottom exactly
    // when the last row is in view.
    let mut state = ScrollbarState::new(info.total - info.viewport + 1)
        .position(info.position)
        .viewport_content_length(info.viewport);
    Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .thumb_symbol("█")
        .track_style(p.track_style())
        .thumb_style(Style::new().fg(p.muted))
        .render(area, buf, &mut state);
}

/// Word-wrap a styled line at `width` the way cswap's cards wrap: break at
/// spaces, split a word longer than the width, and start continuation rows
/// at the first column. The first row keeps its leading indent.
pub fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    let mut rows: Vec<Vec<(char, Style)>> = vec![Vec::new()];
    let mut i = 0;
    while i < cells.len() {
        let space = cells[i].0 == ' ';
        let mut j = i;
        while j < cells.len() && (cells[j].0 == ' ') == space {
            j += 1;
        }
        let run = &cells[i..j];
        i = j;
        let row = rows.last().expect("row");
        let row_len = row.len();
        if space {
            // Gaps and the first row's indent stay; spaces at a break or at
            // the start of a continuation row are dropped.
            if row_len == 0 && rows.len() > 1 {
                continue;
            }
            if row_len + run.len() <= width {
                rows.last_mut().expect("row").extend_from_slice(run);
            } else {
                rows.push(Vec::new());
            }
            continue;
        }
        if row.iter().any(|(c, _)| *c != ' ') && row_len + run.len() > width {
            rows.push(Vec::new());
        }
        let mut rest = run;
        loop {
            let row = rows.last_mut().expect("row");
            let room = width - row.len();
            if rest.len() <= room {
                row.extend_from_slice(rest);
                break;
            }
            row.extend_from_slice(&rest[..room]);
            rest = &rest[room..];
            rows.push(Vec::new());
        }
    }
    rows.into_iter()
        .map(|mut row| {
            while row.last().is_some_and(|(c, _)| *c == ' ') {
                row.pop();
            }
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (c, style) in row {
                match spans.last_mut() {
                    Some(span) if span.style == style => span.content.to_mut().push(c),
                    _ => spans.push(Span::styled(c.to_string(), style)),
                }
            }
            Line::from(spans).style(line.style)
        })
        .collect()
}

/// Rows a text takes when wrapped at `width` (word wrap, long words split).
pub fn wrap_count(text: &str, width: usize) -> usize {
    wrap_text(text, width).len().max(1)
}

/// Greedy word wrap on spaces; words longer than `width` are split.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for raw in text.split('\n') {
        let mut row = String::new();
        let mut row_len = 0;
        for word in raw.split(' ') {
            let mut word: Vec<char> = word.chars().collect();
            loop {
                let len = word.len();
                if row_len == 0 {
                    if len <= width {
                        row.extend(word.iter());
                        row_len = len;
                        break;
                    }
                    row.extend(word[..width].iter());
                    rows.push(std::mem::take(&mut row));
                    word = word[width..].to_vec();
                    continue;
                }
                if row_len + 1 + len <= width {
                    row.push(' ');
                    row.extend(word.iter());
                    row_len += 1 + len;
                    break;
                }
                rows.push(std::mem::take(&mut row));
                row_len = 0;
            }
        }
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NormalizedUsage, ScopedWindow, WindowUsage, format_iso};
    use crate::provider::Provider;
    use crate::tui::test_support::{account, entry};
    use crate::tui::theme::DARK;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn bar(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn bar_glyphs_tick_and_severity() {
        let p = &DARK;
        let cells = bar_cells(Some(76.0), 30, false, Some(90.0), p);
        assert_eq!(bar(&cells), "━━━━━━━━━━━━━━━━━━━━━━╸────┃──");
        assert_eq!(cells[0].style.fg, Some(p.warn));
        assert_eq!(cells[27].style.fg, Some(p.warn), "tick is warn-colored");
        assert_eq!(cells[29].style.fg, Some(p.track));
        assert!(!cells[0].style.add_modifier.contains(Modifier::DIM));

        let stale = bar_cells(Some(96.0), 12, true, None, p);
        assert_eq!(bar(&stale), "━━━━━━━━━━━╸");
        assert_eq!(stale[0].style.fg, Some(p.crit));
        assert!(stale[0].style.add_modifier.contains(Modifier::DIM));

        let empty = bar_cells(None, 5, false, Some(50.0), p);
        assert_eq!(bar(&empty), "─────");

        let full = bar_cells(Some(100.0), 10, false, Some(90.0), p);
        assert_eq!(bar(&full), "━━━━━━━━━┃");
        let low = bar_cells(Some(0.0), 4, false, Some(0.0), p);
        assert_eq!(bar(&low), "┃───");
        let tick_clamped = bar_cells(Some(0.0), 4, false, Some(100.0), p);
        assert_eq!(bar(&tick_clamped), "───┃");
    }

    #[test]
    fn usage_bar_row_text() {
        let p = &DARK;
        let line = usage_bar(
            "5h",
            Some(47.0),
            "resets 2h 13m · 20:39",
            12,
            false,
            Some(90.0),
            p,
        );
        assert_eq!(text(&line), "5h ━━━━━╸─────┃  47%  resets 2h 13m · 20:39");
        let unknown = usage_bar("7d", None, "", 12, false, None, p);
        assert_eq!(text(&unknown), "7d ────────────  usage unknown");
    }

    #[test]
    fn card_rows_only_for_present_windows_and_fit_rule() {
        let p = &DARK;
        let now = 1_790_000_000.0;
        let mut acc = account(
            2,
            "john.doe@gmail.com",
            true,
            entry(Some(now - 400.0), None),
        );
        acc.tag = "Personal".into();
        acc.usage.age_s = Some(400.0);
        acc.usage.last_good = Some(NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: 76.0,
                resets_at: Some(format_iso(now as i64 + 2 * 3600 + 47 * 60)),
            }),
            scoped: vec![ScopedWindow {
                name: "Fable".into(),
                pct: 100.0,
                resets_at: None,
            }],
            ..NormalizedUsage::default()
        });
        acc.usage.last_good.as_mut().unwrap().reset_credits = Some(2);
        let lines = account_card(&acc, 100, Some(90.0), now, p);
        assert_eq!(
            text(&lines[0]),
            " 2  john.doe@gmail.com  [Personal]   ● active   · 6m ago   ♥ 2"
        );
        assert_eq!(lines.len(), 3, "no 7d row without a weekly window");
        let five = text(&lines[1]);
        assert!(
            five.starts_with("    5h    ━━━━━━━━━━━━━━━━━━━━━━╸────┃──  76%  resets 2h 47m · "),
            "{five}"
        );
        assert_eq!(
            text(&lines[2]),
            "    Fable ━━━━━━━━━━━━━━━━━━━━━━━━━━━┃━━ 100%  (!)"
        );
        assert!(
            lines[1].spans[2].style.add_modifier.contains(Modifier::DIM),
            "dim at age > 300 s"
        );

        // Narrow: the clock is dropped per row when it does not fit. Width 48
        // rejects even the short same-day clock form, whatever the local zone.
        // (The 61-column header wraps first, so find the bar row by label.)
        let narrow = account_card(&acc, 48, None, now, p);
        let bar = narrow
            .iter()
            .map(text)
            .find(|t| t.starts_with("    5h"))
            .unwrap();
        assert_eq!(bar, "    5h    ━━━━━━━━━───  76%  resets 2h 47m");
        assert!(!bar.contains('┃'), "no tick without a threshold");
    }

    #[test]
    fn card_sentinel_and_unavailable_branches() {
        let p = &DARK;
        let now = 1000.0;
        let mut acc = account(4, "x@y.z", false, entry(Some(280.0), Some(61.0)));
        acc.usage.sentinel = Some(UsageSentinel::TokenExpired);
        let lines = account_card(&acc, 100, Some(90.0), now, p);
        assert_eq!(lines.len(), 3);
        assert_eq!(
            text(&lines[1]),
            "    ⚠ token expired — refresh deferred this pass; retries automatically"
        );
        assert_eq!(text(&lines[2]), "    └ last seen 61% used · 12m ago");

        let mut key = account(
            5,
            "api-key-5@token.local",
            false,
            entry(Some(900.0), Some(10.0)),
        );
        key.api_key = true;
        key.usage.sentinel = Some(UsageSentinel::ApiKey);
        let lines = account_card(&key, 100, None, now, p);
        assert_eq!(lines.len(), 2, "api key never gets a last-seen line");
        assert_eq!(text(&lines[1]), "    · API key (no quota)");

        let mut none = account(6, "n@y.z", false, entry(None, None));
        none.usage.last_error = Some("http-429".into());
        let lines = account_card(&none, 100, None, now, p);
        assert_eq!(text(&lines[1]), "    usage unavailable · http-429");

        let mut credits = account(7, "c@y.z", false, entry(Some(990.0), Some(5.0)));
        credits.usage.last_good.as_mut().unwrap().credits = Some(crate::model::Credits {
            balance: Some(12.5),
            unlimited: false,
        });
        let lines = account_card(&credits, 100, None, now, p);
        assert_eq!(text(&lines[2]), "    credits $12.50");
        credits.usage.last_good.as_mut().unwrap().reset_credits = Some(2);
        let lines = account_card(&credits, 100, None, now, p);
        assert_eq!(
            text(&lines[0]),
            " 7  c@y.z  [personal]   ♥ 2",
            "reset cards close the header line"
        );
        let cards = lines[0].spans.last().unwrap();
        assert_eq!(cards.style.fg, Some(p.ok), "green");
        assert!(cards.style.add_modifier.contains(Modifier::BOLD), "bold");
        assert_eq!(text(&lines[2]), "    credits $12.50");
        credits.usage.last_good.as_mut().unwrap().credits = None;
        credits.usage.last_good.as_mut().unwrap().reset_credits = Some(0);
        let lines = account_card(&credits, 100, None, now, p);
        assert_eq!(text(&lines[0]), " 7  c@y.z  [personal]", "zero is hidden");
        assert_eq!(lines.len(), 2, "no notes line without credits");
    }

    #[test]
    fn card_shows_spend_row_first_and_mini_omits_it() {
        let p = &DARK;
        let now = 1_790_000_000.0;
        let mut acc = account(4, "s@y.z", false, entry(Some(now), Some(5.0)));
        acc.usage.age_s = Some(0.0);
        acc.usage.last_good.as_mut().unwrap().spend = Some(crate::model::Spend {
            used: 12.5,
            limit: 50.0,
            pct: 25.0,
            currency: "USD".into(),
            resets_at: None,
        });
        let lines = account_card(&acc, 100, None, now, p);
        assert!(text(&lines[1]).contains("$$"), "{}", text(&lines[1]));
        assert!(text(&lines[1]).contains("$12.50 / $50.00"));
        assert!(text(&lines[2]).contains("5h"), "{}", text(&lines[2]));
        assert!(!text(&mini_line(&acc, now, p)).contains("$$"));
    }

    #[test]
    fn mini_line_shapes() {
        let p = &DARK;
        let now = 1_790_000_000.0;
        let mut acc = account(2, "work@acme.dev", false, entry(Some(now), None));
        acc.usage.age_s = Some(0.0);
        acc.usage.last_good = Some(NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: 92.0,
                resets_at: None,
            }),
            seven_day: Some(WindowUsage {
                pct: 63.0,
                resets_at: Some(format_iso(now as i64 + 4 * 86_400)),
            }),
            scoped: vec![ScopedWindow {
                name: "Fable".into(),
                pct: 100.0,
                resets_at: None,
            }],
            reset_credits: Some(2),
            spend: None,
            ..NormalizedUsage::default()
        });
        let line = mini_line(&acc, now, p);
        assert_eq!(
            text(&line),
            " 2  work@acme.dev  [personal]   5h 92% · 7d 63% (ahead) · Fable (!) · ♥ 2"
        );
        let cards = line.spans.last().unwrap();
        assert_eq!(cards.style.fg, Some(p.ok), "green");
        assert!(cards.style.add_modifier.contains(Modifier::BOLD), "bold");
        acc.usage.last_good.as_mut().unwrap().reset_credits = Some(0);
        assert!(
            text(&mini_line(&acc, now, p)).ends_with("Fable (!)"),
            "zero is hidden"
        );
        acc.disabled = true;
        acc.usage.sentinel = Some(UsageSentinel::ReloginNeeded);
        let line = mini_line(&acc, now, p);
        assert_eq!(
            text(&line),
            " 2  work@acme.dev  [personal]  (disabled)   re-login needed — refresh token dead; log in with Codex, then run: ccsw add"
        );
        let unknown = account(3, "u@v.w", false, entry(None, None));
        assert_eq!(
            text(&mini_line(&unknown, now, p)),
            " 3  u@v.w  [personal]   usage unknown"
        );
        let mut maxed = account(1, "m@v.w", false, entry(Some(now), Some(100.0)));
        maxed
            .usage
            .last_good
            .as_mut()
            .unwrap()
            .five_hour
            .as_mut()
            .unwrap()
            .resets_at = Some(format_iso(now as i64 + 600));
        assert_eq!(
            text(&mini_line(&maxed, now, p)),
            " 1  m@v.w  [personal]   5h 100% (resets 10m)"
        );
    }

    #[test]
    fn panel_join_rules_and_empty_states() {
        let p = &DARK;
        assert_eq!(
            text(&accounts_panel(None, 80, None, true, 0.0, p)[0]),
            "loading…"
        );
        let empty = crate::tui::test_support::snapshot(Vec::new(), 0.0);
        let lines = accounts_panel(Some(&empty), 80, None, true, 0.0, p);
        assert_eq!(text(&lines[0]), "No managed accounts yet.");
        assert!(text(&lines[1]).contains("Claude Code login"));

        let snap = crate::tui::test_support::snapshot(
            vec![
                account(1, "a@x.y", false, entry(None, None)),
                account(2, "b@x.y", true, entry(Some(1000.0), Some(10.0))),
                account(3, "c@x.y", false, entry(None, None)),
            ],
            1000.0,
        );
        let lines = accounts_panel(Some(&snap), 80, Some(90.0), true, 1000.0, p);
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert!(texts[0].starts_with(" 1  a@x.y"));
        assert_eq!(texts[1], "", "blank before the card");
        assert!(texts[2].starts_with(" 2  b@x.y"));
        assert!(texts[3].contains('┃'));
        assert_eq!(texts[4], "", "blank after the card");
        assert!(texts[5].starts_with(" 3  c@x.y"));
        let only_card = accounts_panel(Some(&snap), 80, None, false, 1000.0, p);
        assert_eq!(only_card.len(), 2);
        let mut no_active = snap.clone();
        no_active.accounts[1].is_active = false;
        let lines = accounts_panel(Some(&no_active), 80, None, false, 1000.0, p);
        assert_eq!(text(&lines[0]), "no active managed login");
    }

    #[test]
    fn mixed_panel_gets_a_header_per_provider_and_single_does_not() {
        use crate::tui::test_support::claude_account;
        let p = &DARK;
        let mixed = crate::tui::test_support::snapshot(
            vec![
                account(1, "a@x.y", true, entry(Some(900.0), Some(10.0))),
                claude_account(2, "c@x.y", true, entry(Some(900.0), Some(20.0))),
                claude_account(3, "d@x.y", false, entry(Some(900.0), Some(30.0))),
            ],
            1000.0,
        );
        let lines = accounts_panel(Some(&mixed), 90, Some(90.0), true, 1000.0, p);
        let lines: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(lines[0], "codex");
        assert!(lines[1].starts_with(" 1  a@x.y"), "{}", lines[1]);
        let claude_at = lines.iter().position(|l| l == "claude").unwrap();
        assert_eq!(lines[claude_at - 1], "", "blank line between sections");
        assert!(
            lines[claude_at + 1].starts_with(" 2  c@x.y"),
            "the card follows its header directly"
        );
        assert!(lines[claude_at + 2].starts_with("    5h"));
        assert_eq!(lines[claude_at + 3], "");
        assert!(
            lines[claude_at + 4].starts_with(" 3  d@x.y"),
            "{}",
            lines[claude_at + 4]
        );
        assert_eq!(
            section_header(Provider::Claude, p).spans[0].content,
            "claude"
        );

        let single = crate::tui::test_support::snapshot(
            vec![account(1, "a@x.y", true, entry(Some(900.0), Some(10.0)))],
            1000.0,
        );
        let lines = accounts_panel(Some(&single), 90, None, true, 1000.0, p);
        assert!(
            text(&lines[0]).starts_with(" 1  a@x.y"),
            "no header for one provider: {}",
            text(&lines[0])
        );
    }

    #[test]
    fn mixed_panel_without_minis_skips_sections_with_no_rows() {
        use crate::tui::test_support::claude_account;
        let p = &DARK;
        let mut snap = crate::tui::test_support::snapshot(
            vec![
                account(1, "a@x.y", true, entry(Some(900.0), Some(10.0))),
                claude_account(2, "c@x.y", false, entry(Some(900.0), Some(20.0))),
            ],
            1000.0,
        );
        let lines = accounts_panel(Some(&snap), 90, None, false, 1000.0, p);
        let lines: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(lines[0], "codex");
        assert!(lines[1].starts_with(" 1  a@x.y"));
        assert!(lines.iter().all(|l| l != "claude"), "{lines:?}");
        assert_ne!(lines.last().unwrap(), "", "no trailing blank");

        snap.accounts[0].is_active = false;
        let lines = accounts_panel(Some(&snap), 90, None, false, 1000.0, p);
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "no active managed login");
    }

    #[test]
    fn empty_state_mentions_both_tools() {
        let empty = crate::tui::snapshot::AccountsSnapshot::empty(1.0);
        let lines = accounts_panel(Some(&empty), 90, None, true, 1.0, &DARK);
        assert_eq!(
            text(&lines[1]),
            "Use the menu below: Add account — from your current Codex or Claude Code login, or from a token."
        );
    }

    #[test]
    fn wrapping() {
        assert_eq!(wrap_text("a bb ccc", 5), vec!["a bb", "ccc"]);
        assert_eq!(wrap_text("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap_text("x\ny", 3), vec!["x", "y"]);
        assert_eq!(wrap_count("", 10), 1);
    }

    #[test]
    fn wrap_line_breaks_at_spaces_keeps_styles_and_folds_long_words() {
        let p = &DARK;
        let line = Line::from(vec![
            Span::raw("    5h "),
            Span::styled("━━━━━━━━", p.warn_style()),
            Span::styled(" 76%", p.warn_style()),
            Span::styled("  resets 2h 47m  (ahead of pace)", p.muted_style()),
        ]);
        let rows = wrap_line(line.clone(), 30);
        let texts: Vec<String> = rows.iter().map(text).collect();
        assert_eq!(
            texts,
            ["    5h ━━━━━━━━ 76%  resets 2h", "47m  (ahead of pace)"]
        );
        assert_eq!(
            rows[0].spans[0].content, "    5h ",
            "the first row keeps its indent"
        );
        assert_eq!(
            rows[1].spans[0].style,
            p.muted_style(),
            "a span split across rows keeps its style"
        );
        assert_eq!(wrap_line(line, 100).len(), 1, "nothing to wrap");
        let folded: Vec<String> = wrap_line(Line::from("abcdefgh"), 3)
            .iter()
            .map(text)
            .collect();
        assert_eq!(folded, ["abc", "def", "gh"]);
        assert_eq!(wrap_line(Line::default(), 10).len(), 1);
    }

    #[test]
    fn narrow_cards_and_minis_wrap_like_cswap() {
        let p = &DARK;
        let now = 1_790_000_000.0;
        let mut acc = account(
            2,
            "john.doe@gmail.com",
            true,
            entry(Some(now - 400.0), None),
        );
        acc.tag = "Personal".into();
        acc.usage.age_s = Some(400.0);
        acc.usage.last_good = Some(NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: 76.0,
                resets_at: Some(format_iso(now as i64 + 2 * 3600 + 47 * 60)),
            }),
            reset_credits: Some(2),
            ..NormalizedUsage::default()
        });
        let texts: Vec<String> = account_card(&acc, 36, None, now, p)
            .iter()
            .map(text)
            .collect();
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert_eq!(texts[0], " 2  john.doe@gmail.com  [Personal]");
        assert_eq!(texts[1], "● active   · 6m ago   ♥ 2");
        assert!(texts[2].ends_with("  76%  resets 2h"), "{}", texts[2]);
        assert_eq!(texts[3], "47m");

        let mut expired = account(5, "expired@x.y", false, entry(Some(now - 720.0), None));
        expired.usage.sentinel = Some(UsageSentinel::TokenExpired);
        let snap = crate::tui::test_support::snapshot(vec![acc, expired], now);
        let texts: Vec<String> = accounts_panel(Some(&snap), 52, None, true, now, p)
            .iter()
            .map(text)
            .collect();
        let i = texts
            .iter()
            .position(|t| t.starts_with(" 5  expired@x.y"))
            .unwrap();
        assert_eq!(texts[i], " 5  expired@x.y  [personal]   token expired —");
        assert_eq!(
            texts[i + 1],
            "refresh deferred this pass; retries automatically"
        );
    }
}
