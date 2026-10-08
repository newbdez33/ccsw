//! Auto-switch engine, events, loop, and the `ccsw auto` front end (spec §9;
//! research notes `cswap-model-autoswitch.md` §6 and `cswap-cli-contract.md` §10).
//!
//! The engine talks to the switcher through [`AutoFacade`] so it can be driven
//! by a fake in tests, and it never fetches usage itself: every measurement
//! comes from [`crate::collect::run_pass`], the same pass `list` takes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use serde_json::{Map, Value, json};

use crate::claude::keychain::{SecurityCli, SystemSecurity};
use crate::collect::{self, CollectMode, CollectOptions, Collected, RefreshStatus};
use crate::errors::Result;
use crate::model::{AccountRef, CurrentAccount, Roster, format_iso, now_iso, now_unix};
use crate::printer;
use crate::provider::Provider;
use crate::store::poll_policy::{
    ESCALATION_MARGIN_PCT, EXHAUSTED_INTERVAL_S, RESET_SLACK_S, URGENT_INTERVAL_S,
};
use crate::store::state::{self, AutoSwitchState, QuarantineEntry};
use crate::store::usage_store::{UsageEntry, UsageSentinel};
use crate::store::{AutoSwitchOverrides, AutoSwitchSettings, Settings, Store, credentials};
use crate::usage_math::{limiting_reset_ts, relevant_windows, seven_day_reset_ts};

/// Cap on a blocked sleep toward a known reset.
pub const MAX_SLEEP_S: f64 = EXHAUSTED_INTERVAL_S;
/// Blocked / idle-hold cadence when no reset is known.
pub const NO_RESET_FALLBACK_S: f64 = 300.0;
/// Longest idle-hold on a `token expired` active account before unhealthy counting resumes.
pub const IDLE_HOLD_MAX_S: f64 = 1800.0;

/// Why `auto` is Claude-only (spec §9): Codex sessions keep the account they
/// started with until they restart, so switching under them achieves nothing.
pub const CLAUDE_ONLY_NOTICE: &str = "Auto-switch covers Claude Code accounts only: Codex sessions do not pick up a switched account without a restart. Add a Claude Code account with 'ccsw add claude' first.";

/// What the engine needs from the switcher.
pub trait AutoFacade {
    fn store(&self) -> &Store;
    fn roster(&mut self) -> Result<Roster>;
    fn current_account(&mut self, provider: Provider) -> Result<CurrentAccount>;
    fn switch_to(&mut self, slot: u32) -> Result<crate::model::SwitchOutcome>;
}

/// Why a switch was wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Proactive,
    AtLimit,
    Failover,
    ConsumeFirst,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proactive => "proactive",
            Self::AtLimit => "at-limit",
            Self::Failover => "failover",
            Self::ConsumeFirst => "consume-first",
        }
    }

    /// Cooldown applies to the nudges, never to a limit or a failover.
    fn honors_cooldown(self) -> bool {
        matches!(self, Self::Proactive | Self::ConsumeFirst)
    }
}

/// One engine event; `to_json` is the JSONL line, `human` the log line.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Poll {
        active: Option<AccountRef>,
        /// Headroom per slot (active and candidates); `None` = unknown.
        headroom: BTreeMap<u32, Option<f64>>,
        threshold: f64,
        /// `lastError` of slots whose decision value is unknown.
        fetch_errors: BTreeMap<u32, String>,
        /// Relevant windows `(label, pct)` per slot with a decision value.
        windows: BTreeMap<u32, Vec<(String, f64)>>,
    },
    Switch {
        trigger: Trigger,
        from: Option<AccountRef>,
        to: Option<AccountRef>,
        warnings: Vec<String>,
        dry_run: bool,
    },
    NoSwitch {
        reason: String,
        detail: String,
    },
    AccountQuarantined {
        number: u32,
        email: String,
        reason: String,
    },
    AccountUnquarantined {
        number: u32,
        email: String,
        reason: String,
    },
    AllExhausted {
        earliest_reset_at: Option<String>,
    },
    Sleep {
        seconds: f64,
        until: String,
    },
    Error {
        message: String,
        transient: bool,
    },
    ConfigWarning {
        message: String,
    },
}

/// `%.10g`: whole numbers without decimals, otherwise the shortest spelling.
pub fn pct_label(value: f64) -> String {
    let rounded = (value * 1e8).round() / 1e8;
    format!("{rounded}")
}

fn account_label(reference: &AccountRef) -> String {
    match reference.number {
        Some(n) => format!("Account-{n}"),
        None => "(none)".to_string(),
    }
}

impl Event {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Poll { .. } => "poll",
            Self::Switch { .. } => "switch",
            Self::NoSwitch { .. } => "no-switch",
            Self::AccountQuarantined { .. } => "account-quarantined",
            Self::AccountUnquarantined { .. } => "account-unquarantined",
            Self::AllExhausted { .. } => "all-exhausted",
            Self::Sleep { .. } => "sleep",
            Self::Error { .. } => "error",
            Self::ConfigWarning { .. } => "config-warning",
        }
    }

    /// `{"schemaVersion": 2, "event": kind, "ts": now, "provider": provider, …}`.
    pub fn to_json(&self, provider: Provider) -> Value {
        let mut map = Map::new();
        map.insert("schemaVersion".into(), json!(2));
        map.insert("event".into(), json!(self.kind()));
        map.insert("ts".into(), json!(now_iso()));
        map.insert("provider".into(), json!(provider.as_str()));
        match self {
            Self::Poll {
                active,
                headroom,
                threshold,
                fetch_errors,
                windows,
            } => {
                map.insert("active".into(), json!(active));
                let headroom: Map<String, Value> = headroom
                    .iter()
                    .map(|(slot, h)| (slot.to_string(), json!(h)))
                    .collect();
                map.insert("headroomPct".into(), Value::Object(headroom));
                map.insert("threshold".into(), json!(threshold));
                if !fetch_errors.is_empty() {
                    let errors: Map<String, Value> = fetch_errors
                        .iter()
                        .map(|(slot, err)| (slot.to_string(), json!(err)))
                        .collect();
                    map.insert("fetchErrors".into(), Value::Object(errors));
                }
                if !windows.is_empty() {
                    let windows: Map<String, Value> = windows
                        .iter()
                        .map(|(slot, rows)| {
                            let row: Map<String, Value> = rows
                                .iter()
                                .map(|(label, pct)| (label.clone(), json!(pct)))
                                .collect();
                            (slot.to_string(), Value::Object(row))
                        })
                        .collect();
                    map.insert("windowsPct".into(), Value::Object(windows));
                }
            }
            Self::Switch {
                trigger,
                from,
                to,
                warnings,
                dry_run,
            } => {
                map.insert("trigger".into(), json!(trigger.as_str()));
                map.insert("from".into(), json!(from));
                map.insert("to".into(), json!(to));
                map.insert("warnings".into(), json!(warnings));
                map.insert("dryRun".into(), json!(dry_run));
            }
            Self::NoSwitch { reason, detail } => {
                map.insert("reason".into(), json!(reason));
                map.insert("detail".into(), json!(detail));
            }
            Self::AccountQuarantined {
                number,
                email,
                reason,
            }
            | Self::AccountUnquarantined {
                number,
                email,
                reason,
            } => {
                map.insert("number".into(), json!(number.to_string()));
                map.insert("email".into(), json!(email));
                map.insert("reason".into(), json!(reason));
            }
            Self::AllExhausted { earliest_reset_at } => {
                map.insert("earliestResetAt".into(), json!(earliest_reset_at));
            }
            Self::Sleep { seconds, until } => {
                map.insert("seconds".into(), json!((seconds * 10.0).round() / 10.0));
                map.insert("until".into(), json!(until));
            }
            Self::Error { message, transient } => {
                map.insert("message".into(), json!(message));
                map.insert("transient".into(), json!(transient));
            }
            Self::ConfigWarning { message } => {
                map.insert("message".into(), json!(message));
            }
        }
        Value::Object(map)
    }

    pub fn human(&self) -> String {
        match self {
            Self::Poll {
                active,
                headroom,
                threshold,
                fetch_errors,
                windows,
            } => {
                let Some(active) = active else {
                    return "poll: no active account".to_string();
                };
                let slot = active.number.unwrap_or(0);
                let mut line = format!("{} ({}): ", account_label(active), active.email);
                match headroom.get(&slot).copied().flatten() {
                    Some(h) => line.push_str(&format!("{}% used", pct_label(100.0 - h))),
                    None => {
                        line.push_str("usage unknown");
                        if let Some(err) = fetch_errors.get(&slot) {
                            line.push_str(&format!(" ({err})"));
                        }
                    }
                }
                line.push_str(&format!(" (switch at {}%)", pct_label(*threshold)));
                let others: Vec<String> = headroom
                    .iter()
                    .filter(|(other, _)| **other != slot)
                    .map(|(other, h)| {
                        let desc = match (windows.get(other), h) {
                            (Some(rows), _) if !rows.is_empty() => rows
                                .iter()
                                .map(|(label, pct)| format!("{label} {pct:.0}%"))
                                .collect::<Vec<_>>()
                                .join(" · "),
                            (_, Some(h)) => format!("{}%", pct_label(100.0 - h)),
                            (_, None) => match fetch_errors.get(other) {
                                Some(err) => format!("? ({err})"),
                                None => "?".to_string(),
                            },
                        };
                        format!("#{other}: {desc}")
                    })
                    .collect();
                if !others.is_empty() {
                    line.push_str(&format!(" | others: {}", others.join(", ")));
                }
                line
            }
            Self::Switch {
                trigger,
                from,
                to,
                dry_run,
                ..
            } => {
                let verb = if *dry_run {
                    "[dry-run] would switch"
                } else {
                    "Switched"
                };
                let src = from.as_ref().map_or("(none)".to_string(), account_label);
                let dst = to.as_ref().map_or("?".to_string(), |t| {
                    format!("{} ({})", account_label(t), t.email)
                });
                format!("{verb} {src} -> {dst} ({})", trigger.as_str())
            }
            Self::NoSwitch { reason, detail } => {
                if detail.is_empty() {
                    format!("no switch: {reason}")
                } else {
                    format!("no switch: {reason} ({detail})")
                }
            }
            Self::AccountQuarantined {
                number,
                email,
                reason,
            } => format!(
                "Account-{number} ({email}) quarantined: {reason}. Log in with it and run 'ccsw add --slot {number}' to recover."
            ),
            Self::AccountUnquarantined {
                number,
                email,
                reason,
            } => format!("Account-{number} ({email}) back in rotation ({reason})"),
            Self::AllExhausted { earliest_reset_at } => match earliest_reset_at {
                Some(ts) => format!("all accounts exhausted; earliest reset {ts}"),
                None => "all accounts exhausted; no reset time known".to_string(),
            },
            Self::Sleep { seconds, until } => {
                format!("sleeping {:.0}m (until {until})", seconds / 60.0)
            }
            Self::Error { message, transient } => {
                if *transient {
                    format!("error: {message} (will retry)")
                } else {
                    format!("error: {message}")
                }
            }
            Self::ConfigWarning { message } => format!("warning: {message}"),
        }
    }
}

/// `--once` exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum TickOutcome {
    Switched = 0,
    Error = 1,
    NoAction = 2,
    Blocked = 3,
}

impl TickOutcome {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// The sentinels under which the active account is held rather than counted
/// unhealthy: an expired token Claude Code refreshes on its next use, or a
/// Keychain that cannot be read right now.
fn held_sentinel(entry: Option<&UsageEntry>) -> Option<UsageSentinel> {
    match entry.and_then(|e| e.sentinel) {
        Some(s @ (UsageSentinel::TokenExpired | UsageSentinel::KeychainUnavailable)) => Some(s),
        _ => None,
    }
}

pub struct Engine<'a> {
    facade: &'a mut dyn AutoFacade,
    security: &'a dyn SecurityCli,
    provider: Provider,
    settings: AutoSwitchSettings,
    models: Vec<String>,
    model_warning_pending: bool,
    dry_run: bool,
    sink: Box<dyn FnMut(&Event) + 'a>,
    clock: Box<dyn Fn() -> f64 + 'a>,
    unhealthy_ticks: u32,
    idle_hold_since: Option<f64>,
    // Per-tick loop hints.
    sleep_until: Option<f64>,
    blocked_wait_long: bool,
    idle_hold_slow: bool,
    active_next_poll_at: Option<f64>,
}

