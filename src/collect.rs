//! The on-demand usage pass shared by `list`, `status`, `switch --strategy`,
//! the TUI and the auto-switch engine (spec §8.3–8.4).
//!
//! One pass decides which rows are due (the usage store's reservation gate),
//! fetches at most the active account plus one due candidate (every candidate
//! under escalation), persists any token rotation the fetch produced with
//! compare-and-swap on the presented refresh token, records the outcomes, and
//! hands back the rows with their sentinels derived.
//!
//! The active account is always read from the live `auth.json` (Codex owns
//! it) and is never refreshed ahead of a fetch; inactive accounts use their
//! slot snapshot, which is refreshed when a JWT is about to expire.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::codex::auth::{AuthJson, AuthKind};
use crate::codex::jwt::{is_expiring, token_expires_at};
use crate::codex::oauth::{self, Presented, RefreshError, RefreshedTokens};
use crate::codex::usage::{
    FetchError, FetchOutcome, REFRESH_MARGIN_SECS, build_client, fetch_usage,
};
use crate::errors::Result;
use crate::model::{CurrentAccount, Identity, Roster, now_unix};
use crate::store::Store;
use crate::store::credentials;
use crate::store::poll_policy::{CANDIDATE_MAX_INTERVAL_S, plan_after_fetch};
use crate::store::usage_store::{
    AUTH_DEAD_STRIKES, FetchRecord, ReserveMode, STALE_OK_S, UsageEntry, UsageSentinel, UsageStore,
    due_candidate,
};
use crate::usage_math::{at_limit, binding_pct, headroom};

/// Delay between the starts of two usage requests in one pass.
pub const STAGGER: Duration = Duration::from_millis(250);

/// Which rows a pass may fetch (spec §8.4, research notes §4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectMode {
    /// `list`, `status`, `switch --strategy`, dashboards: stale and plan-due rows only.
    OnDemand,
    /// The engine's regular tick: plan-due rows, or stale rows without a usable plan.
    Scheduled,
    /// The engine near the threshold: every stale or due candidate.
    Escalation,
    /// Read the store; never fetch.
    StoreOnly,
}

#[derive(Debug, Clone)]
pub struct CollectOptions<'a> {
    pub mode: CollectMode,
    /// The slot whose credentials are the live login, if any.
    pub active: Option<u32>,
    /// The other slots to report (and, when due, fetch).
    pub candidates: &'a [u32],
    /// Poll-plan inputs (the auto-switch threshold and model pools).
    pub threshold: f64,
    pub models: &'a [String],
}

#[derive(Debug, Clone, Default)]
pub struct Collected {
    /// One row per requested slot (active and candidates), sentinels applied.
    pub entries: BTreeMap<u32, UsageEntry>,
    /// `[Account-N] token refresh succeeded but the rotated credentials could not be saved: …`
    pub token_persist_failures: Vec<String>,
}

/// Outcome of [`refresh_slot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshStatus {
    /// Tokens were rotated and persisted.
    Ok,
    /// The tokens are not close to expiry (or the account is an API key).
    NotNeeded,
    /// A refresh is needed but the credential carries no refresh token.
    NoRefreshToken,
    /// Transport trouble, a 5xx, or the rotation could not be saved.
    Transient(String),
    /// The auth server rejected the refresh token for good.
    Terminal { code: String, memorable: bool },
}

/// The slot whose credentials are in the live `auth.json`: identity match for a
/// ChatGPT login, key match for an API key. Never trusts `activeAccountNumber`.
pub fn live_login(store: &Store, roster: &Roster) -> CurrentAccount {
    let Ok(Some(auth)) = AuthJson::read(&store.paths.live_auth_file()) else {
        return CurrentAccount::NoLogin;
    };
    match auth.kind() {
        AuthKind::ChatGpt => match auth.identity() {
            Some(identity) => match roster.find_slot(&identity) {
                Some(slot) => CurrentAccount::Managed {
                    slot,
                    email: roster
                        .record(slot)
                        .map(|r| r.email.clone())
                        .unwrap_or(identity.email),
                    api_key: false,
                },
                None => CurrentAccount::Unmanaged {
                    email: identity.email,
                },
            },
            None => CurrentAccount::Unmanaged {
                email: auth.account_info().email.unwrap_or_default(),
            },
        },
        AuthKind::ApiKey => {
            let key = auth.api_key();
            let matched = roster.sequence.iter().copied().find(|slot| {
                credentials::read(store, *slot)
                    .ok()
                    .flatten()
                    .is_some_and(|stored| AuthJson::from_value(stored).api_key() == key)
            });
            match matched {
                Some(slot) => CurrentAccount::Managed {
                    slot,
                    email: roster
                        .record(slot)
                        .map(|r| r.email.clone())
                        .unwrap_or_default(),
                    api_key: true,
                },
                None => CurrentAccount::Unmanaged {
                    email: String::new(),
                },
            }
        }
        AuthKind::Unknown => CurrentAccount::NoLogin,
    }
}

/// What a pass knows about one slot before any network call.
struct SlotView {
    identity: Option<Identity>,
    api_key_record: bool,
    /// The stored snapshot; `None` when the slot has no readable credentials.
    stored: Option<AuthJson>,
    /// The live `auth.json`, for the active slot only.
    live: Option<AuthJson>,
}

impl SlotView {
    fn load(store: &Store, roster: &Roster, slot: u32, is_active: bool) -> Self {
        let record = roster.record(slot);
        let stored = credentials::read(store, slot)
            .ok()
            .flatten()
            .map(AuthJson::from_value);
        let live = if is_active {
            AuthJson::read(&store.paths.live_auth_file()).ok().flatten()
        } else {
            None
        };
        Self {
            identity: record.map(|r| r.identity()),
            api_key_record: record.is_some_and(|r| r.is_api_key()),
            stored,
            live,
        }
    }

    /// The credential a fetch would present: the live file for the active slot.
    fn presented(&self) -> Option<&AuthJson> {
        self.live.as_ref().or(self.stored.as_ref())
    }

    fn fetchable(&self) -> bool {
        !self.api_key_record
            && self.identity.is_some()
            && self
                .presented()
                .is_some_and(|auth| auth.kind() == AuthKind::ChatGpt)
    }

    fn fingerprint(&self) -> Option<String> {
        self.presented()
            .and_then(|auth| credentials::fingerprint(&auth.0))
    }

