//! `cache/usage.json` (schema 2): the last good measurement per slot, failure
//! backoff, dead-token strikes and the adaptive poll plan every surface shares
//! (spec §8.4, research notes `cswap-model-autoswitch.md` §1.7 and §4.3–4.6).
//!
//! Sentinels (`no credentials`, `token expired`, …) are derived by the caller on
//! every pass and never persisted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{CcswError, Result};
use crate::fsutil::{self, FileLock};
use crate::model::{Identity, NormalizedUsage};
use crate::paths::Paths;
use crate::store::poll_policy::{EDGE_BACKOFF_S, RECENT_429_WINDOW_S, SERVE_TTL_S};
use crate::usage_math::earliest_future_reset_ts;

pub const SCHEMA_VERSION: u32 = 2;
/// A last-good measurement drives decisions up to this age unconditionally.
pub const STALE_OK_S: f64 = 300.0;
/// A reserved row is a live fetch claim for this long after `lastAttemptAt`.
pub const CLAIM_TTL_S: f64 = 90.0;
/// Ceiling on trust extension after ordinary failures.
pub const TRUST_MAX_AGE_S: f64 = 3600.0;
/// Ceiling on trust extension after a 429: usage is monotone inside a window,
/// so a throttled last-good stays a valid lower bound until a window rolls over.
pub const RATE_LIMIT_TRUST_MAX_AGE_S: f64 = 7200.0;
pub const BACKOFF_BASE_S: f64 = 30.0;
pub const BACKOFF_CAP_S: f64 = 600.0;
const BACKOFF_MAX_SHIFT: u32 = 32;
/// Added to a 429 `Retry-After` above the cap: the server's ask is a floor, not
/// a promise.
pub const RETRY_AFTER_MARGIN_S: f64 = 900.0;
pub const RETRY_AFTER_FLOOR_CAP_S: f64 = 4500.0;
pub const AUTH_DEAD_STRIKES: u32 = 1;
pub const RATE_LIMIT_ERROR: &str = "http-429";
/// A plan sleeping past this much beyond its interval is an obsolete reset-parked
/// plan and gets repaired.
const OVERSLEEP_FLOOR_S: f64 = 600.0;

/// Why a slot has no live measurement this pass. Never written to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSentinel {
    NoCredentials,
    TokenExpired,
    ApiKey,
    ReloginNeeded,
    KeychainUnavailable,
}

impl UsageSentinel {
    /// The human list/status text.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NoCredentials => "no credentials",
            Self::TokenExpired => {
                "token expired — refresh deferred this pass; retries automatically"
            }
            Self::ApiKey => "API key (no quota)",
            Self::ReloginNeeded => {
                "re-login needed — refresh token dead; log in with Codex, then run: ccsw add"
            }
            Self::KeychainUnavailable => "keychain unavailable — locked or in use; try again",
        }
    }

    /// The JSON `usageStatus` value.
    pub fn usage_status(&self) -> &'static str {
        match self {
            Self::NoCredentials => "no_credentials",
            Self::TokenExpired => "token_expired",
            Self::ApiKey => "api_key",
            Self::ReloginNeeded => "relogin_required",
            Self::KeychainUnavailable => "keychain_unavailable",
        }
    }
}

/// One row as read at a given `now`.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageEntry {
    /// Set by the caller after derivation; `None` from the store.
    pub sentinel: Option<UsageSentinel>,
    pub last_good: Option<NormalizedUsage>,
    pub fetched_at: Option<f64>,
    pub age_s: Option<f64>,
    pub last_attempt_at: Option<f64>,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub backoff_until: Option<f64>,
    pub next_poll_at: Option<f64>,
    pub poll_interval_s: Option<f64>,
    pub last_429_at: Option<f64>,
    pub auth_dead_strikes: u32,
    pub struck_fingerprint: Option<String>,
    /// The last good measurement may drive decisions past `STALE_OK_S`.
    pub trust_extended: bool,
}

impl UsageEntry {
    pub fn fresh(&self, now: f64, ttl: f64) -> bool {
        self.fetched_at.is_some_and(|at| now - at <= ttl)
    }

    pub fn in_backoff(&self, now: f64) -> bool {
        self.backoff_until.is_some_and(|until| now < until)
    }

    pub fn claimed(&self, now: f64) -> bool {
        self.last_attempt_at
            .is_some_and(|at| now - at < CLAIM_TTL_S)
    }

    /// Recency is measured from when an honored 429 backoff lifts, not from
    /// the 429 itself.
    pub fn recent_429(&self, now: f64) -> bool {
        let Some(last_429) = self.last_429_at else {
            return false;
        };
        let anchor = match self.backoff_until {
            Some(until) if self.rate_limited() && until > last_429 => until,
            _ => last_429,
        };
        now < anchor + RECENT_429_WINDOW_S
    }

    /// Dead unless the stored credential is a different generation than the
    /// one the strike condemned. A strike without a fingerprint binds always.
    pub fn token_dead(&self, threshold: u32, stored_fp: Option<&str>) -> bool {
        if self.auth_dead_strikes < threshold {
            return false;
        }
        !matches!(
            (stored_fp, self.struck_fingerprint.as_deref()),
            (Some(stored), Some(struck)) if stored != struck
        )
    }