/// Where the ranking left the tick.
struct Ranked {
    ordered: Vec<u32>,
    any_known: bool,
    active_reset_ts: Option<i64>,
}

impl<'a> Engine<'a> {
    pub fn new(
        facade: &'a mut dyn AutoFacade,
        provider: Provider,
        settings: AutoSwitchSettings,
        dry_run: bool,
        sink: impl FnMut(&Event) + 'a,
    ) -> Self {
        let models = settings.model_names();
        Self {
            facade,
            security: &SystemSecurity,
            provider,
            settings,
            model_warning_pending: models.iter().any(|m| !m.eq_ignore_ascii_case("all")),
            models,
            dry_run,
            sink: Box::new(sink),
            clock: Box::new(|| now_unix() as f64),
            unhealthy_ticks: 0,
            idle_hold_since: None,
            sleep_until: None,
            blocked_wait_long: false,
            idle_hold_slow: false,
            active_next_poll_at: None,
        }
    }

    /// Replace the Keychain client (tests).
    #[cfg(test)]
    fn with_security(mut self, security: &'a dyn SecurityCli) -> Self {
        self.security = security;
        self
    }

    /// The provider this engine rotates.
    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Replace the wall clock (tests).
    pub fn with_clock(mut self, clock: impl Fn() -> f64 + 'a) -> Self {
        self.clock = Box::new(clock);
        self
    }