    /// The access token is past its `exp` claim (or absent).
    fn access_expired(&self, now: f64) -> bool {
        match self.presented().and_then(AuthJson::access_token) {
            None => true,
            Some(token) => token_expires_at(token).is_some_and(|exp| exp as f64 <= now),
        }
    }
}

fn blank_entry() -> UsageEntry {
    UsageEntry {
        sentinel: None,
        last_good: None,
        fetched_at: None,
        age_s: None,
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

/// Sentinel for a slot as of `now`; `TokenExpired` applies to the active slot
/// only once the pass is over and it did not get a fresh measurement.
fn derive_sentinel(
    view: &SlotView,
    entry: &UsageEntry,
    is_active: bool,
    now: f64,
    pass_started: Option<f64>,
) -> Option<UsageSentinel> {
    let Some(auth) = view.presented() else {
        return Some(UsageSentinel::NoCredentials);
    };
    if view.api_key_record || auth.kind() == AuthKind::ApiKey {
        return Some(UsageSentinel::ApiKey);
    }
    if auth.kind() != AuthKind::ChatGpt {
        return Some(UsageSentinel::NoCredentials);
    }
    if entry.token_dead(AUTH_DEAD_STRIKES, view.fingerprint().as_deref()) {
        return Some(UsageSentinel::ReloginNeeded);
    }
    if let Some(started) = pass_started
        && is_active
        && view.access_expired(now)
        && !entry.fetched_at.is_some_and(|at| at >= started)
    {
        return Some(UsageSentinel::TokenExpired);
    }
    None
}

/// A candidate-style plan left on the active row by a role change: the row is
/// past the decision horizon and its interval is wider than an active plan.
fn stale_candidate_plan(entry: &UsageEntry, models: &[String]) -> bool {
    entry.age_s.is_some_and(|age| age >= STALE_OK_S)
        && entry.poll_interval_s.is_some_and(|i| i > 300.0)
        && entry
            .last_good
            .as_ref()
            .and_then(|usage| binding_pct(usage, models))
            .is_some_and(|pct| pct < 100.0)
}

/// An exhausted candidate parked on a wide (post-429) plan keeps that plan
/// through an escalation.
fn exhausted_with_wide_plan(entry: &UsageEntry, models: &[String], now: f64) -> bool {
    entry
        .decision_value()
        .is_some_and(|usage| at_limit(usage, models))
        && entry.next_poll_at.is_some_and(|at| at > now)
        && entry
            .poll_interval_s
            .is_some_and(|i| i > CANDIDATE_MAX_INTERVAL_S)
}

/// Run one pass (see the module docs).
pub fn run_pass(store: &Store, roster: &Roster, opts: CollectOptions<'_>) -> Result<Collected> {
    let started = now_unix() as f64;
    let usage_store = UsageStore::new(&store.paths);

    let mut slots: Vec<u32> = opts.active.into_iter().collect();
    for &slot in opts.candidates {
        if !slots.contains(&slot) {
            slots.push(slot);
        }
    }
    let views: BTreeMap<u32, SlotView> = slots
        .iter()
        .map(|&slot| {
            (
                slot,
                SlotView::load(store, roster, slot, opts.active == Some(slot)),
            )
        })
        .collect();
    let identities: BTreeMap<u32, Identity> = views
        .iter()
        .filter_map(|(slot, view)| Some((*slot, view.identity.clone()?)))
        .collect();

    let mut entries = assemble(&usage_store, &views, &identities, &opts, started, None);
    heal_strikes(&usage_store, &views, &entries)?;
    if opts.mode == CollectMode::StoreOnly {
        return Ok(Collected {
            entries: assemble(
                &usage_store,
                &views,
                &identities,
                &opts,
                started,
                Some(started),
            ),
            token_persist_failures: Vec::new(),
        });
    }

    let reserved = reserve(&usage_store, &views, &entries, &identities, &opts, started)?;
    let mut failures = Vec::new();
    if !reserved.is_empty() {
        let jobs: Vec<(u32, AuthJson)> = reserved
            .iter()
            .filter_map(|slot| Some((*slot, views[slot].presented()?.clone())))
            .collect();
        for (slot, presented, outcome) in fetch_all(jobs)? {
            let is_active = opts.active == Some(slot);
            let now = now_unix() as f64;
            if let Some(tokens) = &outcome.refreshed
                && let Some(rt) = presented.refresh_token()
                && let Err(err) = persist_rotation(store, slot, is_active, rt, tokens)
            {
                failures.push(format!(
                    "[Account-{slot}] token refresh succeeded but the rotated credentials could not be saved: {err}"
                ));
            }
            let identity = &identities[&slot];
            let previous = entries.get(&slot).cloned().unwrap_or_else(blank_entry);
            let record = match outcome.result {
                Ok(usage) => {
                    let plan = plan_after_fetch(
                        previous.poll_interval_s,
                        previous.last_good.as_ref(),
                        &usage,
                        is_active,
                        opts.threshold,
                        opts.models,
                        previous.recent_429(now),
                        now,
                        rand::random::<f64>,
                    );
                    FetchRecord::Success {
                        usage,
                        plan: Some(plan),
                    }
                }
                Err(err) => {
                    let retry_after = match &err {
                        FetchError::Http { retry_after, .. } => *retry_after,
                        _ => None,
                    };
                    let permanent_auth = err.is_terminal_auth();
                    tracing::warn!("Usage fetch failed for account {slot}: {}", err.label());
                    FetchRecord::Failure {
                        error: err.label(),
                        retry_after,
                        permanent_auth,
                        struck_fp: permanent_auth
                            .then(|| credentials::fingerprint(&presented.0))
                            .flatten(),
                    }
                }
            };
            usage_store.record(slot, identity, record, now)?;
        }
        // Re-read the views: a rotation may have changed the presented credential.
        let views: BTreeMap<u32, SlotView> = slots
            .iter()
            .map(|&slot| {
                (
                    slot,
                    SlotView::load(store, roster, slot, opts.active == Some(slot)),
                )
            })
            .collect();
        entries = assemble(
            &usage_store,
            &views,
            &identities,
            &opts,
            now_unix() as f64,
            Some(started),
        );
    } else {
        entries = assemble(
            &usage_store,
            &views,
            &identities,
            &opts,
            started,
            Some(started),
        );
    }
    Ok(Collected {
        entries,
        token_persist_failures: failures,
    })
}

/// Rows for every viewed slot (blank when the store has none) with sentinels.
fn assemble(
    usage_store: &UsageStore,
    views: &BTreeMap<u32, SlotView>,
    identities: &BTreeMap<u32, Identity>,
    opts: &CollectOptions<'_>,
    now: f64,
    pass_started: Option<f64>,
) -> BTreeMap<u32, UsageEntry> {
    let stored = usage_store.entries(identities, now);
    views
        .iter()
        .map(|(slot, view)| {
            let mut entry = stored.get(slot).cloned().unwrap_or_else(blank_entry);
            entry.sentinel =
                derive_sentinel(view, &entry, opts.active == Some(*slot), now, pass_started);
            (*slot, entry)
        })
        .collect()
}

/// Lift dead-token strikes whose fingerprint no longer matches the stored
/// credential: the reservation gate does not look at fingerprints itself.
fn heal_strikes(
    usage_store: &UsageStore,
    views: &BTreeMap<u32, SlotView>,
    entries: &BTreeMap<u32, UsageEntry>,
) -> Result<()> {
    let healed: Vec<u32> = entries
        .iter()
        .filter(|(slot, entry)| {
            entry.auth_dead_strikes >= AUTH_DEAD_STRIKES
                && !entry.token_dead(AUTH_DEAD_STRIKES, views[slot].fingerprint().as_deref())
        })
        .map(|(slot, _)| *slot)
        .collect();
    if healed.is_empty() {
        return Ok(());
    }
    usage_store.clear_dead_token(&healed)
}

/// Claim the rows this pass fetches: the active slot when due, then one due
/// candidate (every due candidate under escalation).
fn reserve(
    usage_store: &UsageStore,
    views: &BTreeMap<u32, SlotView>,
    entries: &BTreeMap<u32, UsageEntry>,
    identities: &BTreeMap<u32, Identity>,
    opts: &CollectOptions<'_>,
    now: f64,
) -> Result<Vec<u32>> {
    let fetchable = |slot: u32| views[&slot].fetchable() && entries[&slot].sentinel.is_none();
    let store_mode = match opts.mode {
        CollectMode::OnDemand => ReserveMode::OnDemand,
        CollectMode::Scheduled => ReserveMode::Scheduled,
        CollectMode::Escalation | CollectMode::StoreOnly => ReserveMode::Escalation,
    };
    let mut reserved = Vec::new();
    if let Some(active) = opts.active
        && fetchable(active)
    {
        let mode = if opts.mode == CollectMode::Scheduled
            && stale_candidate_plan(&entries[&active], opts.models)
        {
            ReserveMode::Escalation
        } else {
            store_mode
        };
        reserved.extend(usage_store.reserve(&[active], identities, now, mode)?);
    }
    let candidates: Vec<u32> = opts
        .candidates
        .iter()
        .copied()
        .filter(|slot| Some(*slot) != opts.active && fetchable(*slot))
        .collect();
    match opts.mode {
        CollectMode::Escalation => {
            let wanted: Vec<u32> = candidates
                .into_iter()
                .filter(|slot| !exhausted_with_wide_plan(&entries[slot], opts.models, now))
                .collect();
            reserved.extend(usage_store.reserve(&wanted, identities, now, store_mode)?);
        }
        _ => {
            let mut remaining = candidates;
            while let Some(pick) = due_candidate(&remaining, entries, now) {
                if !usage_store
                    .reserve(&[pick], identities, now, store_mode)?
                    .is_empty()
                {
                    reserved.push(pick);
                    break;
                }
                remaining.retain(|slot| *slot != pick);
            }
        }
    }
    Ok(reserved)
}

type Fetched = (u32, AuthJson, FetchOutcome);

/// Fetch every job on a private runtime, starts staggered by [`STAGGER`].
/// Called from inside another runtime, the work moves to a helper thread so
/// the blocking wait never sits on an executor thread.
fn fetch_all(jobs: Vec<(u32, AuthJson)>) -> Result<Vec<Fetched>> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::scope(|scope| {
            scope
                .spawn(move || fetch_all_blocking(jobs))
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
    }
    fetch_all_blocking(jobs)
}

fn fetch_all_blocking(jobs: Vec<(u32, AuthJson)>) -> Result<Vec<Fetched>> {
    let client = build_client(None)?;
    let runtime = runtime()?;
    Ok(runtime.block_on(async move {
        let mut set = tokio::task::JoinSet::new();
        for (index, (slot, auth)) in jobs.into_iter().enumerate() {
            let client = client.clone();
            set.spawn(async move {
                tokio::time::sleep(STAGGER * index as u32).await;
                let outcome = fetch_usage(&client, &auth).await;
                (slot, auth, outcome)
            });
        }
        let mut results = Vec::new();
        // Every started fetch is awaited: dropping one mid-refresh would lose a rotation.
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(result) => results.push(result),
                Err(err) => tracing::warn!("usage fetch task failed: {err}"),
            }
        }
        results
    }))
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            crate::errors::CswitchError::config(format!("could not start async runtime: {err}"))
        })
}

