//! Pure usage arithmetic: relevant windows, headroom, resets, pace.
//!
//! Everything here reads a [`NormalizedUsage`] and never touches the clock or
//! the store, so the auto-switch engine, the CLI rows and the TUI agree on the
//! numbers (research notes `cswap-model-autoswitch.md` §4.2 and §4.10).

use crate::model::{NormalizedUsage, WindowUsage};

pub const WEEKLY_PERIOD_S: f64 = 604_800.0;
/// Pace is not judged during the first day of a weekly window: too little has
/// elapsed for the projection to mean anything.
pub const PACE_SUPPRESS_AFTER_RESET_S: f64 = 86_400.0;
pub const AHEAD_THRESHOLD_PCT: f64 = 15.0;

/// A window that takes part in a decision: `(label, pct, reset unix seconds)`.
pub type RelevantWindow = (String, f64, Option<i64>);

/// RFC 3339 `resets_at` to unix seconds; `None` when it does not parse.
pub fn parse_reset(value: &str) -> Option<i64> {
    crate::model::parse_iso(value)
}

fn reset_ts(resets_at: Option<&str>) -> Option<i64> {
    resets_at.and_then(parse_reset)
}

fn wants_all(models: &[String]) -> bool {
    models.iter().any(|m| m.eq_ignore_ascii_case("all"))
}

/// The windows a decision looks at: always `5h` and `7d` when present, plus the
/// scoped pools named in `models` (case-insensitive; `all` selects every pool).
/// A `limited` account reads 100 on every window, or on a synthetic `5h` when
/// it has none, so its headroom is 0.
pub fn relevant_windows(usage: &NormalizedUsage, models: &[String]) -> Vec<RelevantWindow> {
    let mut windows = Vec::new();
    if let Some(window) = &usage.five_hour {
        windows.push((
            "5h".to_string(),
            window.pct,
            reset_ts(window.resets_at.as_deref()),
        ));
    }
    if let Some(window) = &usage.seven_day {
        windows.push((
            "7d".to_string(),
            window.pct,
            reset_ts(window.resets_at.as_deref()),
        ));
    }
    let all = wants_all(models);
    for pool in &usage.scoped {
        if all || models.iter().any(|m| m.eq_ignore_ascii_case(&pool.name)) {
            windows.push((
                pool.name.clone(),
                pool.pct,
                reset_ts(pool.resets_at.as_deref()),
            ));
        }
    }
    if usage.limited {
        if windows.is_empty() {
            windows.push(("5h".to_string(), 100.0, None));
        }
        for window in &mut windows {
            window.1 = window.1.max(100.0);
        }
    }
    windows
}

/// The highest percentage among the relevant windows; `None` without windows.
pub fn binding_pct(usage: &NormalizedUsage, models: &[String]) -> Option<f64> {
    relevant_windows(usage, models)
        .into_iter()
        .map(|(_, pct, _)| pct)
        .reduce(f64::max)
}

/// `100 - binding_pct`; `None` means unknown, which callers must never treat as
/// "no room".
pub fn headroom(usage: &NormalizedUsage, models: &[String]) -> Option<f64> {
    binding_pct(usage, models).map(|pct| 100.0 - pct)
}

pub fn at_limit(usage: &NormalizedUsage, models: &[String]) -> bool {
    headroom(usage, models).is_some_and(|h| h <= 0.0)
}

/// When the account is usable again: the latest reset among windows at or over 100.
pub fn limiting_reset_ts(usage: &NormalizedUsage, models: &[String]) -> Option<i64> {
    relevant_windows(usage, models)
        .into_iter()
        .filter(|(_, pct, _)| *pct >= 100.0)
        .filter_map(|(_, _, reset)| reset)
        .max()
}

/// The soonest reset after `now` over all relevant windows.
pub fn earliest_future_reset_ts(
    usage: &NormalizedUsage,
    now: i64,
    models: &[String],
) -> Option<i64> {
    relevant_windows(usage, models)
        .into_iter()
        .filter_map(|(_, _, reset)| reset)
        .filter(|reset| *reset > now)
        .min()
}