    /// A `nextPollAt` far beyond the plan's own interval: an obsolete reset-parked plan.
    pub fn overslept(&self, now: f64) -> bool {
        let horizon = self.poll_interval_s.unwrap_or(0.0).max(OVERSLEEP_FLOOR_S) * 1.1 + 60.0;
        self.next_poll_at.is_some_and(|at| at > now + horizon)
    }

    /// The measurement a decision may use: `None` when a sentinel applies (read
    /// `sentinel` instead) or the last good value is too old to trust.
    pub fn decision_value(&self) -> Option<&NormalizedUsage> {
        if self.sentinel.is_some() {
            return None;
        }
        let usable = self.age_s.is_some_and(|age| age <= STALE_OK_S) || self.trust_extended;
        if usable {
            self.last_good.as_ref()
        } else {
            None
        }
    }

    fn rate_limited(&self) -> bool {
        self.last_error.as_deref() == Some(RATE_LIMIT_ERROR)
    }
}

/// How a caller wants rows gated in [`UsageStore::reserve`] (§4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveMode {
    /// `list`, `status`, `switch --strategy`, dashboards: stale and plan-due.
    OnDemand,
    /// The auto engine's scheduled collection: plan-due, or stale without a usable plan.
    Scheduled,
    /// The auto engine near the threshold: plan-due or stale.
    Escalation,
}

/// The outcome of one usage fetch, merged by [`UsageStore::record`].
#[derive(Debug, Clone, PartialEq)]
pub enum FetchRecord {
    Success {
        usage: NormalizedUsage,
        /// `(next_poll_at, interval_s)`, written in the same transaction.
        plan: Option<(f64, f64)>,
    },
    Failure {
        error: String,
        retry_after: Option<f64>,
        /// `invalid_grant`-class verdicts: the refresh token is dead.
        permanent_auth: bool,
        struck_fp: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Row {
    email: String,
    account_id: String,
    last_good: Option<NormalizedUsage>,
    fetched_at: Option<f64>,
    last_attempt_at: Option<f64>,
    consecutive_failures: u32,
    last_error: Option<String>,
    backoff_until: Option<f64>,
    next_poll_at: Option<f64>,
    poll_interval_s: Option<f64>,
    #[serde(rename = "last429At")]
    last_429_at: Option<f64>,
    auth_dead_strikes: u32,
    struck_fingerprint: Option<String>,
}

impl Row {
    fn fresh_for(identity: &Identity) -> Self {
        Self {
            email: identity.email.clone(),
            account_id: identity.account_id.clone(),
            ..Self::default()
        }
    }

    fn matches(&self, identity: &Identity) -> bool {
        self.email == identity.email && self.account_id == identity.account_id
    }

    fn entry(&self, now: f64) -> UsageEntry {
        let age_s = self.fetched_at.map(|at| now - at);
        let mut entry = UsageEntry {
            sentinel: None,
            last_good: self.last_good.clone(),
            fetched_at: self.fetched_at,
            age_s,
            last_attempt_at: self.last_attempt_at,
            consecutive_failures: self.consecutive_failures,
            last_error: self.last_error.clone(),
            backoff_until: self.backoff_until,
            next_poll_at: self.next_poll_at,
            poll_interval_s: self.poll_interval_s,
            last_429_at: self.last_429_at,
            auth_dead_strikes: self.auth_dead_strikes,
            struck_fingerprint: self.struck_fingerprint.clone(),
            trust_extended: false,
        };
        entry.trust_extended = trust_extended(&entry, now);
        entry
    }
}

fn trust_extended(entry: &UsageEntry, now: f64) -> bool {
    let Some(age_s) = entry.age_s else {
        return false;
    };
    let within_ceiling = if entry.rate_limited() {
        // Windows still running at measurement time bound the trust: once the
        // soonest of them rolls over, the throttled last-good says nothing.
        let mut bound = now + (RATE_LIMIT_TRUST_MAX_AGE_S - age_s);
        if let Some(reset) = entry
            .last_good
            .as_ref()
            .zip(entry.fetched_at)
            .and_then(|(usage, fetched_at)| earliest_future_reset_ts(usage, fetched_at as i64, &[]))
        {
            bound = bound.min(reset as f64);
        }
        now < bound
    } else {
        age_s <= TRUST_MAX_AGE_S
    };
    within_ceiling
        && (entry.consecutive_failures > 0
            || entry.next_poll_at.is_some_and(|at| now < at)
            || entry.claimed(now))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Table {
    schema_version: u32,
    #[serde(default)]
    accounts: BTreeMap<String, Row>,
}

impl Table {
    fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            accounts: BTreeMap::new(),
        }
    }

    /// Missing, corrupt, or another schema all read as an empty table.
    fn load(path: &Path) -> Self {
        let Ok(Some(value)) = fsutil::read_json(path) else {
            return Self::empty();
        };
        match serde_json::from_value::<Table>(value) {
            Ok(table) if table.schema_version == SCHEMA_VERSION => table,
            _ => Self::empty(),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        let value = serde_json::to_value(self)
            .map_err(|err| CcswError::config(format!("{}: {err}", path.display())))?;
        fsutil::write_json_private(path, &value)
            .map_err(|err| fsutil::io_error(CcswError::Config, path, &err))
    }

    fn row(&self, slot: u32) -> Option<&Row> {
        self.accounts.get(&slot.to_string())
    }

    fn row_mut(&mut self, slot: u32) -> Option<&mut Row> {
        self.accounts.get_mut(&slot.to_string())
    }

    /// The slot's row when it belongs to `identity`, else a fresh row replacing
    /// whatever was there.
    fn own_row(&mut self, slot: u32, identity: &Identity) -> &mut Row {
        let key = slot.to_string();
        let replace = !self
            .accounts
            .get(&key)
            .is_some_and(|row| row.matches(identity));
        if replace {
            self.accounts.insert(key.clone(), Row::fresh_for(identity));
        }
        self.accounts
            .get_mut(&key)
            .expect("row inserted or present")
    }
}

/// `30·2^(n-1)` capped at 600 s, raised to honor a `Retry-After` ask (§4.4).
pub fn failure_backoff_s(n: u32, retry_after: Option<f64>, rate_limited: bool) -> f64 {
    let shift = n.saturating_sub(1).min(BACKOFF_MAX_SHIFT);
    let computed = (BACKOFF_BASE_S * 2f64.powi(shift as i32)).min(BACKOFF_CAP_S);
    let Some(retry_after) = retry_after else {
        return computed;
    };
    if retry_after <= 0.0 {
        // "Retry now" on a 503 is not the 429 edge; a 429 with 0 is a saturated budget.
        return if rate_limited {
            computed.clamp(EDGE_BACKOFF_S, BACKOFF_CAP_S)
        } else {
            computed
        };
    }
    let mut asked = retry_after;
    if rate_limited && retry_after > BACKOFF_CAP_S {
        asked += RETRY_AFTER_MARGIN_S;
    }
    let park_bound = if rate_limited {
        RETRY_AFTER_FLOOR_CAP_S
    } else {
        TRUST_MAX_AGE_S
    };
    asked.min(park_bound).max(computed)
}

/// The stalest fetchable candidate: never-measured first, then by `fetched_at`,
/// ties by slot. Sentinel, dead, backed-off and not-yet-due rows are skipped.
pub fn due_candidate(
    candidates: &[u32],
    entries: &BTreeMap<u32, UsageEntry>,
    now: f64,
) -> Option<u32> {
    let mut ranked: Vec<(u8, f64, u32)> = Vec::new();
    for &slot in candidates {
        match entries.get(&slot) {
            None => ranked.push((0, 0.0, slot)),
            Some(entry) => {
                if entry.sentinel.is_some()
                    || entry.token_dead(AUTH_DEAD_STRIKES, None)
                    || entry.in_backoff(now)
                {
                    continue;
                }
                if entry.next_poll_at.is_some_and(|at| now < at) && !entry.overslept(now) {
                    continue;
                }
                match entry.fetched_at {
                    None => ranked.push((0, 0.0, slot)),
                    Some(at) => ranked.push((1, at, slot)),
                }
            }
        }
    }
    ranked
        .into_iter()
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, _, slot)| slot)
}