/// Persist a rotation under the store lock, only where the presented refresh
/// token is still in place: into the slot file and, for the active slot, into
/// the live file. A live copy the slot missed (Codex rotated it earlier) is
/// folded into the slot afterwards when it is newer.
fn persist_rotation(
    store: &Store,
    slot: u32,
    is_active: bool,
    presented_refresh_token: &str,
    tokens: &RefreshedTokens,
) -> Result<()> {
    let _lock = store.lock()?;
    let mut updated_live = None;
    if is_active {
        let live_path = store.paths.live_auth_file();
        if let Some(mut live) = AuthJson::read(&live_path)?
            && live.refresh_token() == Some(presented_refresh_token)
        {
            live.apply_tokens(
                &tokens.id_token,
                &tokens.access_token,
                &tokens.refresh_token,
            );
            live.write(&live_path)?;
            updated_live = Some(live);
        }
    }
    let Some(stored) = credentials::read(store, slot)? else {
        return Ok(());
    };
    let mut stored = AuthJson::from_value(stored);
    if stored.refresh_token() == Some(presented_refresh_token) {
        stored.apply_tokens(
            &tokens.id_token,
            &tokens.access_token,
            &tokens.refresh_token,
        );
        credentials::write(store, slot, &stored.0)?;
    } else if let Some(live) = updated_live
        && live.identity() == stored.identity()
        && live.is_newer_than(&stored)
    {
        credentials::write(store, slot, &live.0)?;
    }
    Ok(())
}

