//! The account snapshot the screens draw from (research notes `cswap-tui.md`
//! §2.1–§2.2): built from a [`ListSnapshot`], reconciled so usage never
//! regresses, and merged by generation.

use crate::provider::Provider;
use crate::store::usage_store::{UsageEntry, UsageSentinel};
use crate::switcher::ListSnapshot;

#[derive(Debug, Clone, PartialEq)]
pub struct AccountSnapshot {
    pub number: u32,
    pub provider: Provider,
    pub email: String,
    pub tag: String,
    pub alias: Option<String>,
    pub disabled: bool,
    pub api_key: bool,
    pub is_active: bool,
    pub usage: UsageEntry,
}

impl AccountSnapshot {
    /// `alias (email)` when an alias is set, else the email.
    pub fn label(&self) -> String {
        match self.alias.as_deref().filter(|a| !a.is_empty()) {
            Some(alias) => format!("{alias} ({})", self.email),
            None => self.email.clone(),
        }
    }

    /// A switch target: enabled and backed by stored credentials.
    pub fn switchable(&self) -> bool {
        !self.disabled && self.usage.sentinel != Some(UsageSentinel::NoCredentials)
    }

    fn same_account(&self, other: &AccountSnapshot) -> bool {
        self.number == other.number
            && self.provider == other.provider
            && self.email == other.email
            && self.api_key == other.api_key
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountsSnapshot {
    pub active_number: Option<u32>,
    pub accounts: Vec<AccountSnapshot>,
    /// Unix seconds when the snapshot was taken.
    pub taken_at: f64,
}

impl AccountsSnapshot {
    pub fn empty(taken_at: f64) -> Self {
        Self {
            active_number: None,
            accounts: Vec::new(),
            taken_at,
        }
    }

    pub fn from_list(list: ListSnapshot, taken_at: f64) -> Self {
        let accounts = list
            .rows
            .into_iter()
            .map(|row| AccountSnapshot {
                number: row.slot,
                provider: row.record.provider,
                email: row.record.email.clone(),
                tag: row.record.display_tag(),
                alias: row.record.alias.clone().filter(|a| !a.is_empty()),
                disabled: row.record.disabled,
                api_key: row.record.is_api_key(),
                is_active: row.is_active,
                usage: row.usage,
            })
            .collect();
        Self {
            active_number: list.actives.lowest(),
            accounts,
            taken_at,
        }
    }

    /// The same snapshot restricted to one provider's accounts.
    pub fn only(&self, provider: Provider) -> AccountsSnapshot {
        AccountsSnapshot {
            active_number: self
                .accounts
                .iter()
                .filter(|a| a.provider == provider && a.is_active)
                .map(|a| a.number)
                .min(),
            accounts: self
                .accounts
                .iter()
                .filter(|a| a.provider == provider)
                .cloned()
                .collect(),
            taken_at: self.taken_at,
        }
    }

    pub fn is_mixed(&self) -> bool {
        Provider::ALL
            .iter()
            .filter(|p| self.accounts.iter().any(|a| a.provider == **p))
            .count()
            > 1
    }

    /// The accounts of each present provider, in `Provider::ALL` order.
    pub fn grouped(&self) -> Vec<(Provider, Vec<&AccountSnapshot>)> {
        Provider::ALL
            .into_iter()
            .filter_map(|provider| {
                let group: Vec<&AccountSnapshot> = self
                    .accounts
                    .iter()
                    .filter(|a| a.provider == provider)
                    .collect();
                (!group.is_empty()).then_some((provider, group))
            })
            .collect()
    }

    pub fn account(&self, number: u32) -> Option<&AccountSnapshot> {
        self.accounts.iter().find(|a| a.number == number)
    }

    pub fn active(&self) -> Option<&AccountSnapshot> {
        self.accounts.iter().find(|a| a.is_active)
    }

    /// Slot numbers in section order (see [`Self::grouped`]), the order the
    /// card lists draw and the cursor walks.
    pub fn numbers(&self) -> Vec<u32> {
        self.grouped()
            .into_iter()
            .flat_map(|(_, group)| group.into_iter().map(|a| a.number))
            .collect()
    }

    /// A newer generation replaces everything, but per account the usage
    /// never regresses: an older `fetched_at` keeps the previous measurement
    /// (with its age recomputed) and a `token expired` sentinel stays until a
    /// newer fetch lands.
    pub fn reconcile(mut self, previous: Option<&AccountsSnapshot>) -> Self {
        let Some(previous) = previous else {
            return self;
        };
        self.taken_at = self.taken_at.max(previous.taken_at);
        for account in &mut self.accounts {
            let Some(old) = previous.accounts.iter().find(|p| p.same_account(account)) else {
                continue;
            };
            reconcile_usage(&mut account.usage, &old.usage, self.taken_at);
        }
        self
    }

    /// An out-of-order result only refreshes usage rows of accounts still present.
    pub fn merge_usage(&mut self, older: &AccountsSnapshot) {
        self.taken_at = self.taken_at.max(older.taken_at);
        for account in &mut self.accounts {
            if let Some(other) = older.accounts.iter().find(|o| o.same_account(account))
                && other.usage.fetched_at > account.usage.fetched_at
            {
                account.usage = other.usage.clone();
                if let Some(at) = account.usage.fetched_at {
                    account.usage.age_s = Some((self.taken_at - at).max(0.0));
                }
            }
        }
    }
}

fn reconcile_usage(new: &mut UsageEntry, old: &UsageEntry, now: f64) {
    let regressed = match (old.fetched_at, new.fetched_at) {
        (Some(before), Some(after)) => after < before,
        (Some(_), None) => true,
        _ => false,
    };
    if regressed {
        let sentinel = new.sentinel;
        *new = old.clone();
        if sentinel.is_some() {
            new.sentinel = sentinel;
        }
        if let Some(at) = new.fetched_at {
            new.age_s = Some((now - at).max(0.0));
        }
        return;
    }
    let newer_fetch = match (old.fetched_at, new.fetched_at) {
        (Some(before), Some(after)) => after > before,
        (None, Some(_)) => true,
        _ => false,
    };
    if old.sentinel == Some(UsageSentinel::TokenExpired) && new.sentinel.is_none() && !newer_fetch {
        new.sentinel = Some(UsageSentinel::TokenExpired);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccountRecord, ActiveSlots, NormalizedUsage, WindowUsage};
    use crate::switcher::AccountRow;

    fn entry(fetched_at: Option<f64>, pct: f64) -> UsageEntry {
        UsageEntry {
            sentinel: None,
            last_good: Some(NormalizedUsage {
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

    fn snap(fetched_at: Option<f64>, pct: f64, taken_at: f64) -> AccountsSnapshot {
        AccountsSnapshot {
            active_number: Some(1),
            accounts: vec![AccountSnapshot {
                number: 1,
                provider: Provider::Codex,
                email: "a@b.c".into(),
                tag: "personal".into(),
                alias: None,
                disabled: false,
                api_key: false,
                is_active: true,
                usage: entry(fetched_at, pct),
            }],
            taken_at,
        }
    }

    #[test]
    fn from_list_maps_rows() {
        let mut record = AccountRecord::new("a@b.c");
        record.provider = Provider::Claude;
        record.alias = Some("dev".into());
        record.plan_type = Some("pro".into());
        let list = ListSnapshot {
            actives: ActiveSlots {
                codex: Some(3),
                claude: None,
            },
            rows: vec![AccountRow {
                slot: 3,
                record,
                usage: entry(Some(900.0), 10.0),
                is_active: true,
            }],
            warnings: Vec::new(),
        };
        let snapshot = AccountsSnapshot::from_list(list, 1000.0);
        let account = &snapshot.accounts[0];
        assert_eq!(account.number, 3);
        assert_eq!(account.provider, Provider::Claude);
        assert_eq!(account.tag, "personal", "Claude rows carry no plan label");
        assert_eq!(account.label(), "dev (a@b.c)");
        assert!(account.is_active && account.switchable());
        assert_eq!(snapshot.active_number, Some(3));
        assert_eq!(snapshot.numbers(), vec![3]);
    }

    #[test]
    fn reconcile_never_regresses_fetched_at() {
        let previous = snap(Some(900.0), 50.0, 950.0);
        let older = snap(Some(800.0), 20.0, 1000.0).reconcile(Some(&previous));
        let usage = &older.accounts[0].usage;
        assert_eq!(usage.fetched_at, Some(900.0));
        assert_eq!(
            usage
                .last_good
                .as_ref()
                .unwrap()
                .five_hour
                .as_ref()
                .unwrap()
                .pct,
            50.0
        );
        assert_eq!(
            usage.age_s,
            Some(100.0),
            "age recomputed at the new taken_at"
        );
        assert_eq!(older.taken_at, 1000.0);

        let newer = snap(Some(1000.0), 70.0, 1000.0).reconcile(Some(&previous));
        assert_eq!(newer.accounts[0].usage.fetched_at, Some(1000.0));
    }

    #[test]
    fn token_expired_is_sticky_until_a_newer_fetch() {
        let mut previous = snap(Some(900.0), 50.0, 950.0);
        previous.accounts[0].usage.sentinel = Some(UsageSentinel::TokenExpired);
        let same = snap(Some(900.0), 50.0, 1000.0).reconcile(Some(&previous));
        assert_eq!(
            same.accounts[0].usage.sentinel,
            Some(UsageSentinel::TokenExpired)
        );
        let fresh = snap(Some(1000.0), 50.0, 1000.0).reconcile(Some(&previous));
        assert_eq!(fresh.accounts[0].usage.sentinel, None);
    }

    #[test]
    fn merge_usage_only_takes_newer_rows() {
        let mut current = snap(Some(900.0), 50.0, 950.0);
        current.accounts.push(AccountSnapshot {
            number: 2,
            provider: Provider::Codex,
            email: "b@b.c".into(),
            tag: "personal".into(),
            alias: None,
            disabled: false,
            api_key: false,
            is_active: false,
            usage: entry(None, 0.0),
        });
        let mut older = snap(Some(950.0), 60.0, 1000.0);
        older.active_number = Some(9);
        current.merge_usage(&older);
        assert_eq!(current.active_number, Some(1), "metadata is never restored");
        assert_eq!(current.accounts[0].usage.fetched_at, Some(950.0));
        assert_eq!(current.accounts[0].usage.age_s, Some(50.0));
        assert_eq!(current.accounts.len(), 2);
        assert_eq!(current.taken_at, 1000.0);
    }

    #[test]
    fn grouping_and_mixed_detection() {
        use crate::tui::test_support::{account, claude_account, entry};
        let single = crate::tui::test_support::snapshot(
            vec![account(1, "a@x.y", true, entry(None, None))],
            1.0,
        );
        assert!(!single.is_mixed());
        assert_eq!(single.grouped().len(), 1);
        let mixed = crate::tui::test_support::snapshot(
            vec![
                account(1, "a@x.y", true, entry(None, None)),
                claude_account(2, "c@x.y", true, entry(None, None)),
                account(3, "b@x.y", false, entry(None, None)),
            ],
            1.0,
        );
        assert!(mixed.is_mixed());
        let groups = mixed.grouped();
        assert_eq!(groups[0].0, Provider::Codex);
        assert_eq!(
            groups[0].1.iter().map(|a| a.number).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(groups[1].0, Provider::Claude);
        assert_eq!(groups[1].1[0].number, 2);
        assert_eq!(mixed.active_number, Some(1), "the lowest active slot");
    }
    #[test]
    fn only_keeps_one_providers_accounts() {
        let mut snapshot = AccountsSnapshot::empty(0.0);
        snapshot.active_number = Some(1);
        snapshot.accounts = vec![
            crate::tui::test_support::account(
                1,
                "a@x",
                true,
                crate::tui::test_support::entry(None, None),
            ),
            crate::tui::test_support::claude_account(
                2,
                "b@x",
                true,
                crate::tui::test_support::entry(None, None),
            ),
        ];
        let claude = snapshot.only(Provider::Claude);
        assert_eq!(claude.active_number, Some(2));
        assert_eq!(claude.active().map(|a| a.number), Some(2));
        assert_eq!(claude.taken_at, snapshot.taken_at);
        assert_eq!(claude.accounts.len(), 1);
        assert_eq!(claude.accounts[0].number, 2);
        assert!(
            snapshot
                .only(Provider::Codex)
                .accounts
                .iter()
                .all(|a| a.provider == Provider::Codex)
        );
    }
}
