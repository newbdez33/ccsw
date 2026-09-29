//! Adaptive usage polling plan (research notes `cswap-model-autoswitch.md` §5).
//!
//! The usage endpoint allows roughly thirty requests per rolling hour per
//! account, so every surface inherits one plan per row instead of polling on
//! its own schedule.

use crate::model::NormalizedUsage;
use crate::usage_math::{at_limit, binding_pct, earliest_future_reset_ts, limiting_reset_ts};

/// Entries younger than this are served without any fetch.
pub const SERVE_TTL_S: f64 = 180.0;
pub const MIN_INTERVAL_S: f64 = 180.0;
pub const URGENT_INTERVAL_S: f64 = 60.0;
pub const ACTIVE_MAX_INTERVAL_S: f64 = 300.0;
pub const CANDIDATE_DEFAULT_INTERVAL_S: f64 = 300.0;
pub const CANDIDATE_MAX_INTERVAL_S: f64 = 600.0;
pub const EXHAUSTED_INTERVAL_S: f64 = 600.0;
pub const MOVEMENT_DELTA_PCT: f64 = 1.0;
pub const JITTER_FRAC: f64 = 0.1;
pub const EDGE_BACKOFF_S: f64 = 300.0;
pub const POST_429_MIN_INTERVAL_S: f64 = 360.0;
pub const RECENT_429_WINDOW_S: f64 = 3600.0;
pub const POST_429_BACKOFF_MULT: f64 = 1.5;
pub const POST_429_MAX_INTERVAL_S: f64 = 1800.0;
pub const ESCALATION_MARGIN_PCT: f64 = 15.0;
pub const RESET_SLACK_S: f64 = 60.0;

/// The plan after a successful fetch: `(next_poll_at, interval_s)`.
///
/// `rng` yields a value in `[0, 1)` and drives the ±10 % jitter; a constant
/// `0.5` means no jitter.
// The argument list mirrors the reference implementation; a struct would only
// rename the same nine inputs.
#[allow(clippy::too_many_arguments)]
pub fn plan_after_fetch(
    prev_interval: Option<f64>,
    prev_usage: Option<&NormalizedUsage>,
    new_usage: &NormalizedUsage,
    is_active: bool,
    threshold: f64,
    models: &[String],
    recent_429: bool,
    now: f64,
    mut rng: impl FnMut() -> f64,
) -> (f64, f64) {
    let (default, ceiling) = if is_active {
        (MIN_INTERVAL_S, ACTIVE_MAX_INTERVAL_S)
    } else {
        (CANDIDATE_DEFAULT_INTERVAL_S, CANDIDATE_MAX_INTERVAL_S)
    };
    let base = prev_interval.filter(|i| *i > 0.0).unwrap_or(default);

    let prev_pct = prev_usage.and_then(|usage| binding_pct(usage, models));
    let new_pct = binding_pct(new_usage, models);
    let (mut interval, moving) = match (prev_pct, new_pct) {
        (Some(prev), Some(new)) if (new - prev).abs() >= MOVEMENT_DELTA_PCT => {
            (MIN_INTERVAL_S.max(base / 2.0), true)
        }
        (Some(_), Some(_)) => (ceiling.min(MIN_INTERVAL_S.max(base * 1.5)), false),
        _ => (default, false),
    };

    if is_active
        && moving
        && !recent_429
        && new_pct.is_some_and(|pct| pct >= threshold - ESCALATION_MARGIN_PCT)
    {
        interval = URGENT_INTERVAL_S;
    }
    if recent_429 {
        let increased = (base * POST_429_BACKOFF_MULT).max(POST_429_MIN_INTERVAL_S);
        interval = POST_429_MAX_INTERVAL_S.min(interval.max(increased));
    }
    let limited = at_limit(new_usage, models);
    if limited {
        interval = interval.max(EXHAUSTED_INTERVAL_S);
    }

    let mut next_poll = now + interval * (1.0 + JITTER_FRAC * (2.0 * rng() - 1.0));
    let limiting_reset = limiting_reset_ts(new_usage, models)
        .map(|ts| ts as f64)
        .filter(|ts| *ts > now);
    let clamp = match (limited, limiting_reset) {
        (true, Some(reset)) => Some(reset),
        _ => earliest_future_reset_ts(new_usage, now as i64, models).map(|ts| ts as f64),
    };
    if let Some(reset) = clamp {
        next_poll = next_poll.min(reset + RESET_SLACK_S);
    }
    (next_poll, interval)
}