/// Refresh the slot's tokens when the access or id JWT expires within
/// [`REFRESH_MARGIN_SECS`] (always with `force`), persist the rotation with
/// compare-and-swap, and report what happened. A terminal verdict is recorded
/// as a dead-token strike bound to the credential's fingerprint.
pub fn refresh_slot(store: &Store, roster: &Roster, slot: u32, force: bool) -> RefreshStatus {
    let Some(record) = roster.record(slot) else {
        return RefreshStatus::Transient(format!("Account-{slot} does not exist"));
    };
    if record.is_api_key() {
        return RefreshStatus::NotNeeded;
    }
    let stored = match credentials::read(store, slot) {
        Ok(Some(value)) => AuthJson::from_value(value),
        Ok(None) => {
            return RefreshStatus::Transient(format!("Account-{slot} has no stored credentials"));
        }
        Err(err) => return RefreshStatus::Transient(err.to_string()),
    };
    match stored.kind() {
        AuthKind::ApiKey => return RefreshStatus::NotNeeded,
        AuthKind::Unknown => return RefreshStatus::NoRefreshToken,
        AuthKind::ChatGpt => {}
    }
    // A live copy Codex rotated since the snapshot holds the only usable
    // refresh token; fold it in before presenting anything.
    let is_active = live_login(store, roster).slot() == Some(slot);
    let stored = if is_active {
        fold_live_into_slot(store, slot, &stored).unwrap_or(stored)
    } else {
        stored
    };

    let expiring = |token: &str| is_expiring(token, REFRESH_MARGIN_SECS).unwrap_or(false);
    let needed = force
        || match stored.access_token() {
            None => true,
            Some(access) => expiring(access) || stored.id_token().is_some_and(expiring),
        };
    if !needed {
        return RefreshStatus::NotNeeded;
    }
    let Some(refresh_token) = stored.refresh_token() else {
        return RefreshStatus::NoRefreshToken;
    };

    let result = run_refresh(
        refresh_token,
        stored.id_token().map(str::to_string),
        stored.access_token().map(str::to_string),
    );
    match result {
        Ok(tokens) => {
            if let Err(err) = persist_rotation(store, slot, is_active, refresh_token, &tokens) {
                return RefreshStatus::Transient(format!(
                    "token refresh succeeded but the rotated credentials could not be saved: {err}"
                ));
            }
            let _ = UsageStore::new(&store.paths).clear_dead_token(&[slot]);
            RefreshStatus::Ok
        }
        Err(RefreshError::Terminal {
            code, memorable, ..
        }) => {
            let strike = FetchRecord::Failure {
                error: "auth".to_string(),
                retry_after: None,
                permanent_auth: true,
                struck_fp: credentials::fingerprint(&stored.0),
            };
            if let Err(err) = UsageStore::new(&store.paths).record(
                slot,
                &record.identity(),
                strike,
                now_unix() as f64,
            ) {
                tracing::warn!("could not record the dead-token strike for account {slot}: {err}");
            }
            RefreshStatus::Terminal { code, memorable }
        }
        Err(RefreshError::Transient(detail)) => RefreshStatus::Transient(detail),
    }
}

/// When the live file belongs to `slot` and is newer than its snapshot, store it
/// and return the stored copy.
fn fold_live_into_slot(store: &Store, slot: u32, stored: &AuthJson) -> Option<AuthJson> {
    let live = AuthJson::read(&store.paths.live_auth_file())
        .ok()
        .flatten()?;
    if live.identity() != stored.identity() || !live.is_newer_than(stored) {
        return None;
    }
    let _lock = store.lock().ok()?;
    credentials::write(store, slot, &live.0).ok()?;
    Some(live)
}

