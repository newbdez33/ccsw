//! Time helpers and display rows shared by the screens (research notes
//! `cswap-tui.md` §3.2–§3.4). Everything takes `now` so the tests are clock-free.

use crate::model::{Credits, NormalizedUsage};
use crate::printer::{countdown_and_clock, format_age, format_duration};
use crate::store::poll_policy::SERVE_TTL_S;
use crate::store::usage_store::{STALE_OK_S, UsageEntry, UsageSentinel};
use crate::usage_math::{binding_pct, parse_reset, usage_rows};

/// `· 6m ago` once the measurement is older than the serve TTL (180 s).
pub fn age_note(age_s: Option<f64>) -> Option<String> {
    let age = age_s?;
    (age >= SERVE_TTL_S).then(|| format!("· {} ago", format_duration(age)))
}

/// Bars and percentages dim once the measurement is older than 300 s.
pub fn is_stale(entry: &UsageEntry) -> bool {
    entry.age_s.is_some_and(|age| age > STALE_OK_S)
}

/// `resets now` / `resets 2h 13m`, computed live from the reset instant.
pub fn reset_text(resets_at: Option<i64>, now: i64) -> Option<String> {
    let reset = resets_at?;
    let remaining = reset - now;
    Some(if remaining <= 0 {
        "resets now".to_string()
    } else {
        format!("resets {}", format_duration(remaining as f64))
    })
}

/// Local `20:39` (today) or `Jul 5 08:59`; `None` once the reset has passed.
pub fn reset_clock(resets_at: Option<i64>, now: i64) -> Option<String> {
    let reset = resets_at?;
    if reset <= now {
        return None;
    }
    let (_, clock) = countdown_and_clock(reset, now);
    (!clock.is_empty()).then_some(clock)
}

/// `last seen 53% used · 12m ago` from the last good 5h/7d measurement.
pub fn last_seen_note(entry: &UsageEntry, now: f64) -> Option<String> {
    let last = entry.last_good.as_ref()?;
    let fetched_at = entry.fetched_at?;
    let pct = binding_pct(last, &[])?;
    Some(format!(
        "last seen {pct:.0}% used · {}",
        format_age(now - fetched_at)
    ))
}

/// The wording of a sentinel; `API key (no quota)` for API-key accounts.
pub fn sentinel_label(sentinel: UsageSentinel) -> &'static str {
    sentinel.label()
}

/// `credits $12.50` / `credits unlimited`; `None` when nothing is known.
pub fn credits_text(credits: Option<&Credits>) -> Option<String> {
    let credits = credits?;
    match (credits.balance, credits.unlimited) {
        (Some(balance), _) => Some(format!("credits ${balance:.2}")),
        (None, true) => Some("credits unlimited".to_string()),
        (None, false) => None,
    }
}

/// `♥ 2` / `♥ 2 (in 10d)`: the rate-limit reset cards left and, when known,
/// how long the soonest-ending grant stays spendable; `None` when there are
/// none or the API did not say.
pub fn reset_cards_text(count: Option<u32>, ends_at: Option<i64>, now: i64) -> Option<String> {
    let count = count.filter(|count| *count > 0)?;
    Some(match reset_cards_hint(ends_at, now) {
        Some(hint) => format!("♥ {count} ({hint})"),
        None => format!("♥ {count}"),
    })
}

/// `in 10d` (whole days from a day out), `in 5h 12m` under that, `expired`
/// once a stale measurement outlives the grant; `None` without an end.
pub fn reset_cards_hint(ends_at: Option<i64>, now: i64) -> Option<String> {
    let remaining = ends_at? - now;
    Some(if remaining <= 0 {
        "expired".to_string()
    } else if remaining >= 86_400 {
        format!("in {}d", remaining / 86_400)
    } else {
        format!("in {}", format_duration(remaining as f64))
    })
}