    pub fn settings(&self) -> &AutoSwitchSettings {
        &self.settings
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn emit(&mut self, event: Event) {
        (self.sink)(&event);
    }

    fn no_switch(&mut self, reason: &str, detail: impl Into<String>) {
        self.emit(Event::NoSwitch {
            reason: reason.to_string(),
            detail: detail.into(),
        });
    }

    /// One evaluation; never fails (an error becomes an `error` event).
    pub fn tick(&mut self) -> TickOutcome {
        match self.tick_inner() {
            Ok(outcome) => outcome,
            Err(err) => {
                self.emit(Event::Error {
                    message: err.to_string(),
                    transient: true,
                });
                TickOutcome::Error
            }
        }
    }

    fn tick_inner(&mut self) -> Result<TickOutcome> {
        self.sleep_until = None;
        self.blocked_wait_long = false;
        self.idle_hold_slow = false;
        let now = self.now();
        let threshold = self.settings.threshold;

        let roster = self.facade.roster()?;
        let mut state = state::read(&self.facade.store().paths);
        if !self.dry_run {
            state = self.release_recovered_quarantines(&roster, state)?;
        }
        let quarantined: Vec<u32> = state
            .quarantine
            .keys()
            .filter_map(|key| key.parse().ok())
            .collect();

        let (current, email, api_key) = match self.facade.current_account(self.provider)? {
            CurrentAccount::Managed {
                slot,
                email,
                api_key,
            } => (slot, email, api_key),
            other => {
                self.emit(Event::Poll {
                    active: None,
                    headroom: BTreeMap::new(),
                    threshold,
                    fetch_errors: BTreeMap::new(),
                    windows: BTreeMap::new(),
                });
                match other {
                    CurrentAccount::NoLogin => {
                        self.no_switch("no-active-account", "log in and run 'ccsw add' first");
                    }
                    _ => self.no_switch(
                        "unmanaged-active-account",
                        "run 'ccsw add' to include it in rotation",
                    ),
                }
                return Ok(TickOutcome::NoAction);
            }
        };
        let active_ref = AccountRef {
            number: Some(current),
            email: email.clone(),
        };
        let store = self.facade.store();
        // Keep the other provider out of this engine's candidate list.
        let provider = self.provider;
        let candidates: Vec<u32> = roster
            .switchable_slots(|slot| credentials::exists(store, slot))
            .into_iter()
            .filter(|slot| roster.record(*slot).is_some_and(|r| r.provider == provider))
            .filter(|slot| *slot != current && !quarantined.contains(slot))
            .collect();

        let collected = self.collect(&roster, current, &candidates, now)?;
        let entries = collected.entries;
        for failure in collected.token_persist_failures {
            self.emit(Event::Error {
                message: failure,
                transient: false,
            });
        }
        self.active_next_poll_at = entries.get(&current).and_then(|e| e.next_poll_at);
        let headroom: BTreeMap<u32, Option<f64>> = std::iter::once(current)
            .chain(candidates.iter().copied())
            .map(|slot| (slot, entry_headroom(&entries, slot, &self.models)))
            .collect();
        let fetch_errors: BTreeMap<u32, String> = headroom
            .iter()
            .filter(|(_, h)| h.is_none())
            .filter_map(|(slot, _)| Some((*slot, entries.get(slot)?.last_error.clone()?)))
            .collect();
        let windows: BTreeMap<u32, Vec<(String, f64)>> = headroom
            .keys()
            .filter_map(|slot| {
                let usage = entries.get(slot)?.decision_value()?;
                let rows: Vec<(String, f64)> = relevant_windows(usage, &self.models)
                    .into_iter()
                    .map(|(label, pct, _)| (label, pct))
                    .collect();
                (!rows.is_empty()).then_some((*slot, rows))
            })
            .collect();
        self.emit(Event::Poll {
            active: Some(active_ref.clone()),
            headroom: headroom.clone(),
            threshold,
            fetch_errors,
            windows,
        });

        let in_pass: Vec<u32> = std::iter::once(current)
            .chain(candidates.iter().copied())
            .collect();
        self.warn_unknown_models(&entries, &in_pass);

        if api_key && !self.settings.include_api_key_accounts {
            self.no_switch("active-api-key", "API-key accounts have no quota to watch");
            return Ok(TickOutcome::NoAction);
        }

        let consume_first = self.settings.strategy == "consume-first";
        let active_headroom = headroom.get(&current).copied().flatten();
        let below_threshold_detail =
            |h: f64| format!("{}% < {}%", pct_label(100.0 - h), pct_label(threshold));
        let trigger = match active_headroom {
            Some(h) => {
                self.unhealthy_ticks = 0;
                self.idle_hold_since = None;
                let utilization = 100.0 - h;
                if utilization < threshold {
                    if !consume_first {
                        self.no_switch("below-threshold", below_threshold_detail(h));
                        return Ok(TickOutcome::NoAction);
                    }
                    Trigger::ConsumeFirst
                } else if h <= 0.0 {
                    Trigger::AtLimit
                } else {
                    Trigger::Proactive
                }
            }
            None => {
                if let Some(held) = held_sentinel(entries.get(&current)) {
                    let since = *self.idle_hold_since.get_or_insert(now);
                    if held == UsageSentinel::KeychainUnavailable || now - since <= IDLE_HOLD_MAX_S
                    {
                        self.unhealthy_ticks = 0;
                        self.idle_hold_slow = true;
                        let detail = if held == UsageSentinel::KeychainUnavailable {
                            "keychain unavailable; holding until Claude Code's login is readable"
                                .to_string()
                        } else {
                            format!(
                                "token expired while {} is idle; resumes on next use",
                                self.provider.tool_name()
                            )
                        };
                        self.no_switch("active-idle", detail);
                        return Ok(TickOutcome::NoAction);
                    }
                    tracing::warn!(
                        "active account has idled past {IDLE_HOLD_MAX_S}s with an expired token; counting it as unhealthy"
                    );
                } else {
                    self.idle_hold_since = None;
                }
                self.unhealthy_ticks += 1;
                if self.unhealthy_ticks < self.settings.unhealthy_ticks {
                    let detail = format!(
                        "{}/{} before failover",
                        self.unhealthy_ticks, self.settings.unhealthy_ticks
                    );
                    self.no_switch("active-usage-unknown", detail);
                    return Ok(TickOutcome::NoAction);
                }
                Trigger::Failover
            }
        };

        if trigger.honors_cooldown()
            && let Some(remaining) = self.cooldown_remaining(&state, now)
        {
            self.no_switch("cooldown", format!("{}s remaining", remaining.ceil()));
            return Ok(TickOutcome::NoAction);
        }

        let is_api_key = |slot: &u32| roster.record(*slot).is_some_and(|r| r.is_api_key());
        let oauth: Vec<u32> = candidates
            .iter()
            .copied()
            .filter(|slot| !is_api_key(slot))
            .collect();
        let api_keys: Vec<u32> = if self.settings.include_api_key_accounts {
            candidates.iter().copied().filter(is_api_key).collect()
        } else {
            Vec::new()
        };
        if trigger == Trigger::ConsumeFirst
            && oauth.is_empty()
            && let Some(h) = active_headroom
        {
            self.no_switch("below-threshold", below_threshold_detail(h));
            return Ok(TickOutcome::NoAction);
        }
        if oauth.is_empty() && api_keys.is_empty() {
            self.blocked_wait_long = true;
            self.no_switch("no-candidates", "no other switchable account");
            return Ok(TickOutcome::Blocked);
        }

        let mut ranked = rank_candidates(
            trigger,
            consume_first,
            &oauth,
            &entries,
            current,
            active_headroom,
            &self.settings,
            &self.models,
            now,
        );
        if ranked.ordered.is_empty() && !api_keys.is_empty() && trigger != Trigger::ConsumeFirst {
            ranked.ordered = api_keys;
        }
        if ranked.ordered.is_empty() {
            if !ranked.any_known {
                self.no_switch("no-comparison", "no candidate has readable usage");
                return Ok(TickOutcome::Blocked);
            }
            if trigger == Trigger::ConsumeFirst {
                if ranked.active_reset_ts.is_none() {
                    self.no_switch(
                        "reset-unknown",
                        "active account's weekly reset time is unknown; consume-first is idle until it is reported",
                    );
                } else {
                    self.no_switch(
                        "already-consuming-soonest",
                        "no sooner-resetting account with room to spare",
                    );
                }
                return Ok(TickOutcome::NoAction);
            }
            let truly_exhausted = oauth.iter().all(|slot| {
                headroom
                    .get(slot)
                    .copied()
                    .flatten()
                    .is_some_and(|h| h <= 0.0)
            });
            if !truly_exhausted {
                self.no_switch(
                    "no-qualifying-candidate",
                    "no candidate is below the threshold and better than the active account by the hysteresis margin, or usage is unreadable this tick",
                );
                return Ok(TickOutcome::Blocked);
            }
            self.blocked_wait_long = true;
            let earliest = earliest_recovery(&entries, &self.models, now);
            if let Some(reset) = earliest {
                self.sleep_until = Some(reset as f64 + RESET_SLACK_S);
            }
            self.emit(Event::AllExhausted {
                earliest_reset_at: earliest.map(format_iso),
            });
            return Ok(TickOutcome::Blocked);
        }

        let mut transient = false;
        for target in ranked.ordered {
            let target_email = roster
                .record(target)
                .map(|r| r.email.clone())
                .unwrap_or_default();
            if self.dry_run {
                return self.perform(target, &target_email, trigger, &active_ref);
            }
            match collect::refresh_slot_with(
                self.facade.store(),
                &roster,
                target,
                false,
                self.security,
            ) {
                RefreshStatus::Ok | RefreshStatus::NotNeeded => {
                    return self.perform(target, &target_email, trigger, &active_ref);
                }
                RefreshStatus::NoRefreshToken | RefreshStatus::Terminal { .. } => {
                    self.quarantine(target, &target_email, "invalid_grant")?;
                }
                RefreshStatus::Transient(detail) => {
                    tracing::warn!("could not freshen account {target}: {detail}");
                    transient = true;
                }
            }
        }
        if transient {
            self.emit(Event::Error {
                message: "could not freshen any candidate (network?)".to_string(),
                transient: true,
            });
            return Ok(TickOutcome::Error);
        }
        self.no_switch("no-viable-target", "");
        Ok(TickOutcome::Blocked)
    }

    /// The scheduled pass, escalated to every candidate when the active
    /// headroom is unknown (and not an idle hold) or within the margin.
    fn collect(
        &mut self,
        roster: &Roster,
        active: u32,
        candidates: &[u32],
        _now: f64,
    ) -> Result<Collected> {
        let threshold = self.settings.threshold;
        let store = self.facade.store();
        let models = &self.models;
        // Protect a login changed outside this tick without fetching or
        // refreshing the other provider's accounts.
        let mut actives = vec![active];
        if let Some(slot) =
            collect::live_login_for_with(store, roster, self.provider, self.security).slot()
            && !actives.contains(&slot)
        {
            actives.push(slot);
        }
        let mut collected = collect::run_pass_with(
            store,
            roster,
            CollectOptions {
                mode: CollectMode::Scheduled,
                actives: actives.clone(),
                candidates,
                threshold,
                models,
            },
            self.security,
        )?;
        let active_entry = collected.entries.get(&active);
        let active_headroom = entry_headroom(&collected.entries, active, models);
        let idle = held_sentinel(active_entry).is_some();
        let escalate = !candidates.is_empty()
            && match active_headroom {
                None => !idle,
                Some(h) => 100.0 - h >= threshold - ESCALATION_MARGIN_PCT,
            };
        if escalate {
            let more = collect::run_pass_with(
                store,
                roster,
                CollectOptions {
                    mode: CollectMode::Escalation,
                    actives,
                    candidates,
                    threshold,
                    models,
                },
                self.security,
            )?;
            collected.entries = more.entries;
            collected
                .token_persist_failures
                .extend(more.token_persist_failures);
        }
        Ok(collected)
    }

    /// cswap's typo guard: once per run, on the first tick where every slot in
    /// the pass that is not an API-key account has a readable usage dict, warn
    /// about configured model names that no scoped window reports. `all` never
    /// warns, and the check never fetches.
    fn warn_unknown_models(&mut self, entries: &BTreeMap<u32, UsageEntry>, slots: &[u32]) {
        if !self.model_warning_pending {
            return;
        }
        let wanted: Vec<String> = self
            .models
            .iter()
            .filter(|m| !m.eq_ignore_ascii_case("all"))
            .cloned()
            .collect();
        if wanted.is_empty() {
            self.model_warning_pending = false;
            return;
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for slot in slots {
            let entry = entries.get(slot);
            if entry.and_then(|e| e.sentinel) == Some(UsageSentinel::ApiKey) {
                continue;
            }
            let Some(usage) = entry.and_then(UsageEntry::decision_value) else {
                return; // not every slot is readable yet: try again next tick
            };
            seen.extend(usage.scoped.iter().map(|w| w.name.to_ascii_lowercase()));
        }
        self.model_warning_pending = false;
        let missing: Vec<String> = wanted
            .into_iter()
            .filter(|m| !seen.contains(&m.to_ascii_lowercase()))
            .collect();
        if !missing.is_empty() {
            self.emit(Event::ConfigWarning {
                message: format!(
                    "autoswitch.model: {} matches no account's usage windows — only the 5h/7d limits are being watched for it (typo?)",
                    missing.join(",")
                ),
            });
        }
    }

    fn cooldown_remaining(&self, state: &AutoSwitchState, now: f64) -> Option<f64> {
        let last = state.last_switch_at?;
        let remaining = self.settings.cooldown_seconds - (now - last);
        (remaining > 0.0).then_some(remaining)
    }

    /// Dry-run reports; a real switch runs under the state lock with the
    /// cooldown re-checked, then records the switch for the next cooldown.
    fn perform(
        &mut self,
        target: u32,
        target_email: &str,
        trigger: Trigger,
        active_ref: &AccountRef,
    ) -> Result<TickOutcome> {
        if self.dry_run {
            self.emit(Event::Switch {
                trigger,
                from: Some(active_ref.clone()),
                to: Some(AccountRef {
                    number: Some(target),
                    email: target_email.to_string(),
                }),
                warnings: Vec::new(),
                dry_run: true,
            });
            return Ok(TickOutcome::Switched);
        }
        let paths = self.facade.store().paths.clone();
        let lock = self.facade.store().lock_state()?;
        let now = self.now();
        let state_before = state::read(&paths);
        if trigger.honors_cooldown()
            && let Some(remaining) = self.cooldown_remaining(&state_before, now)
        {
            drop(lock);
            self.no_switch("cooldown", format!("{}s remaining", remaining.ceil()));
            return Ok(TickOutcome::NoAction);
        }
        let result = self.facade.switch_to(target)?;
        if !result.switched {
            drop(lock);
            self.no_switch("already-active", result.reason.clone());
            return Ok(TickOutcome::NoAction);
        }
        let mut state = state::read(&paths);
        state.last_switch_at = Some(now);
        state.last_switch_to = Some(target.to_string());
        state.last_switch_from = result.from.as_ref().and_then(|from| from.number);
        state::write(&paths, &state)?;
        drop(lock);
        tracing::info!(
            "auto-switched {} -> Account-{target} ({})",
            account_label(active_ref),
            trigger.as_str()
        );
        self.emit(Event::Switch {
            trigger,
            from: result.from,
            to: result.to,
            warnings: result.warnings,
            dry_run: false,
        });
        Ok(TickOutcome::Switched)
    }

    fn quarantine(&mut self, slot: u32, email: &str, reason: &str) -> Result<()> {
        let store = self.facade.store();
        let entry = QuarantineEntry {
            email: email.to_string(),
            reason: reason.to_string(),
            at: format_iso(self.now() as i64),
            refresh_token_fingerprint: credentials::slot_fingerprint(store, slot),
        };
        state::modify(store, |state| {
            state.quarantine.insert(slot.to_string(), entry);
        })?;
        self.emit(Event::AccountQuarantined {
            number: slot,
            email: email.to_string(),
            reason: reason.to_string(),
        });
        Ok(())
    }

    /// Drop quarantine entries whose slot changed identity or credentials.
    fn release_recovered_quarantines(
        &mut self,
        roster: &Roster,
        state: AutoSwitchState,
    ) -> Result<AutoSwitchState> {
        let store = self.facade.store();
        let mut released: Vec<(String, u32, String, &'static str)> = Vec::new();
        for (key, entry) in &state.quarantine {
            let Ok(slot) = key.parse::<u32>() else {
                continue;
            };
            let reason = match roster.record(slot) {
                None => "account-replaced",
                Some(record) if record.email != entry.email => "account-replaced",
                Some(_) => {
                    if credentials::slot_fingerprint(store, slot) != entry.refresh_token_fingerprint
                    {
                        "credentials-replaced"
                    } else {
                        continue;
                    }
                }
            };
            released.push((key.clone(), slot, entry.email.clone(), reason));
        }
        if released.is_empty() {
            return Ok(state);
        }
        let state = state::modify(store, |state| {
            for (key, ..) in &released {
                state.quarantine.remove(key);
            }
            state.clone()
        })?;
        for (_, slot, email, reason) in released {
            self.emit(Event::AccountUnquarantined {
                number: slot,
                email,
                reason: reason.to_string(),
            });
        }
        Ok(state)
    }

    /// Seconds to wait after `outcome` (research notes §6.14).
    pub fn next_delay(&self, outcome: TickOutcome) -> f64 {
        let interval = self.settings.interval_seconds;
        let now = self.now();
        match outcome {
            TickOutcome::Blocked => {
                if let Some(until) = self.sleep_until {
                    (until - now).min(MAX_SLEEP_S).max(interval)
                } else if self.blocked_wait_long {
                    interval.max(NO_RESET_FALLBACK_S)
                } else {
                    self.respect_poll_plan(jittered(interval), now)
                }
            }
            TickOutcome::NoAction if self.idle_hold_slow => interval.max(NO_RESET_FALLBACK_S),
            _ => self.respect_poll_plan(jittered(interval), now),
        }
    }

    /// Only ever shortens a delay toward the active account's next poll.
    fn respect_poll_plan(&self, delay: f64, now: f64) -> f64 {
        match self.active_next_poll_at {
            Some(at) => delay.min((at - now).max(URGENT_INTERVAL_S)),
            None => delay,
        }
    }

    /// Tick until `stop` is set; returns 0.
    pub fn run_loop(&mut self, stop: Arc<AtomicBool>) -> i32 {
        loop {
            if stop.load(Ordering::SeqCst) {
                return 0;
            }
            let outcome = self.tick();
            let delay = self.next_delay(outcome);
            if delay > self.settings.interval_seconds * 1.5 {
                let until = self.now() + delay;
                self.emit(Event::Sleep {
                    seconds: delay,
                    until: format_iso(until as i64),
                });
            }
            let deadline = Instant::now() + Duration::from_secs_f64(delay.max(0.0));
            while Instant::now() < deadline {
                if stop.load(Ordering::SeqCst) {
                    return 0;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(remaining.min(Duration::from_millis(200)));
            }
        }
    }
}

fn jittered(interval: f64) -> f64 {
    interval * (0.9 + 0.2 * rand::random::<f64>())
}

fn entry_headroom(
    entries: &BTreeMap<u32, UsageEntry>,
    slot: u32,
    models: &[String],
) -> Option<f64> {
    entries
        .get(&slot)
        .and_then(|entry| collect::entry_headroom(entry, models))
}

/// Rank the OAuth candidates (research notes §6.7, v0.1 subset: no
/// no-return bar, no all-above-threshold escape).
#[allow(clippy::too_many_arguments)]
fn rank_candidates(
    trigger: Trigger,
    consume_first: bool,
    oauth_candidates: &[u32],
    entries: &BTreeMap<u32, UsageEntry>,
    current: u32,
    active_headroom: Option<f64>,
    settings: &AutoSwitchSettings,
    models: &[String],
    now: f64,
) -> Ranked {
    let usage_of = |slot: u32| entries.get(&slot).and_then(|e| e.decision_value());
    let reset_of = |slot: u32| {
        consume_first
            .then(|| usage_of(slot).and_then(|usage| seven_day_reset_ts(usage, now as i64)))
            .flatten()
    };
    let active_reset_ts = reset_of(current);
    let mut any_known = false;
    let mut qualifying: Vec<((i64, f64), u32)> = Vec::new();
    for &slot in oauth_candidates {
        let Some(h) = entry_headroom(entries, slot, models) else {
            continue;
        };
        any_known = true;
        if h <= 0.0 {
            continue;
        }
        let reset_ts = reset_of(slot);
        if matches!(trigger, Trigger::Proactive | Trigger::ConsumeFirst) {
            if 100.0 - h >= settings.threshold {
                continue;
            }
            if consume_first {
                if trigger == Trigger::ConsumeFirst
                    && match (reset_ts, active_reset_ts) {
                        (Some(mine), Some(active)) => mine >= active,
                        _ => true,
                    }
                {
                    continue;
                }
            } else if let Some(active_h) = active_headroom
                && h - active_h < settings.hysteresis_pct
            {
                continue;
            }
        }
        let key = if consume_first {
            (reset_ts.unwrap_or(i64::MAX), -h)
        } else {
            (0, -h)
        };
        qualifying.push((key, slot));
    }
    // Stable: sequence order breaks ties.
    qualifying.sort_by(|a, b| {
        a.0.0.cmp(&b.0.0).then(
            a.0.1
                .partial_cmp(&b.0.1)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    Ranked {
        ordered: qualifying.into_iter().map(|(_, slot)| slot).collect(),
        any_known,
        active_reset_ts,
    }
}

/// When the soonest exhausted account is usable again. `None` as soon as any
/// exhausted account's recovery is unknown or already past: never oversleep
/// toward another account's later reset.
fn earliest_recovery(
    entries: &BTreeMap<u32, UsageEntry>,
    models: &[String],
    now: f64,
) -> Option<i64> {
    let mut earliest: Option<i64> = None;
    for entry in entries.values() {
        let Some(usage) = entry.decision_value() else {
            continue;
        };
        let blocked = relevant_windows(usage, models)
            .iter()
            .any(|(_, pct, _)| *pct >= 100.0);
        if !blocked {
            continue;
        }
        match limiting_reset_ts(usage, models) {
            Some(reset) if reset as f64 > now => {
                earliest = Some(earliest.map_or(reset, |e| e.min(reset)));
            }
            _ => return None,
        }
    }
    earliest
}

const AUTO_EPILOG: &str = "Exit codes with --once:
  0  switched to another account
  1  error (network trouble, lock contention, no Claude Code account, ...)
  2  no action needed
  3  blocked: wanted to switch but no viable target / all exhausted

Auto-switch covers Claude Code accounts: a running Claude Code session picks the
new login up by itself (next message, or ~30 s with the macOS Keychain). Codex
sessions keep the account they started with until they restart, so there is no
Codex auto-switch; use `ccsw switch` and restart the session instead.

Examples:
  ccsw auto                       # foreground loop, switch at 90% used
  ccsw auto claude                # the same (claude is the only provider)
  ccsw auto --threshold 80        # switch earlier
  ccsw auto --model Fable         # also switch when that model's weekly limit is hit
  ccsw auto --json                # one JSON event per line (for scripts)
  ccsw auto --once; echo $?       # single tick, outcome in exit code
  ccsw auto --dry-run             # log decisions, never actually switch

Defaults live in settings.json in the backup root; flags override them.";

#[derive(Debug, Parser)]
#[command(
    name = "ccsw auto",
    about = "Automatically switch Claude Code accounts when the active one nears its 5h/7d rate limit. Runs a foreground polling loop; use --once for a single tick (cron-friendly).",
    after_help = AUTO_EPILOG,
    disable_version_flag = true
)]
struct AutoArgs {
    /// Evaluate once, maybe switch, and exit (exit code = outcome)
    #[arg(long)]
    once: bool,
    /// Emit one machine-readable JSON event per line on stdout
    #[arg(long)]
    json: bool,
    /// Poll interval in loop mode (min 15; default 60)
    #[arg(long, value_name = "SECONDS")]
    interval: Option<f64>,
    /// Switch when the active account's binding 5h/7d window reaches this utilization (50-99.9; default 90)
    #[arg(long, value_name = "PCT")]
    threshold: Option<f64>,
    /// Minimum time between proactive switches (default 300)
    #[arg(long, value_name = "SECONDS")]
    cooldown: Option<f64>,
    /// Also switch when a per-model weekly limit is hit, not just the account-wide 5h/7d windows. One pool name or a comma-separated list of the model pools an account reports (e.g. Fable), or 'all' for every per-model window
    #[arg(long, value_name = "NAMES")]
    model: Option<String>,
    /// Allow switching onto managed API-key accounts as a last resort (they bill per token; default: excluded)
    #[arg(long, conflicts_with = "no_include_api_key_accounts")]
    include_api_key_accounts: bool,
    /// Never switch onto managed API-key accounts (overrides the setting)
    #[arg(long = "no-include-api-key-accounts")]
    no_include_api_key_accounts: bool,
    /// Target selection: 'best' (most quota left; default) or 'consume-first' (proactively use the account whose weekly window resets soonest)
    #[arg(long, value_parser = ["best", "consume-first"])]
    strategy: Option<String>,
    /// Evaluate and report, but never switch or write state
    #[arg(long)]
    dry_run: bool,
    /// Enable debug logging
    #[arg(long)]
    debug: bool,
}

impl AutoArgs {
    fn overrides(&self) -> AutoSwitchOverrides {
        AutoSwitchOverrides {
            threshold: self.threshold,
            interval_seconds: self.interval,
            cooldown_seconds: self.cooldown,
            include_api_key_accounts: if self.include_api_key_accounts {
                Some(true)
            } else if self.no_include_api_key_accounts {
                Some(false)
            } else {
                None
            },
            model: self.model.clone(),
            strategy: self.strategy.clone(),
        }
    }
}

/// Refuse to run as root outside a container; returns the exit status.
pub fn root_guard() -> Option<i32> {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let root = unsafe { libc::geteuid() } == 0;
        if root && !running_in_container() {
            eprintln!("Error: Do not run this script as root (unless running in a container)");
            return Some(1);
        }
    }
    None
}

#[cfg(unix)]
fn running_in_container() -> bool {
    let env_set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if env_set("CONTAINER") || env_set("container") || std::path::Path::new("/.dockerenv").exists()
    {
        return true;
    }
    std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|cgroup| {
        ["docker", "lxc", "containerd", "kubepods"]
            .iter()
            .any(|hint| cgroup.contains(hint))
    })
}

/// Write one event: compact JSON, or `HH:MM:SS  <human>` colored by kind.
fn write_event(out: &mut dyn Write, event: &Event, provider: Provider, json: bool) {
    if json {
        let _ = writeln!(out, "{}", event.to_json(provider));
    } else {
        let line = event.human();
        let styled = match event {
            Event::Switch { .. } => printer::accent(&line),
            Event::Error { .. } | Event::AccountQuarantined { .. } => printer::yellowed(&line),
            Event::Poll { .. } | Event::NoSwitch { .. } | Event::Sleep { .. } => {
                printer::dimmed(&line)
            }
            _ => line,
        };
        let _ = writeln!(out, "{}  {styled}", chrono::Local::now().format("%H:%M:%S"));
    }
    let _ = out.flush();
}

/// Wait for SIGINT / SIGTERM on a helper thread: both stop the loop, and
/// `interrupted` tells the front end to exit 130. After the first signal the
/// default disposition is restored so a second Ctrl-C ends a long fetch at once.
fn install_signal_handlers(stop: Arc<AtomicBool>, interrupted: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                match signal(SignalKind::terminate()) {
                    Ok(mut term) => {
                        tokio::select! {
                            _ = tokio::signal::ctrl_c() => interrupted.store(true, Ordering::SeqCst),
                            _ = term.recv() => {}
                        }
                    }
                    Err(_) => {
                        if tokio::signal::ctrl_c().await.is_ok() {
                            interrupted.store(true, Ordering::SeqCst);
                        }
                    }
                }
                // SAFETY: restoring the default disposition of two standard signals.
                unsafe {
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    libc::signal(libc::SIGTERM, libc::SIG_DFL);
                }
            }
            #[cfg(not(unix))]
            {
                if tokio::signal::ctrl_c().await.is_ok() {
                    interrupted.store(true, Ordering::SeqCst);
                }
            }
            stop.store(true, Ordering::SeqCst);
        });
    });
}