fn eligible(entry: &UsageEntry, now: f64, mode: ReserveMode) -> bool {
    if entry.token_dead(AUTH_DEAD_STRIKES, None) || entry.in_backoff(now) || entry.claimed(now) {
        return false;
    }
    let stale = !entry.fresh(now, SERVE_TTL_S);
    let poll_due = entry.next_poll_at.is_some_and(|at| now >= at);
    let no_plan = entry.next_poll_at.is_none();
    match mode {
        ReserveMode::OnDemand => stale && (poll_due || no_plan || entry.overslept(now)),
        ReserveMode::Scheduled => poll_due || (stale && (no_plan || entry.overslept(now))),
        ReserveMode::Escalation => poll_due || stale,
    }
}

pub struct UsageStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl UsageStore {
    pub fn new(paths: &Paths) -> Self {
        Self {
            path: paths.usage_file(),
            lock_path: paths.usage_lock_file(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Rows for the given slots whose stored identity matches; others are invisible.
    pub fn entries(
        &self,
        identities: &BTreeMap<u32, Identity>,
        now: f64,
    ) -> BTreeMap<u32, UsageEntry> {
        let table = Table::load(&self.path);
        identities
            .iter()
            .filter_map(|(slot, identity)| {
                let row = table.row(*slot).filter(|row| row.matches(identity))?;
                Some((*slot, row.entry(now)))
            })
            .collect()
    }

    /// Claim the eligible candidates for a fetch (stamps `lastAttemptAt`) and
    /// return them in the order given. A row of another identity is replaced
    /// and claimed at once; a candidate without an identity is skipped.
    pub fn reserve(
        &self,
        candidates: &[u32],
        identities: &BTreeMap<u32, Identity>,
        now: f64,
        mode: ReserveMode,
    ) -> Result<Vec<u32>> {
        let _lock = self.lock()?;
        let mut table = Table::load(&self.path);
        let mut reserved = Vec::new();
        for &slot in candidates {
            let Some(identity) = identities.get(&slot) else {
                continue;
            };
            let gated = table
                .row(slot)
                .is_some_and(|row| row.matches(identity) && !eligible(&row.entry(now), now, mode));
            if gated {
                continue;
            }
            table.own_row(slot, identity).last_attempt_at = Some(now);
            reserved.push(slot);
        }
        if !reserved.is_empty() {
            table.save(&self.path)?;
        }
        Ok(reserved)
    }

    /// Merge a fetch outcome (§4.5). Failures never touch `lastGood`.
    pub fn record(
        &self,
        slot: u32,
        identity: &Identity,
        outcome: FetchRecord,
        now: f64,
    ) -> Result<()> {
        let _lock = self.lock()?;
        let mut table = Table::load(&self.path);
        let row = table.own_row(slot, identity);
        row.last_attempt_at = Some(now);
        match outcome {
            FetchRecord::Success { usage, plan } => {
                row.last_good = Some(usage);
                row.fetched_at = Some(now);
                if let Some((next_poll_at, interval_s)) = plan {
                    row.next_poll_at = Some(next_poll_at);
                    row.poll_interval_s = Some(interval_s);
                }
                row.consecutive_failures = 0;
                row.last_error = None;
                row.backoff_until = None;
                row.auth_dead_strikes = 0;
            }
            FetchRecord::Failure {
                error,
                retry_after,
                permanent_auth,
                struck_fp,
            } => {
                let rate_limited = error == RATE_LIMIT_ERROR;
                row.consecutive_failures += 1;
                row.last_error = Some(error);
                if rate_limited {
                    row.last_429_at = Some(now);
                }
                row.backoff_until = Some(
                    now + failure_backoff_s(row.consecutive_failures, retry_after, rate_limited),
                );
                if permanent_auth {
                    row.auth_dead_strikes += 1;
                    row.struck_fingerprint = struck_fp;
                }
            }
        }
        table.save(&self.path)
    }

    /// Overwrite the plan of a row that exists under `identity`; a missing or
    /// foreign row is left alone (never-measured accounts stay plan-less).
    pub fn set_poll_plan(
        &self,
        slot: u32,
        identity: &Identity,
        next_poll_at: f64,
        interval_s: f64,
    ) -> Result<()> {
        let _lock = self.lock()?;
        let mut table = Table::load(&self.path);
        let Some(row) = table.row_mut(slot).filter(|row| row.matches(identity)) else {
            return Ok(());
        };
        row.next_poll_at = Some(next_poll_at);
        row.poll_interval_s = Some(interval_s);
        table.save(&self.path)
    }

    /// Lift the dead-token strike (a new credential was stored).
    pub fn clear_dead_token(&self, slots: &[u32]) -> Result<()> {
        let _lock = self.lock()?;
        let mut table = Table::load(&self.path);
        let mut changed = false;
        for &slot in slots {
            if let Some(row) = table.row_mut(slot)
                && (row.auth_dead_strikes != 0 || row.struck_fingerprint.is_some())
            {
                row.auth_dead_strikes = 0;
                row.struck_fingerprint = None;
                changed = true;
            }
        }
        if changed {
            table.save(&self.path)?;
        }
        Ok(())
    }

    fn lock(&self) -> Result<FileLock> {
        FileLock::acquire(&self.lock_path, FileLock::DEFAULT_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{WindowUsage, format_iso};

    const NOW: f64 = 1_000_000.0;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: UsageStore,
        paths: Paths,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_values(
            Some(dir.path().join("store")),
            Some(dir.path().join("codex")),
            None,
            dir.path(),
        )
        .unwrap();
        Fixture {
            store: UsageStore::new(&paths),
            paths,
            _dir: dir,
        }
    }

    fn identity(n: u32) -> Identity {
        Identity::new(format!("u{n}@example.com"), format!("acct-{n}"))
    }

    fn identities(slots: &[u32]) -> BTreeMap<u32, Identity> {
        slots.iter().map(|&n| (n, identity(n))).collect()
    }

    fn usage(five_hour: f64) -> NormalizedUsage {
        NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: five_hour,
                resets_at: None,
            }),
            ..NormalizedUsage::default()
        }
    }

    fn success(pct: f64, plan: Option<(f64, f64)>) -> FetchRecord {
        FetchRecord::Success {
            usage: usage(pct),
            plan,
        }
    }

    fn failure(error: &str, retry_after: Option<f64>) -> FetchRecord {
        FetchRecord::Failure {
            error: error.to_string(),
            retry_after,
            permanent_auth: false,
            struck_fp: None,
        }
    }

    fn entry_with(f: impl FnOnce(&mut UsageEntry)) -> UsageEntry {
        let mut entry = Row::default().entry(NOW);
        f(&mut entry);
        entry
    }

    #[test]
    fn keychain_sentinel_wording() {
        assert_eq!(
            UsageSentinel::KeychainUnavailable.label(),
            "keychain unavailable — locked or in use; try again"
        );
        assert_eq!(
            UsageSentinel::KeychainUnavailable.usage_status(),
            "keychain_unavailable"
        );
    }

    #[test]
    fn sentinel_strings() {
        assert_eq!(UsageSentinel::NoCredentials.label(), "no credentials");
        assert_eq!(
            UsageSentinel::NoCredentials.usage_status(),
            "no_credentials"
        );
        assert_eq!(
            UsageSentinel::TokenExpired.label(),
            "token expired — refresh deferred this pass; retries automatically"
        );
        assert_eq!(UsageSentinel::TokenExpired.usage_status(), "token_expired");
        assert_eq!(UsageSentinel::ApiKey.label(), "API key (no quota)");
        assert_eq!(UsageSentinel::ApiKey.usage_status(), "api_key");
        assert_eq!(
            UsageSentinel::ReloginNeeded.label(),
            "re-login needed — refresh token dead; log in with Codex, then run: ccsw add"
        );
        assert_eq!(
            UsageSentinel::ReloginNeeded.usage_status(),
            "relogin_required"
        );
    }

    #[test]
    fn backoff_worked_examples() {
        assert_eq!(failure_backoff_s(1, None, false), 30.0);
        assert_eq!(failure_backoff_s(3, None, false), 120.0);
        assert_eq!(failure_backoff_s(50, None, false), 600.0);
        assert_eq!(failure_backoff_s(0, None, false), 30.0);
        assert_eq!(failure_backoff_s(1, Some(90.0), true), 90.0);
        assert_eq!(failure_backoff_s(5, Some(10.0), true), 480.0);
        assert_eq!(failure_backoff_s(1, Some(300.0), true), 300.0);
        assert_eq!(failure_backoff_s(1, Some(3600.0), true), 4500.0);
        assert_eq!(failure_backoff_s(1, Some(3600.0), false), 3600.0);
        assert_eq!(failure_backoff_s(1, Some(86_400.0), true), 4500.0);
        assert_eq!(
            failure_backoff_s(1, Some(0.0), false),
            30.0,
            "503 retry-now"
        );
        assert_eq!(failure_backoff_s(1, Some(0.0), true), 300.0, "429 edge");
        assert_eq!(failure_backoff_s(6, Some(0.0), true), 600.0);
    }

    #[test]
    fn missing_corrupt_and_foreign_schema_read_empty() {
        let f = fixture();
        let ids = identities(&[1]);
        assert!(f.store.entries(&ids, NOW).is_empty());
        std::fs::create_dir_all(f.paths.cache_dir()).unwrap();
        std::fs::write(f.paths.usage_file(), "{not json").unwrap();
        assert!(f.store.entries(&ids, NOW).is_empty());
        std::fs::write(
            f.paths.usage_file(),
            r#"{"schemaVersion": 1, "accounts": {"1": {"email": "u1@example.com", "accountId": "acct-1"}}}"#,
        )
        .unwrap();
        assert!(f.store.entries(&ids, NOW).is_empty());
        // A schema-1 file is replaced wholesale by the first write.
        f.store
            .record(1, &identity(1), success(10.0, None), NOW)
            .unwrap();
        let value = fsutil::read_json(&f.paths.usage_file()).unwrap().unwrap();
        assert_eq!(value["schemaVersion"], 2);
    }

    #[test]
    fn success_writes_the_spec_row_shape() {
        let f = fixture();
        f.store
            .record(
                2,
                &identity(2),
                success(42.0, Some((NOW + 180.0, 180.0))),
                NOW,
            )
            .unwrap();
        let value = fsutil::read_json(&f.paths.usage_file()).unwrap().unwrap();
        let row = &value["accounts"]["2"];
        assert_eq!(row["email"], "u2@example.com");
        assert_eq!(row["accountId"], "acct-2");
        assert_eq!(row["lastGood"]["five_hour"]["pct"], 42.0);
        assert_eq!(row["fetchedAt"], NOW);
        assert_eq!(row["lastAttemptAt"], NOW);
        assert_eq!(row["consecutiveFailures"], 0);
        assert!(row["lastError"].is_null());
        assert!(row["backoffUntil"].is_null());
        assert_eq!(row["nextPollAt"], NOW + 180.0);
        assert_eq!(row["pollIntervalS"], 180.0);
        assert!(row["last429At"].is_null());
        assert_eq!(row["authDeadStrikes"], 0);
        assert!(row["struckFingerprint"].is_null());

        let entries = f.store.entries(&identities(&[2]), NOW + 30.0);
        let entry = &entries[&2];
        assert_eq!(entry.age_s, Some(30.0));
        assert!(entry.fresh(NOW + 30.0, SERVE_TTL_S));
        assert_eq!(entry.decision_value(), Some(&usage(42.0)));
        assert_eq!(entry.sentinel, None);
    }

    #[test]
    fn failures_keep_last_good_and_back_off() {
        let f = fixture();
        let id = identity(1);
        let ids = identities(&[1]);
        f.store.record(1, &id, success(10.0, None), NOW).unwrap();
        f.store
            .record(1, &id, failure("timeout", None), NOW + 200.0)
            .unwrap();
        let entry = &f.store.entries(&ids, NOW + 200.0)[&1];
        assert_eq!(entry.last_good, Some(usage(10.0)));
        assert_eq!(entry.fetched_at, Some(NOW));
        assert_eq!(entry.consecutive_failures, 1);
        assert_eq!(entry.last_error.as_deref(), Some("timeout"));
        assert_eq!(entry.backoff_until, Some(NOW + 230.0));
        assert!(entry.in_backoff(NOW + 229.0));
        assert!(!entry.in_backoff(NOW + 230.0));
        assert_eq!(entry.last_429_at, None);

        f.store
            .record(1, &id, failure("network", None), NOW + 300.0)
            .unwrap();
        let entry = &f.store.entries(&ids, NOW + 300.0)[&1];
        assert_eq!(entry.consecutive_failures, 2);
        assert_eq!(entry.backoff_until, Some(NOW + 360.0));
        assert!(
            entry.trust_extended,
            "failures extend trust while the last-good is under an hour old"
        );
        assert_eq!(entry.decision_value(), Some(&usage(10.0)));

        f.store
            .record(1, &id, success(11.0, None), NOW + 400.0)
            .unwrap();
        let entry = &f.store.entries(&ids, NOW + 400.0)[&1];
        assert_eq!(entry.consecutive_failures, 0);
        assert_eq!(entry.last_error, None);
        assert_eq!(entry.backoff_until, None);
    }

    #[test]
    fn rate_limits_are_remembered_and_extend_trust_longer() {
        let f = fixture();
        let id = identity(1);
        let ids = identities(&[1]);
        let mut measured = usage(10.0);
        measured.five_hour.as_mut().unwrap().resets_at = Some(format_iso(1_005_000));
        f.store
            .record(
                1,
                &id,
                FetchRecord::Success {
                    usage: measured,
                    plan: None,
                },
                NOW,
            )
            .unwrap();
        f.store
            .record(1, &id, failure("http-429", Some(3600.0)), NOW + 100.0)
            .unwrap();
        let entry = &f.store.entries(&ids, NOW + 100.0)[&1];
        assert_eq!(entry.last_429_at, Some(NOW + 100.0));
        assert_eq!(entry.backoff_until, Some(NOW + 100.0 + 4500.0));
        assert!(entry.recent_429(NOW + 100.0 + 4500.0 + 3599.0));
        assert!(!entry.recent_429(NOW + 100.0 + 4500.0 + 3600.0));
        assert!(entry.trust_extended);

        // Trust ends at the soonest window reset, even inside the two-hour ceiling.
        let entry = &f.store.entries(&ids, 1_004_999.0)[&1];
        assert!(entry.trust_extended);
        let entry = &f.store.entries(&ids, 1_005_000.0)[&1];
        assert!(!entry.trust_extended);
        assert_eq!(entry.decision_value(), None);

        // Success never clears last429At.
        f.store
            .record(1, &id, success(12.0, None), NOW + 5000.0)
            .unwrap();
        let entry = &f.store.entries(&ids, NOW + 5000.0)[&1];
        assert_eq!(entry.last_429_at, Some(NOW + 100.0));
        // With the backoff gone, recency counts from the 429 itself again.
        assert!(entry.recent_429(NOW + 3699.0));
        assert!(!entry.recent_429(NOW + 3700.0));
    }

    #[test]
    fn non_429_trust_stops_at_an_hour() {
        let f = fixture();
        let id = identity(1);
        let ids = identities(&[1]);
        f.store.record(1, &id, success(10.0, None), NOW).unwrap();
        f.store
            .record(1, &id, failure("timeout", None), NOW + 10.0)
            .unwrap();
        assert!(f.store.entries(&ids, NOW + 3600.0)[&1].trust_extended);
        let entry = &f.store.entries(&ids, NOW + 3601.0)[&1];
        assert!(!entry.trust_extended);
        assert_eq!(entry.decision_value(), None);
        assert_eq!(entry.last_good, Some(usage(10.0)), "display still has it");
    }

    #[test]
    fn a_pending_plan_or_claim_extends_trust_without_failures() {
        let f = fixture();
        let id = identity(1);
        let ids = identities(&[1]);
        f.store
            .record(1, &id, success(10.0, Some((NOW + 600.0, 600.0))), NOW)
            .unwrap();
        let entry = &f.store.entries(&ids, NOW + 400.0)[&1];
        assert!(entry.trust_extended);
        assert_eq!(entry.decision_value(), Some(&usage(10.0)));
        let entry = &f.store.entries(&ids, NOW + 700.0)[&1];
        assert!(!entry.trust_extended);
        assert_eq!(entry.decision_value(), None);

        f.store
            .reserve(&[1], &ids, NOW + 700.0, ReserveMode::OnDemand)
            .unwrap();
        let entry = &f.store.entries(&ids, NOW + 750.0)[&1];
        assert!(entry.claimed(NOW + 750.0));
        assert!(entry.trust_extended);
    }

    #[test]
    fn decision_value_defers_to_a_sentinel() {
        let entry = entry_with(|e| {
            e.last_good = Some(usage(10.0));
            e.age_s = Some(10.0);
            e.sentinel = Some(UsageSentinel::TokenExpired);
        });
        assert_eq!(entry.decision_value(), None);
        let entry = entry_with(|e| {
            e.last_good = Some(usage(10.0));
            e.age_s = Some(300.0);
        });
        assert_eq!(entry.decision_value(), Some(&usage(10.0)));
        let entry = entry_with(|e| {
            e.last_good = Some(usage(10.0));
            e.age_s = Some(301.0);
        });
        assert_eq!(entry.decision_value(), None);
    }

    #[test]
    fn dead_token_strikes_bind_to_the_fingerprint() {
        let f = fixture();
        let id = identity(1);
        let ids = identities(&[1]);
        f.store
            .record(
                1,
                &id,
                FetchRecord::Failure {
                    error: "invalid_grant".into(),
                    retry_after: None,
                    permanent_auth: true,
                    struck_fp: Some("sha256:aaa".into()),
                },
                NOW,
            )
            .unwrap();
        let entry = &f.store.entries(&ids, NOW)[&1];
        assert_eq!(entry.auth_dead_strikes, 1);
        assert_eq!(entry.struck_fingerprint.as_deref(), Some("sha256:aaa"));
        assert!(entry.token_dead(AUTH_DEAD_STRIKES, None));
        assert!(entry.token_dead(AUTH_DEAD_STRIKES, Some("sha256:aaa")));
        assert!(
            !entry.token_dead(AUTH_DEAD_STRIKES, Some("sha256:bbb")),
            "a newer stored credential heals the strike"
        );
        assert!(!entry.token_dead(2, None));

        // Struck before fingerprints existed: binds unconditionally.
        let legacy = entry_with(|e| e.auth_dead_strikes = 1);
        assert!(legacy.token_dead(1, Some("sha256:bbb")));

        assert!(
            f.store
                .reserve(&[1], &ids, NOW + 10_000.0, ReserveMode::Escalation)
                .unwrap()
                .is_empty(),
            "dead rows are never reserved"
        );
        f.store.clear_dead_token(&[1, 99]).unwrap();
        let entry = &f.store.entries(&ids, NOW)[&1];
        assert_eq!(entry.auth_dead_strikes, 0);
        assert_eq!(entry.struck_fingerprint, None);

        // Success also lifts the strike.
        f.store
            .record(
                1,
                &id,
                FetchRecord::Failure {
                    error: "no_refresh_token".into(),
                    retry_after: None,
                    permanent_auth: true,
                    struck_fp: None,
                },
                NOW,
            )
            .unwrap();
        f.store
            .record(1, &id, success(1.0, None), NOW + 1.0)
            .unwrap();
        assert_eq!(f.store.entries(&ids, NOW + 1.0)[&1].auth_dead_strikes, 0);
    }

    #[test]
    fn identity_guard_hides_and_replaces_foreign_rows() {
        let f = fixture();
        let old = Identity::new("old@example.com", "acct-old");
        f.store.record(1, &old, success(50.0, None), NOW).unwrap();
        let ids = identities(&[1]);
        assert!(f.store.entries(&ids, NOW).is_empty());
        let mut old_ids = BTreeMap::new();
        old_ids.insert(1, old.clone());
        assert_eq!(f.store.entries(&old_ids, NOW).len(), 1);

        // Same email under another workspace is a different identity.
        let mut sibling = BTreeMap::new();
        sibling.insert(1, Identity::new("old@example.com", "acct-2"));
        assert!(f.store.entries(&sibling, NOW).is_empty());

        // A foreign row is reserved at once (replaced), and a record replaces it too.
        assert_eq!(
            f.store
                .reserve(&[1], &ids, NOW + 1.0, ReserveMode::OnDemand)
                .unwrap(),
            vec![1]
        );
        let entry = &f.store.entries(&ids, NOW + 1.0)[&1];
        assert_eq!(entry.last_good, None);
        assert_eq!(entry.last_attempt_at, Some(NOW + 1.0));
        assert!(f.store.entries(&old_ids, NOW + 1.0).is_empty());

        f.store
            .record(1, &old, failure("timeout", None), NOW + 2.0)
            .unwrap();
        let entry = &f.store.entries(&old_ids, NOW + 2.0)[&1];
        assert_eq!(entry.consecutive_failures, 1);
        assert_eq!(entry.last_good, None, "the replaced row starts fresh");
    }

    #[test]
    fn reserve_gates_by_mode() {
        let f = fixture();
        let ids = identities(&[1, 2, 3, 4]);
        // 1: never fetched; 2: fresh; 3: stale with a plan not yet due; 4: stale, plan due.
        f.store
            .record(
                2,
                &identity(2),
                success(1.0, Some((NOW + 500.0, 300.0))),
                NOW - 100.0,
            )
            .unwrap();
        f.store
            .record(
                3,
                &identity(3),
                success(1.0, Some((NOW + 500.0, 300.0))),
                NOW - 400.0,
            )
            .unwrap();
        f.store
            .record(
                4,
                &identity(4),
                success(1.0, Some((NOW - 10.0, 300.0))),
                NOW - 400.0,
            )
            .unwrap();
        let all = [1, 2, 3, 4];
        assert_eq!(
            f.store
                .reserve(&all, &ids, NOW, ReserveMode::OnDemand)
                .unwrap(),
            vec![1, 4]
        );
        // Those two are now claimed for 90 s.
        assert!(
            f.store
                .reserve(&[1, 4], &ids, NOW + 89.0, ReserveMode::Escalation)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            f.store
                .reserve(&[1, 4], &ids, NOW + 90.0, ReserveMode::Escalation)
                .unwrap(),
            vec![1, 4]
        );
        assert_eq!(
            f.store
                .reserve(&[2, 3], &ids, NOW, ReserveMode::Escalation)
                .unwrap(),
            vec![3],
            "escalation takes any stale row but never a fresh one"
        );

        let f = fixture();
        f.store
            .record(
                2,
                &identity(2),
                success(1.0, Some((NOW - 10.0, 300.0))),
                NOW - 100.0,
            )
            .unwrap();
        f.store
            .record(
                3,
                &identity(3),
                success(1.0, Some((NOW + 500.0, 300.0))),
                NOW - 400.0,
            )
            .unwrap();
        assert!(
            f.store
                .reserve(&[2, 3], &ids, NOW, ReserveMode::OnDemand)
                .unwrap()
                .is_empty(),
            "on-demand never fetches a fresh row, even when its plan is due"
        );
        assert_eq!(
            f.store
                .reserve(&[2, 3], &ids, NOW, ReserveMode::Scheduled)
                .unwrap(),
            vec![2],
            "scheduled follows the plan"
        );
        assert!(
            f.store
                .reserve(&[9], &ids, NOW, ReserveMode::Escalation)
                .unwrap()
                .is_empty(),
            "no identity, no row"
        );
    }

    #[test]
    fn reserve_repairs_an_overslept_plan() {
        let f = fixture();
        let ids = identities(&[1]);
        f.store
            .record(
                1,
                &identity(1),
                success(1.0, Some((NOW + 5000.0, 600.0))),
                NOW - 400.0,
            )
            .unwrap();
        let entry = &f.store.entries(&ids, NOW)[&1];
        assert!(entry.overslept(NOW));
        assert_eq!(
            f.store
                .reserve(&[1], &ids, NOW, ReserveMode::OnDemand)
                .unwrap(),
            vec![1]
        );
        let f = fixture();
        f.store
            .record(
                1,
                &identity(1),
                success(1.0, Some((NOW + 700.0, 600.0))),
                NOW - 400.0,
            )
            .unwrap();
        assert!(!f.store.entries(&ids, NOW)[&1].overslept(NOW));
        assert!(
            f.store
                .reserve(&[1], &ids, NOW, ReserveMode::Scheduled)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn backoff_blocks_reservation() {
        let f = fixture();
        let ids = identities(&[1]);
        f.store
            .record(1, &identity(1), failure("timeout", None), NOW - 100.0)
            .unwrap();
        assert!(
            f.store
                .reserve(&[1], &ids, NOW - 80.0, ReserveMode::Escalation)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            f.store
                .reserve(&[1], &ids, NOW, ReserveMode::Escalation)
                .unwrap(),
            vec![1]
        );
    }

    #[test]
    fn set_poll_plan_only_touches_owned_rows() {
        let f = fixture();
        let ids = identities(&[1]);
        f.store
            .set_poll_plan(1, &identity(1), NOW + 10.0, 180.0)
            .unwrap();
        assert!(f.store.entries(&ids, NOW).is_empty());
        f.store
            .record(
                1,
                &identity(1),
                success(1.0, Some((NOW + 500.0, 300.0))),
                NOW,
            )
            .unwrap();
        f.store
            .set_poll_plan(1, &identity(2), NOW + 10.0, 180.0)
            .unwrap();
        assert_eq!(
            f.store.entries(&ids, NOW)[&1].next_poll_at,
            Some(NOW + 500.0)
        );
        f.store
            .set_poll_plan(1, &identity(1), NOW + 10.0, 180.0)
            .unwrap();
        let entry = &f.store.entries(&ids, NOW)[&1];
        assert_eq!(entry.next_poll_at, Some(NOW + 10.0));
        assert_eq!(entry.poll_interval_s, Some(180.0));
    }

    #[test]
    fn due_candidate_prefers_the_stalest() {
        let mut entries = BTreeMap::new();
        entries.insert(
            2,
            entry_with(|e| {
                e.fetched_at = Some(NOW - 500.0);
                e.next_poll_at = Some(NOW - 1.0);
            }),
        );
        entries.insert(
            3,
            entry_with(|e| {
                e.fetched_at = Some(NOW - 900.0);
                e.next_poll_at = Some(NOW - 1.0);
            }),
        );
        entries.insert(4, entry_with(|e| e.sentinel = Some(UsageSentinel::ApiKey)));
        entries.insert(5, entry_with(|e| e.auth_dead_strikes = 1));
        entries.insert(6, entry_with(|e| e.backoff_until = Some(NOW + 1.0)));
        entries.insert(
            7,
            entry_with(|e| {
                e.fetched_at = Some(NOW - 9000.0);
                e.next_poll_at = Some(NOW + 1.0);
            }),
        );
        assert_eq!(due_candidate(&[2, 3], &entries, NOW), Some(3));
        assert_eq!(
            due_candidate(&[2, 3, 8], &entries, NOW),
            Some(8),
            "never seen wins"
        );
        assert_eq!(
            due_candidate(&[3, 9, 8], &entries, NOW),
            Some(8),
            "ties by slot"
        );
        assert_eq!(due_candidate(&[4, 5, 6, 7], &entries, NOW), None);
        entries.get_mut(&7).unwrap().next_poll_at = Some(NOW + 5000.0);
        assert_eq!(
            due_candidate(&[7], &entries, NOW),
            Some(7),
            "an overslept plan does not hold the row"
        );
        assert_eq!(due_candidate(&[], &entries, NOW), None);
    }
}