/// After a switch, pull the new active account's plan forward: `(next_poll_at, 180)`
/// where the poll happens as soon as the last measurement turns stale.
pub fn replan_new_active(fetched_at: f64, now: f64) -> (f64, f64) {
    (now.max(fetched_at + SERVE_TTL_S), MIN_INTERVAL_S)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ScopedWindow, WindowUsage, format_iso};

    const NOW: f64 = 1_000_000.0;

    fn usage(five_hour: f64, seven_day: f64) -> NormalizedUsage {
        NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: five_hour,
                resets_at: None,
            }),
            seven_day: Some(WindowUsage {
                pct: seven_day,
                resets_at: None,
            }),
            ..NormalizedUsage::default()
        }
    }

    fn no_jitter() -> f64 {
        0.5
    }

    fn plan(
        prev_interval: Option<f64>,
        prev: Option<&NormalizedUsage>,
        new: &NormalizedUsage,
        is_active: bool,
        recent_429: bool,
    ) -> (f64, f64) {
        plan_after_fetch(
            prev_interval,
            prev,
            new,
            is_active,
            90.0,
            &[],
            recent_429,
            NOW,
            no_jitter,
        )
    }

    #[test]
    fn first_fetch_uses_the_defaults() {
        let u = usage(10.0, 20.0);
        assert_eq!(plan(None, None, &u, true, false), (NOW + 180.0, 180.0));
        assert_eq!(plan(None, None, &u, false, false), (NOW + 300.0, 300.0));
        assert_eq!(
            plan(Some(450.0), None, &u, false, false).1,
            300.0,
            "unknown previous pct is not movement"
        );
    }

    #[test]
    fn unmoved_backs_off_toward_the_ceiling() {
        let prev = usage(40.0, 20.0);
        let same = usage(40.5, 20.0);
        assert_eq!(plan(Some(300.0), Some(&prev), &same, false, false).1, 450.0);
        assert_eq!(plan(Some(500.0), Some(&prev), &same, false, false).1, 600.0);
        assert_eq!(plan(Some(250.0), Some(&prev), &same, true, false).1, 300.0);
        assert_eq!(
            plan(Some(60.0), Some(&prev), &same, true, false).1,
            180.0,
            "an urgent base snaps back to the minimum"
        );
    }

    #[test]
    fn movement_halves_the_interval() {
        let prev = usage(40.0, 20.0);
        let moved = usage(41.0, 20.0);
        assert_eq!(
            plan(Some(600.0), Some(&prev), &moved, false, false).1,
            300.0
        );
        assert_eq!(
            plan(Some(200.0), Some(&prev), &moved, false, false).1,
            180.0
        );
        assert_eq!(
            plan(Some(600.0), Some(&prev), &moved, true, false).1,
            300.0,
            "far from the threshold, active movement is not urgent"
        );
    }

    #[test]
    fn urgent_near_the_threshold_unless_recently_throttled() {
        let prev = usage(78.0, 20.0);
        let new = usage(82.0, 20.0);
        assert_eq!(plan(Some(180.0), Some(&prev), &new, true, false).1, 60.0);
        assert_eq!(plan(Some(180.0), Some(&prev), &new, true, true).1, 360.0);
        assert_eq!(
            plan(Some(180.0), Some(&prev), &new, false, false).1,
            180.0,
            "candidates are never urgent"
        );
    }

    #[test]
    fn recent_429_is_additive_increase_with_a_cap() {
        let prev = usage(40.0, 20.0);
        let same = usage(40.0, 20.0);
        assert_eq!(
            plan(Some(1000.0), Some(&prev), &same, false, true).1,
            1500.0
        );
        assert_eq!(
            plan(Some(1500.0), Some(&prev), &same, false, true).1,
            1800.0
        );
    }

    #[test]
    fn at_limit_parks_until_the_reset() {
        let prev = usage(40.0, 20.0);
        let mut maxed = usage(100.0, 20.0);
        let (next, interval) = plan(Some(180.0), Some(&prev), &maxed, true, false);
        assert_eq!(interval, 600.0);
        assert_eq!(next, NOW + 600.0);

        maxed.five_hour.as_mut().unwrap().resets_at = Some(format_iso(1_000_200));
        let (next, _) = plan(Some(180.0), Some(&prev), &maxed, true, false);
        assert_eq!(next, 1_000_260.0, "reset plus slack wins over the interval");

        let mut limited = usage(50.0, 20.0);
        limited.limited = true;
        assert_eq!(plan(None, None, &limited, false, false).1, 600.0);
    }

    #[test]
    fn a_sooner_reset_clamps_a_healthy_plan() {
        let mut u = usage(40.0, 20.0);
        u.seven_day.as_mut().unwrap().resets_at = Some(format_iso(1_000_100));
        let (next, interval) = plan(None, None, &u, false, false);
        assert_eq!(interval, 300.0);
        assert_eq!(next, 1_000_160.0);
        u.seven_day.as_mut().unwrap().resets_at = Some(format_iso(999_000));
        assert_eq!(plan(None, None, &u, false, false).0, NOW + 300.0);
    }

    #[test]
    fn scoped_pools_count_only_when_named() {
        let mut u = usage(10.0, 10.0);
        u.scoped.push(ScopedWindow {
            name: "Spark".into(),
            pct: 100.0,
            resets_at: None,
        });
        assert_eq!(
            plan_after_fetch(None, None, &u, false, 90.0, &[], false, NOW, no_jitter).1,
            300.0
        );
        let models = vec!["spark".to_string()];
        assert_eq!(
            plan_after_fetch(None, None, &u, false, 90.0, &models, false, NOW, no_jitter).1,
            600.0
        );
    }

    #[test]
    fn jitter_stays_within_ten_percent() {
        let u = usage(10.0, 10.0);
        let (low, _) = plan_after_fetch(None, None, &u, true, 90.0, &[], false, NOW, || 0.0);
        let (high, _) = plan_after_fetch(None, None, &u, true, 90.0, &[], false, NOW, || 1.0);
        assert!((low - (NOW + 162.0)).abs() < 1e-9);
        assert!((high - (NOW + 198.0)).abs() < 1e-9);
    }

    #[test]
    fn replan_after_switch() {
        assert_eq!(replan_new_active(NOW - 500.0, NOW), (NOW, 180.0));
        assert_eq!(replan_new_active(NOW - 100.0, NOW), (NOW + 80.0, 180.0));
    }
}