/// The weekly window's reset when it lies after `now`.
pub fn seven_day_reset_ts(usage: &NormalizedUsage, now: i64) -> Option<i64> {
    reset_ts(usage.seven_day.as_ref()?.resets_at.as_deref()).filter(|reset| *reset > now)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaceResult {
    pub expected_pct: f64,
    pub actual_pct: f64,
    pub elapsed_s: f64,
    pub period_s: f64,
    pub ahead: bool,
}

/// Pace of a weekly window as of `fetched_at` (never the wall clock, so stale
/// data is judged at its measurement time). The window start is `resets_at`
/// rolled back by whole periods; `None` without a reset or inside the first day.
pub fn compute_pace(window: &WindowUsage, fetched_at: f64) -> Option<PaceResult> {
    let next_reset = reset_ts(window.resets_at.as_deref())? as f64;
    let remaining = (next_reset - fetched_at).rem_euclid(WEEKLY_PERIOD_S);
    let elapsed_s = if remaining == 0.0 {
        0.0
    } else {
        WEEKLY_PERIOD_S - remaining
    };
    if elapsed_s < PACE_SUPPRESS_AFTER_RESET_S {
        return None;
    }
    let expected_pct = (elapsed_s / WEEKLY_PERIOD_S * 100.0).min(100.0);
    Some(PaceResult {
        expected_pct,
        actual_pct: window.pct,
        elapsed_s,
        period_s: WEEKLY_PERIOD_S,
        ahead: window.pct - expected_pct >= AHEAD_THRESHOLD_PCT,
    })
}

/// When the window hits 100 at the measured rate; `None` before any usage.
pub fn projected_exhaustion_ts(pace: &PaceResult, fetched_at: f64) -> Option<f64> {
    if pace.elapsed_s <= 0.0 || pace.actual_pct <= 0.0 {
        return None;
    }
    let rate = pace.actual_pct / pace.elapsed_s;
    let remaining = 100.0 - pace.actual_pct;
    if remaining <= 0.0 {
        return Some(fetched_at);
    }
    Some(fetched_at + remaining / rate)
}

/// Whether the window stays under 100 until its reset at the measured rate.
pub fn will_last_to_reset(pace: &PaceResult) -> Option<bool> {
    if pace.actual_pct <= 0.0 {
        return Some(true);
    }
    if pace.elapsed_s <= 0.0 {
        return None;
    }
    let rate = pace.actual_pct / pace.elapsed_s;
    let projected_total = pace.actual_pct + rate * (pace.period_s - pace.elapsed_s);
    Some(projected_total <= 100.0)
}

/// One display row. Rows exist only for windows the measurement carries; credits
/// are rendered separately by callers.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRow {
    pub label: String,
    pub pct: f64,
    pub resets_at: Option<i64>,
    pub ahead_of_pace: bool,
    pub maxed: bool,
}