/// One bar row: `suffix` is the short form, `suffix_full` adds the reset clock.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayRow {
    pub label: String,
    pub pct: f64,
    pub suffix: String,
    pub suffix_full: String,
    /// The `(!)`-marked scoped pool at 100 %.
    pub maxed_pool: bool,
    pub ahead: bool,
    pub resets_at: Option<i64>,
}

fn join(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("  ")
}

/// `5h`, `7d`, then each scoped pool by name (§3.4). `(!)` replaces the pace
/// marker on a maxed pool; weekly rows may carry `(ahead of pace)`.
pub fn display_rows(usage: &NormalizedUsage, fetched_at: Option<f64>, now: i64) -> Vec<DisplayRow> {
    usage_rows(usage, fetched_at)
        .into_iter()
        .map(|row| {
            let scoped = row.label != "5h" && row.label != "7d";
            let maxed_pool = scoped && row.pct >= 100.0;
            let reset = reset_text(row.resets_at, now).unwrap_or_default();
            let reset_full = match reset_clock(row.resets_at, now) {
                Some(clock) if !reset.is_empty() => format!("{reset} · {clock}"),
                _ => reset.clone(),
            };
            let marker = if maxed_pool {
                "(!)".to_string()
            } else if row.ahead_of_pace {
                "(ahead of pace)".to_string()
            } else {
                String::new()
            };
            DisplayRow {
                label: row.label,
                pct: row.pct,
                suffix: join(&[reset, marker.clone()]),
                suffix_full: join(&[reset_full, marker]),
                maxed_pool,
                ahead: row.ahead_of_pace,
                resets_at: row.resets_at,
            }
        })
        .collect()
}