/// `ccsw auto` (everything after the verb). Returns the exit status.
pub fn run_cli(argv: Vec<String>, facade: &mut dyn AutoFacade) -> i32 {
    run_cli_to(argv, facade, &mut std::io::stdout())
}

/// [`run_cli`] with the event stream (and banner) written to `out`.
pub fn run_cli_to(argv: Vec<String>, facade: &mut dyn AutoFacade, out: &mut dyn Write) -> i32 {
    let mut argv = argv;
    match argv.first().and_then(|word| Provider::parse_selector(word)) {
        Some(Provider::Claude) => {
            argv.remove(0);
        }
        Some(Provider::Codex) => {
            eprintln!("Error: {CLAUDE_ONLY_NOTICE}");
            return 1;
        }
        None => {}
    }
    let args = match AutoArgs::try_parse_from(std::iter::once("ccsw auto".to_string()).chain(argv))
    {
        Ok(args) => args,
        Err(err) => {
            let _ = err.print();
            return err.exit_code();
        }
    };
    if let Some(code) = root_guard() {
        return code;
    }
    match facade.roster() {
        Ok(roster) if roster.slots_of(Provider::Claude).is_empty() => {
            eprintln!("Error: {CLAUDE_ONLY_NOTICE}");
            return 1;
        }
        Ok(_) => {}
        Err(err) => {
            eprintln!("Error: {err}");
            return 1;
        }
    }
    if args.debug {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_writer(std::io::stderr)
            .try_init();
    }
    let settings = Settings::load(&facade.store().paths)
        .autoswitch
        .merged_with_cli(&args.overrides());
    let json = args.json;
    if !json && !args.once {
        let banner = format!(
            "Auto-switch running: threshold {:.0}%, every {:.0}s{} — Ctrl-C to stop",
            settings.threshold,
            settings.interval_seconds,
            if args.dry_run { " (dry-run)" } else { "" }
        );
        let _ = writeln!(out, "{}", printer::dimmed(&banner));
    }
    let sink = |event: &Event| write_event(out, event, Provider::Claude, json);
    let mut engine = Engine::new(facade, Provider::Claude, settings, args.dry_run, sink);
    if args.once {
        return engine.tick().code();
    }
    let stop = Arc::new(AtomicBool::new(false));
    let interrupted = Arc::new(AtomicBool::new(false));
    install_signal_handlers(stop.clone(), interrupted.clone());
    let code = engine.run_loop(stop);
    drop(engine);
    if interrupted.load(Ordering::SeqCst) {
        let note = printer::dimmed("\nAuto-switch stopped");
        if json {
            eprintln!("{note}");
        } else {
            let _ = writeln!(out, "{note}");
        }
        return 130;
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::auth::AuthJson;
    use crate::codex::jwt::make_jwt;
    use crate::model::{
        AccountKind, AccountRecord, Identity, NormalizedUsage, SwitchOutcome, WindowUsage,
    };
    use crate::provider::Provider;
    use crate::store::temp_store;
    use crate::store::usage_store::{FetchRecord, UsageStore};
    use std::cell::RefCell;
    use std::rc::Rc;

    const FAR: i64 = 4_102_444_800;

    struct Fake {
        store: Store,
        roster: Roster,
        provider: Provider,
        current: CurrentAccount,
        switches: Vec<u32>,
        fail_switch: bool,
    }

    impl AutoFacade for Fake {
        fn store(&self) -> &Store {
            &self.store
        }
        fn roster(&mut self) -> Result<Roster> {
            Ok(self.roster.clone())
        }
        fn current_account(&mut self, provider: Provider) -> Result<CurrentAccount> {
            assert_eq!(
                provider, self.provider,
                "the engine asks for its own provider"
            );
            Ok(self.current.clone())
        }
        fn switch_to(&mut self, slot: u32) -> Result<SwitchOutcome> {
            if self.fail_switch {
                return Err(crate::errors::CcswError::switch("boom"));
            }
            self.switches.push(slot);
            let from = match &self.current {
                CurrentAccount::Managed { slot, email, .. } => Some(AccountRef {
                    number: Some(*slot),
                    email: email.clone(),
                }),
                _ => None,
            };
            let email = self.roster.record(slot).unwrap().email.clone();
            let switched = from.as_ref().and_then(|f| f.number) != Some(slot);
            if switched {
                self.current = CurrentAccount::Managed {
                    slot,
                    email: email.clone(),
                    api_key: false,
                };
            }
            Ok(SwitchOutcome {
                switched,
                provider: self.provider,
                from,
                to: Some(AccountRef {
                    number: Some(slot),
                    email,
                }),
                strategy: "direct".into(),
                reason: if switched {
                    "switched"
                } else {
                    "already-active"
                }
                .into(),
                message: String::new(),
                warnings: vec!["w1".into()],
            })
        }
    }

    /// A Claude slot file for `a{slot}@example.com` whose token is far from expiry.
    fn claude_slot(slot: u32) -> Value {
        use crate::claude::credentials::{ClaudeCredential, OauthAccount, SlotFile};
        let credential = ClaudeCredential::from_value(json!({"claudeAiOauth": {
            "accessToken": format!("cat-{slot}"), "refreshToken": format!("crt-{slot}"),
            "expiresAt": FAR * 1000, "scopes": ["user:inference"]
        }}));
        SlotFile::new(
            &credential,
            OauthAccount::synthesized(&format!("a{slot}@example.com")),
        )
        .to_value()
    }

    fn auth(account_id: &str, access_exp: i64, refresh: Option<&str>) -> AuthJson {
        let id_token = make_jwt(&json!({
            "email": format!("{account_id}@example.com"),
            "exp": FAR,
            "https://api.openai.com/auth": {"chatgpt_account_id": account_id}
        }));
        let mut tokens = json!({
            "id_token": id_token,
            "access_token": make_jwt(&json!({"exp": access_exp})),
            "account_id": account_id,
        });
        if let Some(rt) = refresh {
            tokens["refresh_token"] = json!(rt);
        }
        AuthJson::from_value(json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "tokens": tokens,
            "last_refresh": "2026-09-29T10:00:00Z"
        }))
    }

    fn usage(five_hour: f64, seven_day: f64, reset: Option<i64>) -> NormalizedUsage {
        NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: five_hour,
                resets_at: reset.map(format_iso),
            }),
            seven_day: Some(WindowUsage {
                pct: seven_day,
                resets_at: reset.map(|r| format_iso(r + 86_400)),
            }),
            ..NormalizedUsage::default()
        }
    }

    fn entry_with(usage: Option<NormalizedUsage>, age: f64) -> UsageEntry {
        UsageEntry {
            sentinel: None,
            last_good: usage,
            fetched_at: Some(1_000_000.0 - age),
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

    /// A store whose rows are fresh and planned, so no tick ever fetches.
    struct Fixture {
        _dir: tempfile::TempDir,
        fake: Fake,
        now: f64,
    }

    impl Fixture {
        fn new(slots: &[u32]) -> Self {
            Self::new_for(Provider::Codex, slots)
        }

        /// Claude records with Claude slot files; the first slot is the live login.
        fn claude(slots: &[u32]) -> Self {
            Self::new_for(Provider::Claude, slots)
        }

        fn new_for(provider: Provider, slots: &[u32]) -> Self {
            let (dir, store) = temp_store();
            let mut roster = Roster::empty();
            for &slot in slots {
                let id = format!("a{slot}");
                let mut record = AccountRecord::new(format!("{id}@example.com"));
                record.organization_uuid = id.clone();
                record.provider = provider;
                roster.add_record(slot, record);
                match provider {
                    Provider::Codex => {
                        credentials::write(&store, slot, &auth(&id, FAR, Some("rt")).0).unwrap()
                    }
                    Provider::Claude => {
                        credentials::write(&store, slot, &claude_slot(slot)).unwrap()
                    }
                }
            }
            let current = CurrentAccount::Managed {
                slot: slots[0],
                email: format!("a{}@example.com", slots[0]),
                api_key: false,
            };
            Self {
                _dir: dir,
                fake: Fake {
                    store,
                    roster,
                    provider,
                    current,
                    switches: Vec::new(),
                    fail_switch: false,
                },
                now: now_unix() as f64,
            }
        }

        fn identity(&self, slot: u32) -> Identity {
            self.fake.roster.record(slot).unwrap().identity()
        }

        fn seed(&self, slot: u32, usage: NormalizedUsage) {
            UsageStore::new(&self.fake.store.paths)
                .record(
                    slot,
                    &self.identity(slot),
                    FetchRecord::Success {
                        usage,
                        plan: Some((self.now + 600.0, 300.0)),
                    },
                    self.now,
                )
                .unwrap();
        }

        /// A row in a long backoff with no measurement: unknown, never fetched.
        fn seed_failure(&self, slot: u32, error: &str) {
            UsageStore::new(&self.fake.store.paths)
                .record(
                    slot,
                    &self.identity(slot),
                    FetchRecord::Failure {
                        error: error.into(),
                        retry_after: Some(900.0),
                        permanent_auth: false,
                        struck_fp: None,
                    },
                    self.now,
                )
                .unwrap();
        }
    }

    fn defaults() -> AutoSwitchSettings {
        AutoSwitchSettings::default()
    }

    type Log = Rc<RefCell<Vec<Event>>>;

    fn tick(
        fixture: &mut Fixture,
        settings: AutoSwitchSettings,
        dry_run: bool,
    ) -> (TickOutcome, Vec<Event>) {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        let provider = fixture.fake.provider;
        let outcome = {
            let mut engine = Engine::new(
                &mut fixture.fake,
                provider,
                settings,
                dry_run,
                move |event| sink_log.borrow_mut().push(event.clone()),
            );
            engine.tick()
        };
        let events = log.borrow().clone();
        (outcome, events)
    }

    fn kinds(events: &[Event]) -> Vec<&'static str> {
        events.iter().map(Event::kind).collect()
    }

    fn no_switch_reason(events: &[Event]) -> Option<(String, String)> {
        events.iter().find_map(|e| match e {
            Event::NoSwitch { reason, detail } => Some((reason.clone(), detail.clone())),
            _ => None,
        })
    }

    fn attempts(fixture: &Fixture, slot: u32) -> Option<f64> {
        let mut ids = BTreeMap::new();
        ids.insert(slot, fixture.identity(slot));
        UsageStore::new(&fixture.fake.store.paths)
            .entries(&ids, fixture.now)
            .get(&slot)
            .and_then(|e| e.last_attempt_at)
    }

    // ---- events ----

    #[test]
    fn pct_label_matches_python_g_formatting() {
        assert_eq!(pct_label(90.0), "90");
        assert_eq!(pct_label(99.9), "99.9");
        assert_eq!(pct_label(62.600_000_000_000_01), "62.6");
        assert_eq!(pct_label(0.0), "0");
        assert_eq!(pct_label(37.5), "37.5");
    }

    #[test]
    fn events_serialize_per_contract() {
        let reference = AccountRef {
            number: Some(1),
            email: "a@x".into(),
        };
        let mut headroom = BTreeMap::new();
        headroom.insert(1, Some(38.0));
        headroom.insert(2, None);
        headroom.insert(3, Some(11.0));
        let mut fetch_errors = BTreeMap::new();
        fetch_errors.insert(2, "http-429".to_string());
        let mut windows = BTreeMap::new();
        windows.insert(1, vec![("5h".to_string(), 62.0), ("7d".to_string(), 10.0)]);
        windows.insert(3, vec![("5h".to_string(), 3.0), ("7d".to_string(), 89.0)]);
        let poll = Event::Poll {
            active: Some(reference.clone()),
            headroom,
            threshold: 90.0,
            fetch_errors,
            windows,
        };
        let json = poll.to_json(Provider::Claude);
        assert_eq!(json["schemaVersion"], 2);
        assert_eq!(json["provider"], "claude");
        assert_eq!(json["event"], "poll");
        let ts = json["ts"].as_str().unwrap();
        assert!(ts.ends_with('Z') && ts.len() == 20, "{ts}");
        assert_eq!(json["active"], json!({"number": 1, "email": "a@x"}));
        assert_eq!(
            json["headroomPct"],
            json!({"1": 38.0, "2": null, "3": 11.0})
        );
        assert_eq!(json["threshold"], 90.0);
        assert_eq!(json["fetchErrors"], json!({"2": "http-429"}));
        assert_eq!(json["windowsPct"]["3"], json!({"5h": 3.0, "7d": 89.0}));
        assert_eq!(
            poll.human(),
            "Account-1 (a@x): 62% used (switch at 90%) | others: #2: ? (http-429), #3: 5h 3% · 7d 89%"
        );

        let bare = Event::Poll {
            active: None,
            headroom: BTreeMap::new(),
            threshold: 80.0,
            fetch_errors: BTreeMap::new(),
            windows: BTreeMap::new(),
        };
        let json = bare.to_json(Provider::Claude);
        assert!(json["active"].is_null());
        assert_eq!(json["headroomPct"], json!({}));
        assert!(json.get("fetchErrors").is_none());
        assert!(json.get("windowsPct").is_none());
        assert_eq!(bare.human(), "poll: no active account");

        let mut unknown_active = BTreeMap::new();
        unknown_active.insert(1, None);
        let mut errors = BTreeMap::new();
        errors.insert(1, "timeout".to_string());
        let event = Event::Poll {
            active: Some(reference.clone()),
            headroom: unknown_active,
            threshold: 90.0,
            fetch_errors: errors,
            windows: BTreeMap::new(),
        };
        assert_eq!(
            event.human(),
            "Account-1 (a@x): usage unknown (timeout) (switch at 90%)"
        );

        let switch = Event::Switch {
            trigger: Trigger::AtLimit,
            from: Some(reference.clone()),
            to: Some(AccountRef {
                number: Some(2),
                email: "b@x".into(),
            }),
            warnings: vec!["w".into()],
            dry_run: false,
        };
        let json = switch.to_json(Provider::Claude);
        assert_eq!(json["event"], "switch");
        assert_eq!(json["trigger"], "at-limit");
        assert_eq!(json["from"]["number"], 1);
        assert_eq!(json["to"]["email"], "b@x");
        assert_eq!(json["warnings"], json!(["w"]));
        assert_eq!(json["dryRun"], false);
        assert_eq!(
            switch.human(),
            "Switched Account-1 -> Account-2 (b@x) (at-limit)"
        );
        let dry = Event::Switch {
            trigger: Trigger::Proactive,
            from: None,
            to: None,
            warnings: vec![],
            dry_run: true,
        };
        assert_eq!(dry.to_json(Provider::Claude)["dryRun"], true);
        assert!(dry.to_json(Provider::Claude)["from"].is_null());
        assert_eq!(
            dry.human(),
            "[dry-run] would switch (none) -> ? (proactive)"
        );

        let no_switch = Event::NoSwitch {
            reason: "below-threshold".into(),
            detail: "62% < 90%".into(),
        };
        assert_eq!(
            no_switch.to_json(Provider::Claude)["reason"],
            "below-threshold"
        );
        assert_eq!(no_switch.to_json(Provider::Claude)["detail"], "62% < 90%");
        assert_eq!(no_switch.human(), "no switch: below-threshold (62% < 90%)");
        let plain = Event::NoSwitch {
            reason: "no-viable-target".into(),
            detail: String::new(),
        };
        assert_eq!(plain.to_json(Provider::Claude)["detail"], "");
        assert_eq!(plain.human(), "no switch: no-viable-target");

        let quarantined = Event::AccountQuarantined {
            number: 3,
            email: "c@x".into(),
            reason: "invalid_grant".into(),
        };
        let json = quarantined.to_json(Provider::Claude);
        assert_eq!(json["event"], "account-quarantined");
        assert_eq!(json["number"], "3", "number is a string");
        assert_eq!(
            quarantined.human(),
            "Account-3 (c@x) quarantined: invalid_grant. Log in with it and run 'ccsw add --slot 3' to recover."
        );
        let back = Event::AccountUnquarantined {
            number: 3,
            email: "c@x".into(),
            reason: "credentials-replaced".into(),
        };
        assert_eq!(
            back.to_json(Provider::Claude)["event"],
            "account-unquarantined"
        );
        assert_eq!(
            back.human(),
            "Account-3 (c@x) back in rotation (credentials-replaced)"
        );

        let exhausted = Event::AllExhausted {
            earliest_reset_at: Some("2026-09-29T12:00:00Z".into()),
        };
        assert_eq!(
            exhausted.to_json(Provider::Claude)["earliestResetAt"],
            "2026-09-29T12:00:00Z"
        );
        assert_eq!(
            exhausted.human(),
            "all accounts exhausted; earliest reset 2026-09-29T12:00:00Z"
        );
        let unknown = Event::AllExhausted {
            earliest_reset_at: None,
        };
        assert!(unknown.to_json(Provider::Claude)["earliestResetAt"].is_null());
        assert_eq!(
            unknown.human(),
            "all accounts exhausted; no reset time known"
        );

        let sleep = Event::Sleep {
            seconds: 599.96,
            until: "2026-09-29T12:10:00Z".into(),
        };
        assert_eq!(sleep.to_json(Provider::Claude)["seconds"], 600.0);
        let other = sleep.to_json(Provider::Codex);
        assert_eq!(other["provider"], "codex");
        assert_eq!(other["schemaVersion"], 2);
        for key in ["event", "ts", "schemaVersion", "provider"] {
            assert!(other.get(key).is_some());
        }
        assert_eq!(sleep.human(), "sleeping 10m (until 2026-09-29T12:10:00Z)");

        let error = Event::Error {
            message: "boom".into(),
            transient: true,
        };
        assert_eq!(error.to_json(Provider::Claude)["transient"], true);
        assert_eq!(error.human(), "error: boom (will retry)");
        let fatal = Event::Error {
            message: "boom".into(),
            transient: false,
        };
        assert_eq!(fatal.human(), "error: boom");
        let warning = Event::ConfigWarning {
            message: "typo?".into(),
        };
        assert_eq!(warning.to_json(Provider::Claude)["event"], "config-warning");
        assert_eq!(warning.human(), "warning: typo?");
    }

    // ---- ranking ----

    fn entries_from(rows: &[(u32, Option<NormalizedUsage>)]) -> BTreeMap<u32, UsageEntry> {
        rows.iter()
            .map(|(slot, usage)| (*slot, entry_with(usage.clone(), 10.0)))
            .collect()
    }

    #[test]
    fn best_proactive_requires_landing_below_threshold_and_beating_hysteresis() {
        let settings = AutoSwitchSettings::default(); // threshold 90, hysteresis 10
        // active 1 at 92% used (headroom 8); candidates: 2 = 85% (h 15: beats by 7 → no),
        // 3 = 70% (h 30 → yes), 4 = 95% (h 5, over threshold), 5 = 50% (h 50 → yes), 6 unknown.
        let entries = entries_from(&[
            (1, Some(usage(92.0, 10.0, None))),
            (2, Some(usage(85.0, 10.0, None))),
            (3, Some(usage(70.0, 10.0, None))),
            (4, Some(usage(95.0, 10.0, None))),
            (5, Some(usage(50.0, 10.0, None))),
            (6, None),
        ]);
        let ranked = rank_candidates(
            Trigger::Proactive,
            false,
            &[2, 3, 4, 5, 6],
            &entries,
            1,
            Some(8.0),
            &settings,
            &[],
            1_000_000.0,
        );
        assert_eq!(ranked.ordered, vec![5, 3]);
        assert!(ranked.any_known);
        assert_eq!(ranked.active_reset_ts, None);

        // At limit: only headroom > 0 matters; ties keep sequence order.
        let entries = entries_from(&[
            (1, Some(usage(100.0, 10.0, None))),
            (2, Some(usage(95.0, 10.0, None))),
            (3, Some(usage(100.0, 10.0, None))),
            (4, Some(usage(95.0, 10.0, None))),
            (5, Some(usage(60.0, 10.0, None))),
        ]);
        let ranked = rank_candidates(
            Trigger::AtLimit,
            false,
            &[2, 3, 4, 5],
            &entries,
            1,
            Some(0.0),
            &settings,
            &[],
            1_000_000.0,
        );
        assert_eq!(ranked.ordered, vec![5, 2, 4]);
        let ranked = rank_candidates(
            Trigger::Failover,
            false,
            &[2, 3, 4, 5],
            &entries,
            1,
            None,
            &settings,
            &[],
            1_000_000.0,
        );
        assert_eq!(ranked.ordered, vec![5, 2, 4]);

        // Nothing readable.
        let ranked = rank_candidates(
            Trigger::AtLimit,
            false,
            &[6],
            &entries_from(&[(6, None)]),
            1,
            Some(0.0),
            &settings,
            &[],
            1_000_000.0,
        );
        assert!(ranked.ordered.is_empty() && !ranked.any_known);
    }

    #[test]
    fn consume_first_prefers_the_soonest_weekly_reset_with_room() {
        let settings = AutoSwitchSettings {
            strategy: "consume-first".into(),
            ..AutoSwitchSettings::default()
        };
        let now = 1_000_000.0;
        // Active resets in 5 days; 2 resets in 2 days, 3 in 1 day, 4 in 6 days, 5 no reset, 6 full.
        let day = 86_400;
        let entries = entries_from(&[
            (1, Some(usage(20.0, 30.0, Some(1_000_000 + 5 * day)))),
            (2, Some(usage(20.0, 30.0, Some(1_000_000 + 2 * day)))),
            (3, Some(usage(20.0, 30.0, Some(1_000_000 + day)))),
            (4, Some(usage(20.0, 30.0, Some(1_000_000 + 6 * day)))),
            (5, Some(usage(20.0, 30.0, None))),
            (6, Some(usage(100.0, 30.0, Some(1_000_000 + day / 2)))),
        ]);
        let ranked = rank_candidates(
            Trigger::ConsumeFirst,
            true,
            &[2, 3, 4, 5, 6],
            &entries,
            1,
            Some(70.0),
            &settings,
            &[],
            now,
        );
        assert_eq!(ranked.ordered, vec![3, 2]);
        assert_eq!(ranked.active_reset_ts, Some(1_000_000 + 5 * day + day));

        // Over the threshold under consume-first: landing gate only, ordered by reset then headroom.
        let ranked = rank_candidates(
            Trigger::Proactive,
            true,
            &[2, 3, 4, 5, 6],
            &entries,
            1,
            Some(5.0),
            &settings,
            &[],
            now,
        );
        assert_eq!(ranked.ordered, vec![3, 2, 4, 5]);
    }

    #[test]
    fn earliest_recovery_needs_every_exhausted_reset() {
        let now = 1_000_000.0;
        let entries = entries_from(&[
            (1, Some(usage(100.0, 10.0, Some(1_000_500)))),
            (2, Some(usage(100.0, 10.0, Some(1_000_200)))),
            (3, Some(usage(40.0, 10.0, None))),
            (4, None),
        ]);
        assert_eq!(earliest_recovery(&entries, &[], now), Some(1_000_200));
        let mut unknown = entries.clone();
        unknown.insert(5, entry_with(Some(usage(100.0, 10.0, None)), 10.0));
        assert_eq!(earliest_recovery(&unknown, &[], now), None);
        let mut past = entries.clone();
        past.insert(5, entry_with(Some(usage(100.0, 10.0, Some(999_000))), 10.0));
        assert_eq!(earliest_recovery(&past, &[], now), None);
        assert_eq!(
            earliest_recovery(
                &entries_from(&[(3, Some(usage(40.0, 10.0, None)))]),
                &[],
                now
            ),
            None
        );
    }

    // ---- ticks ----

    #[test]
    fn no_login_and_unmanaged_login_take_no_action() {
        let mut fixture = Fixture::new(&[1, 2]);
        fixture.fake.current = CurrentAccount::NoLogin;
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::NoAction);
        assert_eq!(kinds(&events), ["poll", "no-switch"]);
        assert!(matches!(&events[0], Event::Poll { active: None, .. }));
        assert_eq!(
            no_switch_reason(&events),
            Some((
                "no-active-account".into(),
                "log in and run 'ccsw add' first".into()
            ))
        );
        fixture.fake.current = CurrentAccount::Unmanaged {
            email: "x@y".into(),
        };
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::NoAction);
        assert_eq!(
            no_switch_reason(&events).unwrap().0,
            "unmanaged-active-account"
        );
    }

    #[test]
    fn below_threshold_holds_and_fetches_nothing() {
        let mut fixture = Fixture::new(&[1, 2]);
        fixture.seed(1, usage(62.0, 10.0, None));
        fixture.seed(2, usage(5.0, 3.0, None));
        let before = attempts(&fixture, 2);
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::NoAction);
        assert_eq!(kinds(&events), ["poll", "no-switch"]);
        assert_eq!(
            no_switch_reason(&events),
            Some(("below-threshold".into(), "62% < 90%".into()))
        );
        let Event::Poll {
            active,
            headroom,
            windows,
            ..
        } = &events[0]
        else {
            panic!("poll first");
        };
        assert_eq!(active.as_ref().unwrap().number, Some(1));
        assert_eq!(headroom[&1], Some(38.0));
        assert_eq!(headroom[&2], Some(95.0));
        assert_eq!(
            windows[&2],
            vec![("5h".to_string(), 5.0), ("7d".to_string(), 3.0)]
        );
        assert_eq!(
            attempts(&fixture, 2),
            before,
            "fresh rows are served, not fetched"
        );
        assert!(fixture.fake.switches.is_empty());
    }

    #[test]
    fn proactive_switch_records_state_and_dry_run_writes_nothing() {
        let mut fixture = Fixture::new(&[1, 2, 3]);
        fixture.seed(1, usage(95.0, 10.0, None));
        fixture.seed(2, usage(40.0, 10.0, None));
        fixture.seed(3, usage(20.0, 10.0, None));

        let (outcome, events) = tick(&mut fixture, defaults(), true);
        assert_eq!(outcome, TickOutcome::Switched);
        assert_eq!(kinds(&events), ["poll", "switch"]);
        assert!(matches!(
            &events[1],
            Event::Switch { trigger: Trigger::Proactive, dry_run: true, to: Some(to), .. } if to.number == Some(3)
        ));
        assert!(fixture.fake.switches.is_empty());
        assert!(
            !fixture.fake.store.paths.state_file().exists(),
            "dry-run writes no state"
        );

        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::Switched);
        assert_eq!(fixture.fake.switches, vec![3]);
        let Event::Switch {
            trigger,
            from,
            to,
            warnings,
            dry_run,
        } = &events[1]
        else {
            panic!("switch event");
        };
        assert_eq!(*trigger, Trigger::Proactive);
        assert_eq!(from.as_ref().unwrap().number, Some(1));
        assert_eq!(to.as_ref().unwrap().email, "a3@example.com");
        assert_eq!(warnings, &vec!["w1".to_string()]);
        assert!(!dry_run);
        let state = state::read(&fixture.fake.store.paths);
        assert!(state.last_switch_at.unwrap() >= fixture.now);
        assert_eq!(state.last_switch_to.as_deref(), Some("3"));
        assert_eq!(state.last_switch_from, Some(1));
    }

    #[test]
    fn records_of_the_other_provider_are_never_candidates() {
        // A Codex engine ignores a Claude record …
        let mut fixture = Fixture::new(&[1, 2, 3]);
        let record = fixture.fake.roster.record_mut(3).unwrap();
        record.provider = Provider::Claude;
        credentials::write(&fixture.fake.store, 3, &claude_slot(3)).unwrap();
        fixture.seed(1, usage(95.0, 10.0, None));
        fixture.seed(2, usage(40.0, 10.0, None));
        fixture.seed(3, usage(5.0, 5.0, None));
        let (outcome, events) = tick(&mut fixture, defaults(), true);
        assert_eq!(outcome, TickOutcome::Switched);
        let Event::Poll { headroom, .. } = &events[0] else {
            panic!("poll event");
        };
        assert!(!headroom.contains_key(&3), "the Claude slot is not ranked");
        assert!(matches!(
            &events[1],
            Event::Switch { to: Some(to), .. } if to.number == Some(2)
        ));

        // … and a Claude engine ignores a Codex record, even the best one.
        let mut fixture = Fixture::claude(&[1, 2, 3]);
        let record = fixture.fake.roster.record_mut(3).unwrap();
        record.provider = Provider::Codex;
        credentials::write(&fixture.fake.store, 3, &auth("a3", FAR, Some("rt")).0).unwrap();
        fixture.seed(1, usage(95.0, 10.0, None));
        fixture.seed(2, usage(40.0, 10.0, None));
        fixture.seed(3, usage(5.0, 5.0, None));
        let (outcome, events) = tick(&mut fixture, defaults(), true);
        assert_eq!(outcome, TickOutcome::Switched);
        let Event::Poll { headroom, .. } = &events[0] else {
            panic!("poll event");
        };
        assert!(!headroom.contains_key(&3), "the Codex slot is not ranked");
        assert!(matches!(
            &events[1],
            Event::Switch { to: Some(to), .. } if to.number == Some(2)
        ));
        assert!(fixture.fake.switches.is_empty(), "dry-run");
    }

    #[test]
    fn idle_detail_names_the_engine_provider() {
        let mut fixture = Fixture::claude(&[1, 2]);
        fixture.seed_failure(1, "http-401");
        fixture.seed(2, usage(20.0, 10.0, None));
        // An expired live token: the collector derives `token expired` for the
        // active slot when its access token is past expiry and nothing fetched it.
        let expired = {
            use crate::claude::credentials::{ClaudeCredential, OauthAccount, SlotFile};
            let credential = ClaudeCredential::from_value(json!({"claudeAiOauth": {
                "accessToken": "cat-1", "refreshToken": "crt-1", "expiresAt": 1000,
                "scopes": ["user:inference"]
            }}));
            SlotFile::new(&credential, OauthAccount::synthesized("a1@example.com")).to_value()
        };
        credentials::write(&fixture.fake.store, 1, &expired).unwrap();
        let mut settings = defaults();
        settings.unhealthy_ticks = 1;
        let (outcome, events) = tick(&mut fixture, settings, false);
        assert_eq!(outcome, TickOutcome::NoAction);
        assert_eq!(
            no_switch_reason(&events),
            Some((
                "active-idle".into(),
                "token expired while Claude Code is idle; resumes on next use".into()
            ))
        );
        assert!(fixture.fake.switches.is_empty());
    }

    #[test]
    fn cooldown_holds_proactive_but_not_at_limit() {
        let mut fixture = Fixture::new(&[1, 2]);
        fixture.seed(1, usage(95.0, 10.0, None));
        fixture.seed(2, usage(20.0, 10.0, None));
        state::write(
            &fixture.fake.store.paths,
            &AutoSwitchState {
                last_switch_at: Some(fixture.now - 10.0),
                ..AutoSwitchState::default()
            },
        )
        .unwrap();
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::NoAction);
        let (reason, detail) = no_switch_reason(&events).unwrap();
        assert_eq!(reason, "cooldown");
        assert!(detail.ends_with("s remaining"), "{detail}");
        assert!(fixture.fake.switches.is_empty());

        fixture.seed(1, usage(100.0, 10.0, None));
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::Switched);
        assert!(matches!(
            &events[1],
            Event::Switch {
                trigger: Trigger::AtLimit,
                ..
            }
        ));
        assert_eq!(fixture.fake.switches, vec![2]);
    }

    #[test]
    fn unknown_active_usage_fails_over_after_unhealthy_ticks() {
        let mut fixture = Fixture::new(&[1, 2]);
        fixture.seed_failure(1, "timeout");
        fixture.seed(2, usage(20.0, 10.0, None));
        let mut settings = defaults();
        settings.unhealthy_ticks = 2;
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        let mut engine = Engine::new(
            &mut fixture.fake,
            Provider::Codex,
            settings,
            false,
            move |event| sink_log.borrow_mut().push(event.clone()),
        );
        assert_eq!(engine.tick(), TickOutcome::NoAction);
        let events = log.borrow().clone();
        assert_eq!(
            no_switch_reason(&events),
            Some(("active-usage-unknown".into(), "1/2 before failover".into()))
        );
        let Event::Poll { fetch_errors, .. } = &events[0] else {
            panic!("poll");
        };
        assert_eq!(fetch_errors[&1], "timeout");
        log.borrow_mut().clear();
        assert_eq!(engine.tick(), TickOutcome::Switched);
        let events = log.borrow().clone();
        assert!(matches!(
            &events[1],
            Event::Switch {
                trigger: Trigger::Failover,
                ..
            }
        ));
        drop(engine);
        assert_eq!(fixture.fake.switches, vec![2]);
    }

    #[test]
    fn expired_active_token_idles_instead_of_counting_unhealthy() {
        let mut fixture = Fixture::new(&[1, 2]);
        let expired = auth("a1", 1, Some("rt"));
        expired
            .write(&fixture.fake.store.paths.live_auth_file())
            .unwrap();
        fixture.seed_failure(1, "http-401");
        fixture.seed(2, usage(20.0, 10.0, None));
        let mut settings = defaults();
        settings.unhealthy_ticks = 1;
        let (outcome, events) = tick(&mut fixture, settings.clone(), false);
        assert_eq!(outcome, TickOutcome::NoAction);
        assert_eq!(
            no_switch_reason(&events),
            Some((
                "active-idle".into(),
                "token expired while Codex is idle; resumes on next use".into()
            ))
        );
        assert!(fixture.fake.switches.is_empty());

        // Past the idle-hold cap the account counts as unhealthy again.
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        let base = fixture.now;
        let clock = Rc::new(RefCell::new(base));
        let clock_ref = clock.clone();
        let mut engine = Engine::new(
            &mut fixture.fake,
            Provider::Codex,
            settings,
            false,
            move |event| sink_log.borrow_mut().push(event.clone()),
        )
        .with_clock(move || *clock_ref.borrow());
        assert_eq!(engine.tick(), TickOutcome::NoAction);
        *clock.borrow_mut() = base + IDLE_HOLD_MAX_S + 1.0;
        assert_eq!(engine.tick(), TickOutcome::Switched);
        drop(engine);
        assert_eq!(fixture.fake.switches, vec![2]);
    }

    #[test]
    fn keychain_unavailable_active_holds_like_token_expired() {
        use crate::claude::keychain::test_support::FakeSecurity;
        let mut fixture = Fixture::claude(&[1, 2]);
        // The Keychain is the live backend and cannot be read; nothing on disk
        // covers the login, so the active slot reports `keychain unavailable`.
        fixture.fake.store.paths.keychain_enabled = true;
        {
            let paths = &fixture.fake.store.paths;
            std::fs::create_dir_all(&paths.claude_home).unwrap();
            crate::fsutil::write_json_private(
                &paths.claude_global_config_file(),
                &json!({"oauthAccount": {
                    "emailAddress": "a1@example.com", "accountUuid": "u1",
                    "organizationUuid": "a1", "organizationName": null
                }}),
            )
            .unwrap();
        }
        fixture.seed(2, usage(20.0, 10.0, None));
        let mut settings = defaults();
        settings.unhealthy_ticks = 1;
        let security = FakeSecurity { failing: true };
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        let outcome = {
            let clock = Rc::new(RefCell::new(0.0));
            let clock_ref = clock.clone();
            let mut engine = Engine::new(
                &mut fixture.fake,
                Provider::Claude,
                settings,
                false,
                move |event| sink_log.borrow_mut().push(event.clone()),
            )
            .with_security(&security)
            .with_clock(move || *clock_ref.borrow());
            assert_eq!(engine.tick(), TickOutcome::NoAction);
            *clock.borrow_mut() = IDLE_HOLD_MAX_S + 1.0;
            engine.tick()
        };
        let events = log.borrow().clone();
        assert_eq!(outcome, TickOutcome::NoAction);
        assert!(
            matches!(&events[0], Event::Poll { headroom, .. } if headroom.get(&1) == Some(&None)),
            "the active account's usage is unknown: {events:?}"
        );
        assert_eq!(
            no_switch_reason(&events),
            Some((
                "active-idle".into(),
                "keychain unavailable; holding until Claude Code's login is readable".into()
            ))
        );
        assert!(
            fixture.fake.switches.is_empty(),
            "a cool candidate is not switched in while the login is unreadable"
        );
    }

    #[test]
    fn api_key_active_and_missing_candidates() {
        let mut fixture = Fixture::new(&[1, 2]);
        fixture.fake.current = CurrentAccount::Managed {
            slot: 1,
            email: "a1@example.com".into(),
            api_key: true,
        };
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::NoAction);
        assert_eq!(no_switch_reason(&events).unwrap().0, "active-api-key");

        let mut fixture = Fixture::new(&[1, 2]);
        fixture.seed(1, usage(95.0, 10.0, None));
        fixture.fake.roster.set_disabled(2, true).unwrap();
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        let mut engine = Engine::new(
            &mut fixture.fake,
            Provider::Codex,
            AutoSwitchSettings::default(),
            false,
            move |event| sink_log.borrow_mut().push(event.clone()),
        );
        let outcome = engine.tick();
        assert_eq!(outcome, TickOutcome::Blocked);
        assert_eq!(no_switch_reason(&log.borrow()).unwrap().0, "no-candidates");
        assert_eq!(
            engine.next_delay(outcome),
            300.0,
            "blocked without candidates waits long"
        );
    }

    #[test]
    fn unreadable_or_unqualified_candidates_block_at_normal_cadence() {
        let mut fixture = Fixture::new(&[1, 2]);
        fixture.seed(1, usage(95.0, 10.0, None));
        fixture.seed_failure(2, "network");
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::Blocked);
        assert_eq!(no_switch_reason(&events).unwrap().0, "no-comparison");

        let mut fixture = Fixture::new(&[1, 2]);
        fixture.seed(1, usage(95.0, 10.0, None));
        fixture.seed(2, usage(92.0, 10.0, None));
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::Blocked);
        assert_eq!(
            no_switch_reason(&events).unwrap().0,
            "no-qualifying-candidate"
        );
    }

    #[test]
    fn all_exhausted_sleeps_toward_the_earliest_reset() {
        let mut fixture = Fixture::new(&[1, 2, 3]);
        let now = fixture.now as i64;
        fixture.seed(1, usage(100.0, 10.0, Some(now + 3000)));
        fixture.seed(2, usage(100.0, 10.0, Some(now + 900)));
        fixture.seed(3, usage(100.0, 10.0, Some(now + 5000)));
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        let settings = AutoSwitchSettings {
            interval_seconds: 60.0,
            ..AutoSwitchSettings::default()
        };
        let mut engine = Engine::new(
            &mut fixture.fake,
            Provider::Codex,
            settings,
            false,
            move |event| sink_log.borrow_mut().push(event.clone()),
        );
        let outcome = engine.tick();
        assert_eq!(outcome, TickOutcome::Blocked);
        let events = log.borrow().clone();
        assert_eq!(kinds(&events), ["poll", "all-exhausted"]);
        assert_eq!(
            events[1],
            Event::AllExhausted {
                earliest_reset_at: Some(format_iso(now + 900))
            }
        );
        let delay = engine.next_delay(outcome);
        assert!(
            (delay - 600.0).abs() < 2.0,
            "capped at MAX_SLEEP_S: {delay}"
        );

        // Unknown reset: long wait, no sleep target.
        drop(engine);
        fixture.seed(2, usage(100.0, 10.0, None));
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        let mut engine = Engine::new(
            &mut fixture.fake,
            Provider::Codex,
            AutoSwitchSettings::default(),
            false,
            move |event| sink_log.borrow_mut().push(event.clone()),
        );
        let outcome = engine.tick();
        assert_eq!(outcome, TickOutcome::Blocked);
        assert_eq!(
            log.borrow()[1],
            Event::AllExhausted {
                earliest_reset_at: None
            }
        );
        assert_eq!(engine.next_delay(outcome), 300.0);
    }

    #[test]
    fn dead_target_is_quarantined_and_released_when_credentials_change() {
        let mut fixture = Fixture::new(&[1, 2, 3]);
        fixture.seed(1, usage(95.0, 10.0, None));
        fixture.seed(2, usage(10.0, 10.0, None));
        fixture.seed(3, usage(30.0, 10.0, None));
        // Slot 2 ranks first but its token is expired with no refresh token.
        let dead = auth("a2", 1, None);
        credentials::write(&fixture.fake.store, 2, &dead.0).unwrap();
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::Switched);
        assert_eq!(kinds(&events), ["poll", "account-quarantined", "switch"]);
        assert_eq!(
            events[1],
            Event::AccountQuarantined {
                number: 2,
                email: "a2@example.com".into(),
                reason: "invalid_grant".into()
            }
        );
        assert_eq!(fixture.fake.switches, vec![3]);
        let state = state::read(&fixture.fake.store.paths);
        let entry = &state.quarantine["2"];
        assert_eq!(entry.reason, "invalid_grant");
        assert_eq!(
            entry.refresh_token_fingerprint,
            credentials::fingerprint(&dead.0)
        );
        assert!(entry.at.ends_with('Z'));

        // Quarantined slots are not candidates: with 1 and 3 exhausted, slot 2's
        // room does not count.
        fixture.seed(1, usage(100.0, 10.0, None));
        fixture.seed(3, usage(100.0, 10.0, None));
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::Blocked);
        assert_eq!(kinds(&events), ["poll", "all-exhausted"]);

        // New credentials release the quarantine at the top of the next tick.
        credentials::write(&fixture.fake.store, 2, &auth("a2", FAR, Some("rt-new")).0).unwrap();
        let (_, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(
            events[0],
            Event::AccountUnquarantined {
                number: 2,
                email: "a2@example.com".into(),
                reason: "credentials-replaced".into()
            }
        );
        assert!(state::read(&fixture.fake.store.paths).quarantine.is_empty());

        // A slot whose record changed identity is released as account-replaced.
        state::modify(&fixture.fake.store, |state| {
            state.quarantine.insert(
                "3".into(),
                QuarantineEntry {
                    email: "old@example.com".into(),
                    reason: "invalid_grant".into(),
                    at: "2026-09-29T00:00:00Z".into(),
                    refresh_token_fingerprint: None,
                },
            );
        })
        .unwrap();
        let (_, events) = tick(&mut fixture, defaults(), false);
        assert!(matches!(
            &events[0],
            Event::AccountUnquarantined { number: 3, reason, .. } if reason == "account-replaced"
        ));
        // Dry-run never releases.
        state::modify(&fixture.fake.store, |state| {
            state.quarantine.insert(
                "3".into(),
                QuarantineEntry {
                    email: "old@example.com".into(),
                    reason: "invalid_grant".into(),
                    at: "2026-09-29T00:00:00Z".into(),
                    refresh_token_fingerprint: None,
                },
            );
        })
        .unwrap();
        let (_, events) = tick(&mut fixture, defaults(), true);
        assert_eq!(events[0].kind(), "poll");
        assert_eq!(state::read(&fixture.fake.store.paths).quarantine.len(), 1);
    }

    #[test]
    fn api_key_candidates_are_a_last_resort_only_when_allowed() {
        let mut fixture = Fixture::new(&[1, 2, 3]);
        fixture.fake.roster.record_mut(3).unwrap().kind = Some(AccountKind::ApiKey);
        credentials::write(&fixture.fake.store, 3, &AuthJson::api_key_auth("sk-3").0).unwrap();
        fixture.seed(1, usage(100.0, 10.0, None));
        fixture.seed(2, usage(100.0, 10.0, None));
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::Blocked);
        assert_eq!(events[1].kind(), "all-exhausted");

        let mut settings = defaults();
        settings.include_api_key_accounts = true;
        let (outcome, events) = tick(&mut fixture, settings, false);
        assert_eq!(outcome, TickOutcome::Switched);
        assert!(matches!(&events[1], Event::Switch { to: Some(to), .. } if to.number == Some(3)));
        assert_eq!(fixture.fake.switches, vec![3]);
    }

    #[test]
    fn switch_errors_become_error_events() {
        let mut fixture = Fixture::new(&[1, 2]);
        fixture.seed(1, usage(100.0, 10.0, None));
        fixture.seed(2, usage(10.0, 10.0, None));
        fixture.fake.fail_switch = true;
        let (outcome, events) = tick(&mut fixture, defaults(), false);
        assert_eq!(outcome, TickOutcome::Error);
        assert_eq!(
            events.last().unwrap(),
            &Event::Error {
                message: "boom".into(),
                transient: true
            }
        );
    }

    #[test]
    fn already_active_target_is_a_no_action() {
        let fixture = Fixture::new(&[1, 2]);
        fixture.seed(1, usage(100.0, 10.0, None));
        fixture.seed(2, usage(10.0, 10.0, None));
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        struct Stubborn(Fake);
        impl AutoFacade for Stubborn {
            fn store(&self) -> &Store {
                self.0.store()
            }
            fn roster(&mut self) -> Result<Roster> {
                self.0.roster()
            }
            fn current_account(&mut self, provider: Provider) -> Result<CurrentAccount> {
                self.0.current_account(provider)
            }
            fn switch_to(&mut self, slot: u32) -> Result<SwitchOutcome> {
                let mut outcome = self.0.switch_to(slot)?;
                outcome.switched = false;
                outcome.reason = "already-active".into();
                Ok(outcome)
            }
        }
        let mut stubborn = Stubborn(fixture.fake);
        let mut engine = Engine::new(
            &mut stubborn,
            Provider::Codex,
            AutoSwitchSettings::default(),
            false,
            move |event| sink_log.borrow_mut().push(event.clone()),
        );
        assert_eq!(engine.tick(), TickOutcome::NoAction);
        assert_eq!(
            no_switch_reason(&log.borrow()),
            Some(("already-active".into(), "already-active".into()))
        );
    }

    #[test]
    fn next_delay_follows_the_loop_rules() {
        let mut fixture = Fixture::new(&[1, 2]);
        fixture.seed(1, usage(62.0, 10.0, None));
        fixture.seed(2, usage(10.0, 10.0, None));
        let settings = AutoSwitchSettings {
            interval_seconds: 100.0,
            ..AutoSwitchSettings::default()
        };
        let mut engine = Engine::new(&mut fixture.fake, Provider::Codex, settings, false, |_| {});
        assert_eq!(engine.tick(), TickOutcome::NoAction);
        let delay = engine.next_delay(TickOutcome::NoAction);
        assert!((90.0..=110.0).contains(&delay), "{delay}");
        // A near poll plan on the active row shortens the delay, never below 60 s.
        engine.active_next_poll_at = Some(engine.now() + 10.0);
        let delay = engine.next_delay(TickOutcome::NoAction);
        assert!((delay - 60.0).abs() < 1e-6, "{delay}");
        engine.active_next_poll_at = Some(engine.now() + 80.0);
        let delay = engine.next_delay(TickOutcome::Switched);
        assert!((79.0..=81.0).contains(&delay), "{delay}");
        engine.active_next_poll_at = None;
        engine.idle_hold_slow = true;
        assert_eq!(engine.next_delay(TickOutcome::NoAction), 300.0);
        engine.idle_hold_slow = false;
        engine.blocked_wait_long = true;
        assert_eq!(engine.next_delay(TickOutcome::Blocked), 300.0);
        engine.sleep_until = Some(engine.now() + 5000.0);
        assert!((engine.next_delay(TickOutcome::Blocked) - 600.0).abs() < 1.0);
        engine.sleep_until = Some(engine.now() + 10.0);
        assert!((engine.next_delay(TickOutcome::Blocked) - 100.0).abs() < 1.0);
        engine.settings.interval_seconds = 900.0;
        assert!(
            (engine.next_delay(TickOutcome::Blocked) - 900.0).abs() < 1.0,
            "interval floor wins over the cap"
        );
    }

    #[test]
    fn run_loop_stops_on_the_flag() {
        let mut fixture = Fixture::new(&[1, 2]);
        fixture.seed(1, usage(62.0, 10.0, None));
        fixture.seed(2, usage(10.0, 10.0, None));
        let stop = Arc::new(AtomicBool::new(false));
        let stopper = stop.clone();
        let polls = Rc::new(RefCell::new(0));
        let counter = polls.clone();
        let mut engine = Engine::new(
            &mut fixture.fake,
            Provider::Codex,
            AutoSwitchSettings::default(),
            false,
            move |event| {
                if event.kind() == "poll" {
                    *counter.borrow_mut() += 1;
                    stopper.store(true, Ordering::SeqCst);
                }
            },
        );
        let started = Instant::now();
        assert_eq!(engine.run_loop(stop), 0);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(*polls.borrow(), 1);
    }

    #[test]
    fn cli_parses_flags_and_runs_once() {
        let mut fixture = Fixture::claude(&[1, 2]);
        fixture.seed(1, usage(62.0, 10.0, None));
        fixture.seed(2, usage(10.0, 10.0, None));
        let argv = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            run_cli(
                argv(&["--once", "--json", "--threshold", "50", "--dry-run"]),
                &mut fixture.fake
            ),
            TickOutcome::Switched.code()
        );
        assert!(fixture.fake.switches.is_empty());
        assert_eq!(
            run_cli(argv(&["--once", "--strategy", "bogus"]), &mut fixture.fake),
            2
        );
        assert_eq!(run_cli(argv(&["--once", "--nope"]), &mut fixture.fake), 2);
        assert_eq!(run_cli(argv(&["--help"]), &mut fixture.fake), 0);
        assert_eq!(
            run_cli(
                argv(&[
                    "--once",
                    "--include-api-key-accounts",
                    "--no-include-api-key-accounts"
                ]),
                &mut fixture.fake
            ),
            2
        );
        let args = AutoArgs::try_parse_from(argv(&[
            "ccsw auto",
            "--interval",
            "1",
            "--cooldown",
            "5",
            "--model",
            "Spark,all",
            "--no-include-api-key-accounts",
            "--strategy",
            "consume-first",
        ]))
        .unwrap();
        let merged = AutoSwitchSettings::default().merged_with_cli(&args.overrides());
        assert_eq!(merged.interval_seconds, 15.0, "re-clamped");
        assert_eq!(merged.cooldown_seconds, 5.0);
        assert_eq!(merged.model_names(), vec!["Spark", "all"]);
        assert_eq!(merged.strategy, "consume-first");
        assert!(!merged.include_api_key_accounts);
        assert_eq!(
            AutoArgs::try_parse_from(argv(&["ccsw auto", "--include-api-key-accounts"]))
                .unwrap()
                .overrides()
                .include_api_key_accounts,
            Some(true)
        );
        assert_eq!(
            AutoArgs::try_parse_from(argv(&["ccsw auto"]))
                .unwrap()
                .overrides()
                .include_api_key_accounts,
            None
        );
    }
    #[test]
    fn auto_refuses_codex_and_rosters_without_a_claude_account() {
        let argv = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A Codex-only roster: refused before any tick (no poll event, exit 1).
        let mut codex_only = Fixture::new(&[1, 2]);
        codex_only.seed(1, usage(95.0, 10.0, None));
        codex_only.seed(2, usage(10.0, 10.0, None));
        let mut out = Vec::new();
        assert_eq!(
            run_cli_to(argv(&["--once", "--json"]), &mut codex_only.fake, &mut out),
            1
        );
        assert!(
            out.is_empty(),
            "no event is written: {}",
            String::from_utf8_lossy(&out)
        );
        assert!(codex_only.fake.switches.is_empty());

        // `auto codex` is refused even with Claude accounts around.
        let mut claude = Fixture::claude(&[1, 2]);
        claude.seed(1, usage(95.0, 10.0, None));
        claude.seed(2, usage(10.0, 10.0, None));
        let mut out = Vec::new();
        assert_eq!(
            run_cli_to(
                argv(&["codex", "--once", "--json"]),
                &mut claude.fake,
                &mut out
            ),
            1
        );
        assert!(out.is_empty());

        // `auto claude` is the bare form.
        let mut out = Vec::new();
        assert_eq!(
            run_cli_to(
                argv(&["claude", "--once", "--json", "--dry-run"]),
                &mut claude.fake,
                &mut out
            ),
            TickOutcome::Switched.code()
        );
        assert!(String::from_utf8_lossy(&out).contains("\"event\":\"switch\""));
        assert!(claude.fake.switches.is_empty(), "dry-run");
    }
    fn usage_with_pool(five_hour: f64, pool: &str) -> NormalizedUsage {
        let mut u = usage(five_hour, 10.0, None);
        u.scoped = vec![crate::model::ScopedWindow {
            name: pool.to_string(),
            pct: 10.0,
            resets_at: None,
        }];
        u
    }

    fn config_warnings(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::ConfigWarning { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn model_typo_warns_once_and_known_names_never_do() {
        let mut fixture = Fixture::claude(&[1, 2]);
        fixture.seed(1, usage_with_pool(50.0, "Fable"));
        fixture.seed(2, usage_with_pool(10.0, "Fable"));

        // A typo warns exactly once per run, and the engine keeps watching 5h/7d.
        let mut settings = defaults();
        settings.model = Some("Fabel".into());
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        {
            let mut engine = Engine::new(
                &mut fixture.fake,
                Provider::Claude,
                settings,
                true,
                move |event| sink_log.borrow_mut().push(event.clone()),
            );
            assert_eq!(engine.tick(), TickOutcome::NoAction, "50% < 90%");
            assert_eq!(engine.tick(), TickOutcome::NoAction);
        }
        let events = log.borrow().clone();
        assert_eq!(
            config_warnings(&events),
            vec![
                "autoswitch.model: Fabel matches no account's usage windows — only the 5h/7d limits are being watched for it (typo?)"
                    .to_string()
            ],
            "{events:?}"
        );
        assert_eq!(
            kinds(&events)[..2],
            ["poll", "config-warning"],
            "the warning follows the first poll"
        );

        // A known name, `all`, and a mixed list warn only for the unknown part.
        for (model, expected) in [
            ("Fable", Vec::<String>::new()),
            ("all", Vec::new()),
            ("FABLE,all", Vec::new()),
            ("Fable,Opus", vec!["autoswitch.model: Opus matches no account's usage windows — only the 5h/7d limits are being watched for it (typo?)".to_string()]),
        ] {
            let mut settings = defaults();
            settings.model = Some(model.into());
            let (_, events) = tick(&mut fixture, settings, true);
            assert_eq!(config_warnings(&events), expected, "model = {model}");
        }

        // An unreadable slot defers the check instead of guessing.
        let mut fixture = Fixture::claude(&[1, 2]);
        fixture.seed(1, usage_with_pool(50.0, "Fable"));
        fixture.seed_failure(2, "http-500");
        let mut settings = defaults();
        settings.model = Some("Fabel".into());
        let (_, events) = tick(&mut fixture, settings, true);
        assert!(config_warnings(&events).is_empty(), "{events:?}");
    }

    #[test]
    fn model_typo_waits_for_readable_usage_then_warns() {
        let mut fixture = Fixture::claude(&[1, 2]);
        fixture.seed(1, usage_with_pool(50.0, "Fable"));
        fixture.seed(2, usage_with_pool(10.0, "Fable"));
        // A cached measurement cannot make missing credentials readable.
        credentials::write(&fixture.fake.store, 2, &json!({})).unwrap();
        let mut settings = defaults();
        settings.model = Some("Fabel".into());
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        let mut engine = Engine::new(
            &mut fixture.fake,
            Provider::Claude,
            settings,
            true,
            move |event| sink_log.borrow_mut().push(event.clone()),
        );
        assert_eq!(engine.tick(), TickOutcome::NoAction);
        assert!(config_warnings(&log.borrow()).is_empty());
        credentials::write(engine.facade.store(), 2, &claude_slot(2)).unwrap();
        assert_eq!(engine.tick(), TickOutcome::NoAction);
        assert_eq!(config_warnings(&log.borrow()).len(), 1);
        assert_eq!(engine.tick(), TickOutcome::NoAction);
        assert_eq!(config_warnings(&log.borrow()).len(), 1);
    }
}