fn run_refresh(
    refresh_token: &str,
    id_token: Option<String>,
    access_token: Option<String>,
) -> std::result::Result<RefreshedTokens, RefreshError> {
    let refresh_token = refresh_token.to_string();
    let work = move || -> std::result::Result<RefreshedTokens, RefreshError> {
        let client = build_client(None).map_err(|err| RefreshError::Transient(err.to_string()))?;
        let runtime = runtime().map_err(|err| RefreshError::Transient(err.to_string()))?;
        runtime.block_on(oauth::refresh_with(
            &client,
            &refresh_token,
            Presented {
                id_token: id_token.as_deref(),
                access_token: access_token.as_deref(),
            },
        ))
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::scope(|scope| {
            scope
                .spawn(work)
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
    }
    work()
}

/// `100 - binding pct` of a row's decision value; `None` when unknown.
pub fn entry_headroom(entry: &UsageEntry, models: &[String]) -> Option<f64> {
    entry
        .decision_value()
        .and_then(|usage| headroom(usage, models))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::jwt::make_jwt;
    use crate::model::{AccountKind, AccountRecord, NormalizedUsage, WindowUsage};
    use crate::store::temp_store;
    use serde_json::{Value, json};
    use std::net::SocketAddr;
    use std::sync::{Mutex, OnceLock};

    const FAR: i64 = 4_102_444_800;

    // ---- a mock of the usage and token endpoints, shared by every test ----

    #[derive(Debug, Clone)]
    struct Recorded {
        path: String,
        bearer: Option<String>,
        account_id: Option<String>,
    }

    #[derive(Default)]
    struct Mock {
        requests: Mutex<Vec<Recorded>>,
    }

    impl Mock {
        fn for_account(&self, account_id: &str) -> Vec<Recorded> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.account_id.as_deref() == Some(account_id))
                .cloned()
                .collect()
        }
    }

    fn usage_body() -> Value {
        json!({
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": {"used_percent": 42.0, "limit_window_seconds": 18000, "reset_at": FAR},
                "secondary_window": {"used_percent": 84.0, "limit_window_seconds": 604800, "reset_at": FAR + 100}
            }
        })
    }

    async fn usage_route(
        axum::extract::State(mock): axum::extract::State<std::sync::Arc<Mock>>,
        headers: axum::http::HeaderMap,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let bearer =
            header("authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_string));
        mock.requests.lock().unwrap().push(Recorded {
            path: "/usage".into(),
            bearer: bearer.clone(),
            account_id: header("chatgpt-account-id"),
        });
        match bearer.as_deref().unwrap_or("") {
            "at-good" => axum::Json(usage_body()).into_response(),
            "at-limited" => (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                [("Retry-After", "7")],
                "slow down",
            )
                .into_response(),
            _ => (
                axum::http::StatusCode::UNAUTHORIZED,
                axum::Json(json!({"detail": "Unauthorized"})),
            )
                .into_response(),
        }
    }

    async fn token_route(
        axum::extract::State(mock): axum::extract::State<std::sync::Arc<Mock>>,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let presented = body["refresh_token"].as_str().unwrap_or("").to_string();
        // A token request carries no account header, so tests keyed on the
        // account id read the usage trail instead.
        mock.requests.lock().unwrap().push(Recorded {
            path: "/token".into(),
            bearer: None,
            account_id: None,
        });
        match presented.as_str() {
            rt if rt.starts_with("rt-live-") => {
                let account_id = &rt["rt-live-".len()..];
                axum::Json(json!({
                    "id_token": id_token(&format!("{account_id}@example.com"), account_id, FAR),
                    "access_token": "at-good",
                    "refresh_token": format!("rt-next-{account_id}"),
                }))
                .into_response()
            }
            "rt-dead" => (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": {"code": "refresh_token_reused", "message": "used"}})),
            )
                .into_response(),
            _ => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
        }
    }

    fn mock() -> &'static std::sync::Arc<Mock> {
        static MOCK: OnceLock<std::sync::Arc<Mock>> = OnceLock::new();
        MOCK.get_or_init(|| {
            let mock = std::sync::Arc::new(Mock::default());
            let (tx, rx) = std::sync::mpsc::channel::<SocketAddr>();
            let state = mock.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let app = axum::Router::new()
                        .route("/usage", axum::routing::get(usage_route))
                        .route("/token", axum::routing::post(token_route))
                        .with_state(state);
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    tx.send(listener.local_addr().unwrap()).unwrap();
                    axum::serve(listener, app).await.unwrap();
                });
            });
            let addr = rx.recv().unwrap();
            // SAFETY: set once, before any test in this module reads the
            // overrides; the process-wide OnceLock serializes the initialization.
            unsafe {
                std::env::set_var("CSWITCH_USAGE_URL", format!("http://{addr}/usage"));
                std::env::set_var("CSWITCH_TOKEN_URL", format!("http://{addr}/token"));
            }
            mock
        })
    }

    // ---- fixtures ----

    fn id_token(email: &str, account_id: &str, exp: i64) -> String {
        make_jwt(&json!({
            "email": email,
            "exp": exp,
            "https://api.openai.com/auth": {"chatgpt_account_id": account_id, "chatgpt_plan_type": "pro"}
        }))
    }

    fn access_token(exp: i64) -> String {
        make_jwt(&json!({"exp": exp}))
    }

    fn chatgpt(account_id: &str, access: &str, refresh: Option<&str>, access_exp: i64) -> AuthJson {
        // The mock routes on a literal bearer; an empty one becomes a JWT
        // carrying `access_exp`, which no route answers with usage.
        let access = if access.is_empty() {
            access_token(access_exp)
        } else {
            access.to_string()
        };
        let mut tokens = json!({
            "id_token": id_token(&format!("{account_id}@example.com"), account_id, FAR),
            "access_token": access,
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

    fn record(account_id: &str) -> AccountRecord {
        let mut record = AccountRecord::new(format!("{account_id}@example.com"));
        record.organization_uuid = account_id.into();
        record
    }

    fn roster_with(slots: &[(u32, &str)]) -> Roster {
        let mut roster = Roster::empty();
        for (slot, account_id) in slots {
            roster.add_record(*slot, record(account_id));
        }
        roster
    }

    fn store_auth(store: &Store, slot: u32, auth: &AuthJson) {
        credentials::write(store, slot, &auth.0).unwrap();
    }

    fn write_live(store: &Store, auth: &AuthJson) {
        auth.write(&store.paths.live_auth_file()).unwrap();
    }

    fn opts<'a>(
        mode: CollectMode,
        active: Option<u32>,
        candidates: &'a [u32],
    ) -> CollectOptions<'a> {
        CollectOptions {
            mode,
            active,
            candidates,
            threshold: 90.0,
            models: &[],
        }
    }

    fn slot_auth(store: &Store, slot: u32) -> AuthJson {
        AuthJson::from_value(credentials::read(store, slot).unwrap().unwrap())
    }

    fn unique(tag: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{tag}-{}-{n}", std::process::id())
    }

    // ---- store-only behaviour (no network) ----

    #[test]
    fn sentinels_are_derived_per_slot() {
        let (_dir, store) = temp_store();
        let mut roster = roster_with(&[(1, "a1"), (2, "a2"), (3, "a3"), (4, "a4"), (5, "a5")]);
        roster.record_mut(2).unwrap().kind = Some(AccountKind::ApiKey);
        store_auth(&store, 1, &chatgpt("a1", "at-good", Some("rt-1"), FAR));
        store_auth(&store, 2, &AuthJson::api_key_auth("sk-2"));
        // 3 has no credentials at all.
        let dead = chatgpt("a4", "at-x", Some("rt-4"), FAR);
        store_auth(&store, 4, &dead);
        let usage_store = UsageStore::new(&store.paths);
        let strike = |fp: Option<String>| FetchRecord::Failure {
            error: "auth".into(),
            retry_after: None,
            permanent_auth: true,
            struck_fp: fp,
        };
        usage_store
            .record(
                4,
                &record("a4").identity(),
                strike(credentials::fingerprint(&dead.0)),
                1.0,
            )
            .unwrap();
        // 5 was struck under an older credential: healed on this pass.
        store_auth(&store, 5, &chatgpt("a5", "at-x", Some("rt-5-new"), FAR));
        usage_store
            .record(
                5,
                &record("a5").identity(),
                strike(Some("sha256:old".into())),
                1.0,
            )
            .unwrap();

        let collected = run_pass(
            &store,
            &roster,
            opts(CollectMode::StoreOnly, Some(1), &[2, 3, 4, 5, 9]),
        )
        .unwrap();
        let sentinel = |slot: u32| collected.entries[&slot].sentinel;
        assert_eq!(sentinel(1), None);
        assert_eq!(sentinel(2), Some(UsageSentinel::ApiKey));
        assert_eq!(sentinel(3), Some(UsageSentinel::NoCredentials));
        assert_eq!(sentinel(4), Some(UsageSentinel::ReloginNeeded));
        assert_eq!(sentinel(5), None, "a newer credential heals the strike");
        assert_eq!(
            sentinel(9),
            Some(UsageSentinel::NoCredentials),
            "unknown slot"
        );
        assert_eq!(
            collected.entries[&5].auth_dead_strikes, 0,
            "cleared in the store"
        );
        let mut ids = BTreeMap::new();
        ids.insert(5, record("a5").identity());
        assert_eq!(usage_store.entries(&ids, 2.0)[&5].auth_dead_strikes, 0);
        assert!(collected.token_persist_failures.is_empty());
    }

    #[test]
    fn expired_active_token_that_is_not_fetched_reads_token_expired() {
        let (_dir, store) = temp_store();
        let roster = roster_with(&[(1, "a1"), (2, "a2")]);
        let expired = chatgpt("a1", "", Some("rt-1"), 1);
        store_auth(&store, 1, &expired);
        write_live(&store, &expired);
        store_auth(&store, 2, &chatgpt("a2", "", Some("rt-2"), 1));
        let collected =
            run_pass(&store, &roster, opts(CollectMode::StoreOnly, Some(1), &[2])).unwrap();
        assert_eq!(
            collected.entries[&1].sentinel,
            Some(UsageSentinel::TokenExpired)
        );
        assert_eq!(
            collected.entries[&2].sentinel, None,
            "only the active slot idles"
        );
        assert_eq!(collected.entries[&1].decision_value(), None);

        // The live file, not the snapshot, decides for the active slot.
        write_live(&store, &chatgpt("a1", "", Some("rt-1b"), FAR));
        let collected =
            run_pass(&store, &roster, opts(CollectMode::StoreOnly, Some(1), &[])).unwrap();
        assert_eq!(collected.entries[&1].sentinel, None);
    }

    #[test]
    fn live_login_resolves_by_identity_then_by_key() {
        let (_dir, store) = temp_store();
        let mut roster = roster_with(&[(1, "a1"), (2, "a2")]);
        assert_eq!(live_login(&store, &roster), CurrentAccount::NoLogin);

        write_live(&store, &chatgpt("a1", "at", Some("rt"), FAR));
        assert_eq!(
            live_login(&store, &roster),
            CurrentAccount::Managed {
                slot: 1,
                email: "a1@example.com".into(),
                api_key: false
            }
        );
        write_live(&store, &chatgpt("a9", "at", Some("rt"), FAR));
        assert_eq!(
            live_login(&store, &roster),
            CurrentAccount::Unmanaged {
                email: "a9@example.com".into()
            }
        );

        roster.record_mut(2).unwrap().kind = Some(AccountKind::ApiKey);
        store_auth(&store, 2, &AuthJson::api_key_auth("sk-2"));
        write_live(&store, &AuthJson::api_key_auth("sk-2"));
        assert_eq!(
            live_login(&store, &roster),
            CurrentAccount::Managed {
                slot: 2,
                email: "a2@example.com".into(),
                api_key: true
            }
        );
        write_live(&store, &AuthJson::api_key_auth("sk-other"));
        assert_eq!(
            live_login(&store, &roster),
            CurrentAccount::Unmanaged {
                email: String::new()
            }
        );
        std::fs::write(store.paths.live_auth_file(), "{}").unwrap();
        assert_eq!(live_login(&store, &roster), CurrentAccount::NoLogin);
        std::fs::write(store.paths.live_auth_file(), "garbage").unwrap();
        assert_eq!(live_login(&store, &roster), CurrentAccount::NoLogin);
    }

    #[test]
    fn persist_rotation_is_compare_and_swap() {
        let (_dir, store) = temp_store();
        let tokens = RefreshedTokens {
            id_token: id_token("a1@example.com", "a1", FAR),
            access_token: "at-new".into(),
            refresh_token: "rt-new".into(),
        };
        // Slot holds the presented token: rotated.
        store_auth(&store, 1, &chatgpt("a1", "at-old", Some("rt-old"), FAR));
        persist_rotation(&store, 1, false, "rt-old", &tokens).unwrap();
        let stored = slot_auth(&store, 1);
        assert_eq!(stored.refresh_token(), Some("rt-new"));
        assert_eq!(stored.access_token(), Some("at-new"));
        assert!(
            stored.last_refresh().unwrap()
                > crate::model::parse_iso("2026-09-29T10:00:00Z").unwrap()
        );

        // Slot moved on (someone else rotated): left alone.
        store_auth(
            &store,
            2,
            &chatgpt("a2", "at-x", Some("rt-someone-else"), FAR),
        );
        persist_rotation(&store, 2, false, "rt-old", &tokens).unwrap();
        assert_eq!(
            slot_auth(&store, 2).refresh_token(),
            Some("rt-someone-else")
        );

        // Active slot: the live file is swapped on match; a stale snapshot is folded.
        let mut live = chatgpt("a1", "at-old", Some("rt-live-presented"), FAR);
        live.0["last_refresh"] = json!("2026-09-29T11:00:00Z");
        write_live(&store, &live);
        store_auth(
            &store,
            3,
            &chatgpt("a1", "at-older", Some("rt-snapshot"), FAR),
        );
        persist_rotation(&store, 3, true, "rt-live-presented", &tokens).unwrap();
        let live_after = AuthJson::read(&store.paths.live_auth_file())
            .unwrap()
            .unwrap();
        assert_eq!(live_after.refresh_token(), Some("rt-new"));
        assert_eq!(
            slot_auth(&store, 3).refresh_token(),
            Some("rt-new"),
            "the newer live copy is folded into the stale snapshot"
        );

        // Live file holds another token: untouched, and the slot is not overwritten.
        write_live(&store, &chatgpt("a1", "at", Some("rt-foreign"), FAR));
        store_auth(&store, 4, &chatgpt("a1", "at", Some("rt-mine"), FAR));
        persist_rotation(&store, 4, true, "rt-presented", &tokens).unwrap();
        assert_eq!(
            AuthJson::read(&store.paths.live_auth_file())
                .unwrap()
                .unwrap()
                .refresh_token(),
            Some("rt-foreign")
        );
        assert_eq!(slot_auth(&store, 4).refresh_token(), Some("rt-mine"));
    }

    // ---- passes against the mock ----

    #[test]
    fn on_demand_pass_fetches_the_active_and_one_due_candidate() {
        let mock = mock();
        let (_dir, store) = temp_store();
        let (a, b, c) = (unique("ond"), unique("ond"), unique("ond"));
        let roster = roster_with(&[(1, &a), (2, &b), (3, &c)]);
        let live = chatgpt(&a, "at-good", Some("rt-keep"), FAR);
        store_auth(&store, 1, &live);
        write_live(&store, &live);
        store_auth(&store, 2, &chatgpt(&b, "at-good", Some("rt-keep"), FAR));
        store_auth(&store, 3, &chatgpt(&c, "at-good", Some("rt-keep"), FAR));

        let collected = run_pass(
            &store,
            &roster,
            opts(CollectMode::OnDemand, Some(1), &[2, 3]),
        )
        .unwrap();
        assert_eq!(mock.for_account(&a).len(), 1, "active fetched");
        let fetched_candidates = mock.for_account(&b).len() + mock.for_account(&c).len();
        assert_eq!(fetched_candidates, 1, "exactly one candidate per pass");
        let active = &collected.entries[&1];
        assert_eq!(active.sentinel, None);
        assert_eq!(
            active
                .last_good
                .as_ref()
                .unwrap()
                .five_hour
                .as_ref()
                .unwrap()
                .pct,
            42.0
        );
        assert!(active.next_poll_at.is_some() && active.poll_interval_s == Some(180.0));
        assert!(active.fresh(now_unix() as f64, 180.0));
        let (fetched, waiting) = if mock.for_account(&b).len() == 1 {
            (2, 3)
        } else {
            (3, 2)
        };
        assert_eq!(collected.entries[&fetched].poll_interval_s, Some(300.0));
        assert_eq!(collected.entries[&waiting].fetched_at, None);
        assert!(collected.token_persist_failures.is_empty());

        // Fresh rows are served from the store; the waiting candidate is now the due one.
        let second = run_pass(
            &store,
            &roster,
            opts(CollectMode::OnDemand, Some(1), &[2, 3]),
        )
        .unwrap();
        assert_eq!(mock.for_account(&a).len(), 1);
        assert_eq!(mock.for_account(&b).len() + mock.for_account(&c).len(), 2);
        assert!(second.entries[&waiting].fetched_at.is_some());

        // Store-only reads what the passes left behind.
        let third = run_pass(
            &store,
            &roster,
            opts(CollectMode::StoreOnly, Some(1), &[2, 3]),
        )
        .unwrap();
        assert_eq!(third.entries[&1].last_good, collected.entries[&1].last_good);
        assert_eq!(mock.for_account(&a).len(), 1);
    }

    #[test]
    fn escalation_fetches_every_candidate() {
        let mock = mock();
        let (_dir, store) = temp_store();
        let ids: Vec<String> = (0..3).map(|_| unique("esc")).collect();
        let roster = roster_with(&[(1, &ids[0]), (2, &ids[1]), (3, &ids[2])]);
        let live = chatgpt(&ids[0], "at-good", Some("rt-keep"), FAR);
        store_auth(&store, 1, &live);
        write_live(&store, &live);
        store_auth(
            &store,
            2,
            &chatgpt(&ids[1], "at-good", Some("rt-keep"), FAR),
        );
        store_auth(
            &store,
            3,
            &chatgpt(&ids[2], "at-good", Some("rt-keep"), FAR),
        );
        let collected = run_pass(
            &store,
            &roster,
            opts(CollectMode::Escalation, Some(1), &[2, 3]),
        )
        .unwrap();
        for id in &ids {
            assert_eq!(mock.for_account(id).len(), 1, "{id}");
        }
        assert!(collected.entries.values().all(|e| e.last_good.is_some()));
    }

    #[test]
    fn reactive_refresh_rotations_are_persisted_for_live_and_slot() {
        let mock = mock();
        let (_dir, store) = temp_store();
        let (a, b) = (unique("rot"), unique("rot"));
        let roster = roster_with(&[(1, &a), (2, &b)]);
        // The live token is stale: 401 → refresh → 200. The snapshot lags behind the live file.
        let live = chatgpt(&a, "at-stale", Some(&format!("rt-live-{a}")), FAR);
        let mut snapshot = live.clone();
        snapshot.0["last_refresh"] = json!("2026-09-28T10:00:00Z");
        store_auth(&store, 1, &snapshot);
        write_live(&store, &live);
        store_auth(
            &store,
            2,
            &chatgpt(&b, "at-stale", Some(&format!("rt-live-{b}")), FAR),
        );

        let collected = run_pass(
            &store,
            &roster,
            opts(CollectMode::Escalation, Some(1), &[2]),
        )
        .unwrap();
        assert!(collected.token_persist_failures.is_empty());
        let trail: Vec<(String, Option<String>)> = mock
            .for_account(&a)
            .into_iter()
            .map(|r| (r.path, r.bearer))
            .collect();
        assert_eq!(
            trail,
            vec![
                ("/usage".to_string(), Some("at-stale".to_string())),
                ("/usage".to_string(), Some("at-good".to_string()))
            ]
        );
        let live_after = AuthJson::read(&store.paths.live_auth_file())
            .unwrap()
            .unwrap();
        assert_eq!(
            live_after.refresh_token(),
            Some(format!("rt-next-{a}").as_str())
        );
        assert_eq!(live_after.access_token(), Some("at-good"));
        assert_eq!(
            slot_auth(&store, 1).refresh_token(),
            Some(format!("rt-next-{a}").as_str())
        );
        assert_eq!(
            slot_auth(&store, 2).refresh_token(),
            Some(format!("rt-next-{b}").as_str())
        );
        assert!(collected.entries[&1].last_good.is_some());
        assert!(collected.entries[&2].last_good.is_some());
        assert_eq!(collected.entries[&1].sentinel, None);
    }

    #[test]
    fn terminal_refresh_verdict_strikes_the_credential() {
        let mock = mock();
        let (_dir, store) = temp_store();
        let (a, b) = (unique("dead"), unique("dead"));
        let roster = roster_with(&[(1, &a), (2, &b)]);
        let live = chatgpt(&a, "at-good", Some("rt-keep"), FAR);
        store_auth(&store, 1, &live);
        write_live(&store, &live);
        let dead = chatgpt(&b, "at-stale", Some("rt-dead"), FAR);
        store_auth(&store, 2, &dead);

        let collected = run_pass(
            &store,
            &roster,
            opts(CollectMode::Escalation, Some(1), &[2]),
        )
        .unwrap();
        assert_eq!(mock.for_account(&b).len(), 1, "no retry after the verdict");
        let entry = &collected.entries[&2];
        assert_eq!(entry.sentinel, Some(UsageSentinel::ReloginNeeded));
        assert_eq!(entry.last_error.as_deref(), Some("auth"));
        assert_eq!(entry.auth_dead_strikes, 1);
        assert_eq!(entry.struck_fingerprint, credentials::fingerprint(&dead.0));
        assert_eq!(
            slot_auth(&store, 2).refresh_token(),
            Some("rt-dead"),
            "nothing rotated"
        );

        // Struck rows are never fetched again until the credential changes.
        run_pass(
            &store,
            &roster,
            opts(CollectMode::Escalation, Some(1), &[2]),
        )
        .unwrap();
        assert_eq!(mock.for_account(&b).len(), 1);
        store_auth(&store, 2, &chatgpt(&b, "at-good", Some("rt-keep"), FAR));
        let healed = run_pass(
            &store,
            &roster,
            opts(CollectMode::Escalation, Some(1), &[2]),
        )
        .unwrap();
        assert_eq!(
            healed.entries[&2].sentinel, None,
            "a new credential heals the strike"
        );
        assert_eq!(healed.entries[&2].auth_dead_strikes, 0);
        assert!(
            healed.entries[&2].in_backoff(now_unix() as f64),
            "the failure backoff still holds until it lifts"
        );
        assert_eq!(mock.for_account(&b).len(), 1);
    }

    #[test]
    fn rate_limits_record_the_retry_after() {
        let mock = mock();
        let (_dir, store) = temp_store();
        let a = unique("lim");
        let roster = roster_with(&[(1, &a)]);
        let live = chatgpt(&a, "at-limited", Some("rt-keep"), FAR);
        store_auth(&store, 1, &live);
        write_live(&store, &live);
        let collected =
            run_pass(&store, &roster, opts(CollectMode::OnDemand, Some(1), &[])).unwrap();
        assert_eq!(mock.for_account(&a).len(), 1);
        let entry = &collected.entries[&1];
        assert_eq!(entry.last_error.as_deref(), Some("http-429"));
        assert_eq!(entry.consecutive_failures, 1);
        assert!(entry.last_429_at.is_some());
        let now = now_unix() as f64;
        assert!(
            entry.backoff_until.unwrap() >= now + 29.0,
            "30 s floor beats a 7 s ask"
        );
        assert_eq!(entry.sentinel, None);
        assert_eq!(entry.decision_value(), None);
    }

    #[test]
    fn refresh_slot_reports_each_outcome() {
        let _mock = mock();
        let (_dir, store) = temp_store();
        let (a, b, c, d) = (unique("rs"), unique("rs"), unique("rs"), unique("rs"));
        let mut roster =
            roster_with(&[(1, &a), (2, &b), (3, &c), (4, &d), (5, "api"), (6, "none")]);
        roster.record_mut(5).unwrap().kind = Some(AccountKind::ApiKey);
        store_auth(&store, 5, &AuthJson::api_key_auth("sk-5"));

        // Far from expiry: nothing happens unless forced.
        store_auth(
            &store,
            1,
            &chatgpt(&a, "at-good", Some(&format!("rt-live-{a}")), FAR),
        );
        assert_eq!(
            refresh_slot(&store, &roster, 1, false),
            RefreshStatus::NotNeeded
        );
        assert_eq!(refresh_slot(&store, &roster, 1, true), RefreshStatus::Ok);
        let rotated = slot_auth(&store, 1);
        assert_eq!(
            rotated.refresh_token(),
            Some(format!("rt-next-{a}").as_str())
        );
        assert_eq!(rotated.access_token(), Some("at-good"));

        // Expiring within the margin: refreshed without force.
        store_auth(
            &store,
            2,
            &chatgpt(&b, "", Some(&format!("rt-live-{b}")), now_unix() + 60),
        );
        assert_eq!(refresh_slot(&store, &roster, 2, false), RefreshStatus::Ok);
        assert_eq!(
            slot_auth(&store, 2).refresh_token(),
            Some(format!("rt-next-{b}").as_str())
        );

        // Dead token: terminal, memorable, and a strike lands in the usage store.
        let dead = chatgpt(&c, "", Some("rt-dead"), 1);
        store_auth(&store, 3, &dead);
        assert_eq!(
            refresh_slot(&store, &roster, 3, false),
            RefreshStatus::Terminal {
                code: "refresh_token_reused".into(),
                memorable: true
            }
        );
        let mut ids = BTreeMap::new();
        ids.insert(3, record(&c).identity());
        let entry = &UsageStore::new(&store.paths).entries(&ids, now_unix() as f64)[&3];
        assert_eq!(entry.auth_dead_strikes, 1);
        assert_eq!(entry.struck_fingerprint, credentials::fingerprint(&dead.0));

        // Expiring with no refresh token, a 5xx, API keys, and missing slots.
        store_auth(&store, 4, &chatgpt(&d, "", None, 1));
        assert_eq!(
            refresh_slot(&store, &roster, 4, false),
            RefreshStatus::NoRefreshToken
        );
        store_auth(&store, 4, &chatgpt(&d, "", Some("rt-unknown"), 1));
        assert!(matches!(
            refresh_slot(&store, &roster, 4, false),
            RefreshStatus::Transient(_)
        ));
        assert_eq!(
            refresh_slot(&store, &roster, 5, true),
            RefreshStatus::NotNeeded
        );
        assert!(matches!(
            refresh_slot(&store, &roster, 6, true),
            RefreshStatus::Transient(_)
        ));
        assert!(matches!(
            refresh_slot(&store, &roster, 7, true),
            RefreshStatus::Transient(_)
        ));
    }

    #[test]
    fn refresh_slot_uses_the_live_copy_for_the_active_login() {
        let _mock = mock();
        let (_dir, store) = temp_store();
        let a = unique("rsl");
        let roster = roster_with(&[(1, &a)]);
        // The snapshot holds a spent token; Codex rotated the live file since.
        store_auth(&store, 1, &chatgpt(&a, "", Some("rt-dead"), 1));
        let mut live = chatgpt(&a, "", Some(&format!("rt-live-{a}")), 1);
        live.0["last_refresh"] = json!("2026-09-29T12:00:00Z");
        write_live(&store, &live);
        assert_eq!(refresh_slot(&store, &roster, 1, false), RefreshStatus::Ok);
        let expected = format!("rt-next-{a}");
        assert_eq!(
            slot_auth(&store, 1).refresh_token(),
            Some(expected.as_str())
        );
        assert_eq!(
            AuthJson::read(&store.paths.live_auth_file())
                .unwrap()
                .unwrap()
                .refresh_token(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn headroom_reads_the_decision_value_only() {
        let mut entry = blank_entry();
        assert_eq!(entry_headroom(&entry, &[]), None);
        entry.last_good = Some(NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: 30.0,
                resets_at: None,
            }),
            ..NormalizedUsage::default()
        });
        entry.age_s = Some(10.0);
        assert_eq!(entry_headroom(&entry, &[]), Some(70.0));
        entry.sentinel = Some(UsageSentinel::TokenExpired);
        assert_eq!(entry_headroom(&entry, &[]), None);
    }
}