/// The `$$` row of a Claude card: pct plus `resets …` and the amounts.
pub fn spend_row(usage: &NormalizedUsage, now: i64) -> Option<DisplayRow> {
    let spend = usage.spend.as_ref()?;
    let resets_at = spend.resets_at.as_deref().and_then(parse_reset);
    let reset = reset_text(resets_at, now).unwrap_or_default();
    let reset_full = match reset_clock(resets_at, now) {
        Some(clock) if !reset.is_empty() => format!("{reset} · {clock}"),
        _ => reset.clone(),
    };
    Some(DisplayRow {
        label: "$$".to_string(),
        pct: spend.pct,
        suffix: join(&[reset, spend.amounts()]),
        suffix_full: join(&[reset_full, spend.amounts()]),
        maxed_pool: false,
        ahead: false,
        resets_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ScopedWindow, WindowUsage, format_iso};

    fn window(pct: f64, reset: Option<i64>) -> WindowUsage {
        WindowUsage {
            pct,
            resets_at: reset.map(format_iso),
        }
    }

    #[test]
    fn age_note_and_staleness() {
        assert_eq!(age_note(None), None);
        assert_eq!(age_note(Some(179.0)), None);
        assert_eq!(age_note(Some(400.0)), Some("· 6m ago".to_string()));
        let mut entry = crate::tui::test_support::entry(None, None);
        assert!(!is_stale(&entry));
        entry.age_s = Some(300.0);
        assert!(!is_stale(&entry));
        entry.age_s = Some(301.0);
        assert!(is_stale(&entry));
    }

    #[test]
    fn reset_texts() {
        let now = 1_790_000_000;
        assert_eq!(reset_text(None, now), None);
        assert_eq!(reset_text(Some(now - 1), now), Some("resets now".into()));
        assert_eq!(
            reset_text(Some(now + 7980), now),
            Some("resets 2h 13m".into())
        );
        assert_eq!(reset_clock(Some(now - 1), now), None);
        assert!(reset_clock(Some(now + 60), now).is_some());
    }

    #[test]
    fn rows_carry_markers_and_clock_variants() {
        let now = 1_790_000_000;
        let fetched_at = now as f64;
        let usage = NormalizedUsage {
            five_hour: Some(window(47.0, Some(now + 7980))),
            seven_day: Some(window(80.0, Some(now + 4 * 86_400))),
            scoped: vec![
                ScopedWindow {
                    name: "Spark".into(),
                    pct: 100.0,
                    resets_at: None,
                },
                ScopedWindow {
                    name: "Mini".into(),
                    pct: 5.0,
                    resets_at: None,
                },
            ],
            ..NormalizedUsage::default()
        };
        let rows = display_rows(&usage, Some(fetched_at), now);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].label, "5h");
        assert_eq!(rows[0].suffix, "resets 2h 13m");
        assert!(rows[0].suffix_full.starts_with("resets 2h 13m · "));
        assert!(rows[1].ahead, "80% three days in is ahead of pace");
        assert!(rows[1].suffix.ends_with("  (ahead of pace)"));
        assert_eq!(rows[2].suffix, "(!)");
        assert!(rows[2].maxed_pool);
        assert_eq!(rows[3].suffix, "");
        assert_eq!(rows[3].suffix_full, "");
    }

    #[test]
    fn last_seen_and_credits() {
        let mut entry = crate::tui::test_support::entry(Some(400.0), Some(53.0));
        assert_eq!(
            last_seen_note(&entry, 1120.0),
            Some("last seen 53% used · 12m ago".into())
        );
        entry.fetched_at = None;
        assert_eq!(last_seen_note(&entry, 1120.0), None);
        assert_eq!(credits_text(None), None);
        assert_eq!(
            credits_text(Some(&Credits {
                balance: Some(12.5),
                unlimited: false
            })),
            Some("credits $12.50".into())
        );
        assert_eq!(
            credits_text(Some(&Credits {
                balance: None,
                unlimited: true
            })),
            Some("credits unlimited".into())
        );
        assert_eq!(sentinel_label(UsageSentinel::ApiKey), "API key (no quota)");
    }

    #[test]
    fn reset_cards_icon() {
        let now = 1_790_000_000;
        assert_eq!(reset_cards_text(None, None, now), None);
        assert_eq!(reset_cards_text(Some(2), None, now), Some("♥ 2".into()));
        assert_eq!(
            reset_cards_text(Some(0), None, now),
            None,
            "nothing left, nothing shown"
        );
        assert_eq!(
            reset_cards_text(Some(0), Some(now + 86_400), now),
            None,
            "no hint without cards"
        );
    }

    #[test]
    fn reset_cards_hint_counts_down_to_the_earliest_end() {
        let now = 1_790_000_000;
        assert_eq!(reset_cards_hint(None, now), None);
        assert_eq!(
            reset_cards_hint(Some(now + 10 * 86_400 + 4 * 3600), now),
            Some("in 10d".into()),
            "whole days once a day or more remains"
        );
        assert_eq!(
            reset_cards_hint(Some(now + 5 * 3600 + 12 * 60), now),
            Some("in 5h 12m".into())
        );
        assert_eq!(
            reset_cards_hint(Some(now - 1), now),
            Some("expired".into()),
            "a stale measurement past the end"
        );
        assert_eq!(
            reset_cards_text(Some(2), Some(now + 10 * 86_400), now),
            Some("♥ 2 (in 10d)".into())
        );
    }

    #[test]
    fn spend_row_text() {
        let now = 1_790_000_000;
        let usage = NormalizedUsage {
            spend: Some(crate::model::Spend {
                used: 12.5,
                limit: 50.0,
                pct: 25.0,
                currency: "USD".into(),
                resets_at: Some(format_iso(now + 7980)),
            }),
            ..NormalizedUsage::default()
        };
        let row = spend_row(&usage, now).unwrap();
        assert_eq!(row.label, "$$");
        assert_eq!(row.pct, 25.0);
        assert_eq!(row.suffix, "resets 2h 13m  $12.50 / $50.00");
        assert!(row.suffix_full.starts_with("resets 2h 13m · "));
        assert!(row.suffix_full.ends_with("  $12.50 / $50.00"));
        assert_eq!(spend_row(&NormalizedUsage::default(), now), None);
    }
}
