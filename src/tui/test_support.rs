//! Snapshot builders shared by the unit tests.

use crate::model::{NormalizedUsage, WindowUsage};
use crate::provider::Provider;
use crate::store::usage_store::UsageEntry;

use super::snapshot::{AccountSnapshot, AccountsSnapshot};

/// An entry measured at `fetched_at` (age relative to `now = 1000`), with a
/// 5h window at `pct` when given.
pub(crate) fn entry(fetched_at: Option<f64>, pct: Option<f64>) -> UsageEntry {
    UsageEntry {
        sentinel: None,
        last_good: pct.map(|pct| NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct,
                resets_at: None,
            }),
            ..NormalizedUsage::default()
        }),
        fetched_at,
        age_s: fetched_at.map(|at| 1000.0 - at),
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

pub(crate) fn account(
    number: u32,
    email: &str,
    is_active: bool,
    usage: UsageEntry,
) -> AccountSnapshot {
    AccountSnapshot {
        number,
        provider: Provider::Codex,
        email: email.to_string(),
        tag: "personal".to_string(),
        alias: None,
        disabled: false,
        api_key: false,
        is_active,
        usage,
    }
}

pub(crate) fn claude_account(
    number: u32,
    email: &str,
    is_active: bool,
    usage: UsageEntry,
) -> AccountSnapshot {
    let mut account = account(number, email, is_active, usage);
    account.provider = Provider::Claude;
    account
}

pub(crate) fn snapshot(accounts: Vec<AccountSnapshot>, taken_at: f64) -> AccountsSnapshot {
    AccountsSnapshot {
        active_number: accounts
            .iter()
            .filter(|a| a.is_active)
            .map(|a| a.number)
            .min(),
        accounts,
        taken_at,
    }
}