/// Rows in display order: `5h`, `7d`, then each scoped pool under its name.
/// Pace applies to the weekly rows only and is dropped once a row is maxed.
pub fn usage_rows(usage: &NormalizedUsage, fetched_at: Option<f64>) -> Vec<UsageRow> {
    let make = |label: &str, window: &WindowUsage, weekly: bool| {
        let maxed = window.pct >= 100.0 || usage.limited;
        let ahead_of_pace = weekly
            && !maxed
            && fetched_at.is_some_and(|at| compute_pace(window, at).is_some_and(|p| p.ahead));
        UsageRow {
            label: label.to_string(),
            pct: window.pct,
            resets_at: reset_ts(window.resets_at.as_deref()),
            ahead_of_pace,
            maxed,
        }
    };
    let mut rows = Vec::new();
    if let Some(window) = &usage.five_hour {
        rows.push(make("5h", window, false));
    }
    if let Some(window) = &usage.seven_day {
        rows.push(make("7d", window, true));
    }
    for pool in &usage.scoped {
        let window = WindowUsage {
            pct: pool.pct,
            resets_at: pool.resets_at.clone(),
        };
        rows.push(make(&pool.name, &window, true));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ScopedWindow, format_iso};

    fn window(pct: f64, reset: Option<i64>) -> WindowUsage {
        WindowUsage {
            pct,
            resets_at: reset.map(format_iso),
        }
    }

    fn pool(name: &str, pct: f64, reset: Option<i64>) -> ScopedWindow {
        ScopedWindow {
            name: name.to_string(),
            pct,
            resets_at: reset.map(format_iso),
        }
    }

    fn models(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn sample() -> NormalizedUsage {
        NormalizedUsage {
            five_hour: Some(window(22.0, Some(1_000_100))),
            seven_day: Some(window(61.0, Some(1_500_000))),
            scoped: vec![
                pool("GPT-5.3-Codex-Spark", 100.0, Some(1_200_000)),
                pool("Other", 5.0, None),
            ],
            ..NormalizedUsage::default()
        }
    }

    #[test]
    fn windows_follow_the_model_filter() {
        let usage = sample();
        let base = relevant_windows(&usage, &[]);
        assert_eq!(
            base,
            vec![
                ("5h".to_string(), 22.0, Some(1_000_100)),
                ("7d".to_string(), 61.0, Some(1_500_000)),
            ]
        );
        let spark = relevant_windows(&usage, &models(&["gpt-5.3-codex-spark"]));
        assert_eq!(spark.len(), 3);
        assert_eq!(
            spark[2],
            ("GPT-5.3-Codex-Spark".to_string(), 100.0, Some(1_200_000))
        );
        let all = relevant_windows(&usage, &models(&["ALL"]));
        assert_eq!(all.len(), 4);
        assert_eq!(all[3], ("Other".to_string(), 5.0, None));
        assert!(relevant_windows(&usage, &models(&["nope"])).len() == 2);
    }

    #[test]
    fn headroom_binding_and_at_limit() {
        let usage = sample();
        assert_eq!(headroom(&usage, &[]), Some(39.0));
        assert_eq!(binding_pct(&usage, &[]), Some(61.0));
        assert!(!at_limit(&usage, &[]));
        assert!(at_limit(&usage, &models(&["all"])));
        assert_eq!(headroom(&usage, &models(&["all"])), Some(0.0));
        assert_eq!(headroom(&NormalizedUsage::default(), &[]), None);
        assert!(!at_limit(&NormalizedUsage::default(), &[]));
    }

    #[test]
    fn limited_reads_100_everywhere() {
        let mut usage = sample();
        usage.limited = true;
        let windows = relevant_windows(&usage, &[]);
        assert!(windows.iter().all(|w| w.1 == 100.0));
        assert_eq!(windows.len(), 2);
        assert_eq!(headroom(&usage, &[]), Some(0.0));

        let bare = NormalizedUsage {
            limited: true,
            ..NormalizedUsage::default()
        };
        assert_eq!(
            relevant_windows(&bare, &[]),
            vec![("5h".to_string(), 100.0, None)]
        );
        assert!(at_limit(&bare, &[]));
    }

    #[test]
    fn reset_lookups() {
        let usage = sample();
        assert_eq!(limiting_reset_ts(&usage, &[]), None);
        assert_eq!(
            limiting_reset_ts(&usage, &models(&["all"])),
            Some(1_200_000)
        );
        let mut both = usage.clone();
        both.five_hour = Some(window(100.0, Some(1_000_100)));
        assert_eq!(
            limiting_reset_ts(&both, &models(&["all"])),
            Some(1_200_000),
            "latest reset among maxed windows"
        );
        assert_eq!(
            earliest_future_reset_ts(&usage, 900_000, &models(&["all"])),
            Some(1_000_100)
        );
        assert_eq!(
            earliest_future_reset_ts(&usage, 1_000_100, &models(&["all"])),
            Some(1_200_000)
        );
        assert_eq!(earliest_future_reset_ts(&usage, 2_000_000, &[]), None);
        assert_eq!(seven_day_reset_ts(&usage, 1_000_000), Some(1_500_000));
        assert_eq!(seven_day_reset_ts(&usage, 1_500_000), None);
        assert_eq!(parse_reset("2026-09-21T14:13:20Z"), Some(1_790_000_000));
        assert_eq!(parse_reset("soon"), None);
    }

    #[test]
    fn pace_rolls_the_reset_back_and_suppresses_the_first_day() {
        let fetched_at = 1_000_000.0;
        // Three days into the window: the reset is four days out.
        let three_days_in = window(60.0, Some(1_000_000 + 4 * 86_400));
        let pace = compute_pace(&three_days_in, fetched_at).unwrap();
        assert!((pace.elapsed_s - 259_200.0).abs() < 1e-6);
        assert!((pace.expected_pct - 42.857_142_857).abs() < 1e-6);
        assert!(pace.ahead);
        assert_eq!(pace.period_s, WEEKLY_PERIOD_S);
        let calm = window(50.0, Some(1_000_000 + 4 * 86_400));
        assert!(!compute_pace(&calm, fetched_at).unwrap().ahead);

        let half_day_in = window(90.0, Some(1_000_000 + 604_800 - 43_200));
        assert!(compute_pace(&half_day_in, fetched_at).is_none());

        // A reset that already passed is rolled forward by whole periods.
        let past = window(70.0, Some(1_000_000 - 300_000));
        let pace = compute_pace(&past, fetched_at).unwrap();
        assert!((pace.elapsed_s - 300_000.0).abs() < 1e-6);

        assert!(compute_pace(&window(10.0, None), fetched_at).is_none());
        assert!(compute_pace(&window(10.0, Some(1_000_000)), fetched_at).is_none());
    }

    #[test]
    fn projections_from_pace() {
        let fetched_at = 1_000_000.0;
        let pace = compute_pace(&window(60.0, Some(1_000_000 + 4 * 86_400)), fetched_at).unwrap();
        let exhaustion = projected_exhaustion_ts(&pace, fetched_at).unwrap();
        assert!((exhaustion - (fetched_at + 172_800.0)).abs() < 1e-3);
        assert_eq!(will_last_to_reset(&pace), Some(false));

        let slow = compute_pace(&window(30.0, Some(1_000_000 + 4 * 86_400)), fetched_at).unwrap();
        assert_eq!(will_last_to_reset(&slow), Some(true));

        let idle = PaceResult {
            actual_pct: 0.0,
            ..pace
        };
        assert_eq!(projected_exhaustion_ts(&idle, fetched_at), None);
        assert_eq!(will_last_to_reset(&idle), Some(true));

        let full = PaceResult {
            actual_pct: 100.0,
            ..pace
        };
        assert_eq!(projected_exhaustion_ts(&full, fetched_at), Some(fetched_at));

        let fresh = PaceResult {
            elapsed_s: 0.0,
            ..pace
        };
        assert_eq!(will_last_to_reset(&fresh), None);
    }

    #[test]
    fn rows_keep_display_order_and_flags() {
        let fetched_at = 1_000_000.0;
        let usage = NormalizedUsage {
            five_hour: Some(window(100.0, Some(1_000_100))),
            seven_day: Some(window(60.0, Some(1_000_000 + 4 * 86_400))),
            scoped: vec![
                pool("Spark", 100.0, Some(1_000_000 + 4 * 86_400)),
                pool("Mini", 5.0, None),
            ],
            ..NormalizedUsage::default()
        };
        let rows = usage_rows(&usage, Some(fetched_at));
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["5h", "7d", "Spark", "Mini"]);
        assert!(rows[0].maxed && !rows[0].ahead_of_pace);
        assert_eq!(rows[0].resets_at, Some(1_000_100));
        assert!(rows[1].ahead_of_pace && !rows[1].maxed);
        assert!(
            rows[2].maxed && !rows[2].ahead_of_pace,
            "maxed rows drop the pace marker"
        );
        assert!(!rows[3].ahead_of_pace && rows[3].resets_at.is_none());

        assert!(usage_rows(&usage, None).iter().all(|r| !r.ahead_of_pace));
        assert!(usage_rows(&NormalizedUsage::default(), None).is_empty());

        let limited = NormalizedUsage {
            five_hour: Some(window(10.0, None)),
            limited: true,
            ..NormalizedUsage::default()
        };
        assert!(usage_rows(&limited, None)[0].maxed);
    }
}
