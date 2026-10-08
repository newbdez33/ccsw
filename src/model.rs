//! Shared data model: the roster, account identity, and normalized usage.
//!
//! JSON field names follow cswap's `sequence.json` so the two tools' rosters
//! read alike; the Codex meaning of each field is documented on the struct.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::Provider;

/// `schemaVersion` carried by every `--json` payload.
pub const SCHEMA_VERSION: u32 = 2;

/// How an account authenticates. `None` in the roster means ChatGPT OAuth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountKind {
    #[serde(rename = "api_key")]
    ApiKey,
}

/// One roster record (`accounts["<slot>"]`).
///
/// `organization_uuid` holds the Codex account id (the ChatGPT workspace or
/// personal account the login belongs to) and `organization_name` the
/// workspace name when known; the cswap names are kept for JSON compatibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRecord {
    /// Which product's login this is; absent in v0.1 rosters, which were all Codex.
    #[serde(default)]
    pub provider: Provider,
    pub email: String,
    /// `chatgpt_user_id`, or `""`.
    #[serde(default)]
    pub uuid: String,
    /// Codex account id; `""` for API-key accounts.
    #[serde(default)]
    pub organization_uuid: String,
    /// Workspace name; `""` when unknown or personal.
    #[serde(default)]
    pub organization_name: String,
    /// Last known plan wire value (`plus`, `pro`, `team`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(default)]
    pub added: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AccountKind>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Unknown keys survive a read/write round trip.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl AccountRecord {
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            provider: Provider::Codex,
            email: email.into(),
            uuid: String::new(),
            organization_uuid: String::new(),
            organization_name: String::new(),
            plan_type: None,
            added: now_iso(),
            alias: None,
            kind: None,
            disabled: false,
            extra: BTreeMap::new(),
        }
    }

    pub fn identity(&self) -> Identity {
        Identity {
            email: self.email.clone(),
            account_id: self.organization_uuid.clone(),
        }
    }

    pub fn is_api_key(&self) -> bool {
        matches!(self.kind, Some(AccountKind::ApiKey))
    }

    /// The bracketed tag shown after the email. Codex: workspace name, else
    /// the plan label, else `personal`. Claude: organization name, else
    /// `personal` (cswap's rule; no plan labels).
    pub fn display_tag(&self) -> String {
        if !self.organization_name.is_empty() {
            return self.organization_name.clone();
        }
        if self.provider == Provider::Claude {
            return "personal".to_string();
        }
        plan_label(self.plan_type.as_deref()).unwrap_or_else(|| "personal".to_string())
    }

    /// `alias (email)` when an alias is set, else the email.
    pub fn label(&self) -> String {
        match &self.alias {
            Some(alias) if !alias.is_empty() => format!("{alias} ({})", self.email),
            _ => self.email.clone(),
        }
    }
}

/// `sequence.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Roster {
    pub active_account_number: Option<u32>,
    /// The active slot per provider (`{"codex": 2, "claude": 5}`); the Codex
    /// entry mirrors `activeAccountNumber`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub active_by_provider: BTreeMap<String, u32>,
    #[serde(default)]
    pub last_updated: String,
    #[serde(default)]
    pub sequence: Vec<u32>,
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountRecord>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Roster {
    pub fn empty() -> Self {
        Self {
            active_account_number: None,
            active_by_provider: BTreeMap::new(),
            last_updated: now_iso(),
            sequence: Vec::new(),
            accounts: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }

    pub fn record(&self, slot: u32) -> Option<&AccountRecord> {
        self.accounts.get(&slot.to_string())
    }

    pub fn record_mut(&mut self, slot: u32) -> Option<&mut AccountRecord> {
        self.accounts.get_mut(&slot.to_string())
    }

    /// `max(existing) + 1`, or 1; gaps are never reused.
    pub fn next_free_slot(&self) -> u32 {
        self.accounts
            .keys()
            .filter_map(|key| key.parse::<u32>().ok())
            .max()
            .map_or(1, |max| max + 1)
    }

    /// First slot (in sequence order) of `provider` whose record has this identity.
    pub fn find_slot(&self, provider: Provider, identity: &Identity) -> Option<u32> {
        self.sequence.iter().copied().find(|slot| {
            self.record(*slot)
                .is_some_and(|r| r.provider == provider && r.identity() == *identity)
        })
    }

    /// The slots of one provider, in sequence order.
    pub fn slots_of(&self, provider: Provider) -> Vec<u32> {
        self.sequence
            .iter()
            .copied()
            .filter(|slot| self.record(*slot).is_some_and(|r| r.provider == provider))
            .collect()
    }

    /// A v0.1 roster has no map, only `activeAccountNumber`, which is the
    /// Codex slot; fall back to it until the first switch writes the map.
    pub fn active_for(&self, provider: Provider) -> Option<u32> {
        let mapped = self.active_by_provider.get(provider.as_str()).copied();
        match provider {
            Provider::Codex => mapped.or(self.active_account_number),
            Provider::Claude => mapped,
        }
    }

    /// Set one provider's active slot; the Codex entry also drives
    /// `activeAccountNumber`.
    pub fn set_active_for(&mut self, provider: Provider, slot: Option<u32>) {
        match slot {
            Some(slot) => {
                self.active_by_provider
                    .insert(provider.as_str().to_string(), slot);
            }
            None => {
                self.active_by_provider.remove(provider.as_str());
            }
        }
        if provider == Provider::Codex {
            self.active_account_number = slot;
        }
        self.touch();
    }

    pub fn touch(&mut self) {
        self.last_updated = now_iso();
    }
}

/// What makes two logins the same account: the email plus the Codex account id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Identity {
    pub email: String,
    pub account_id: String,
}

impl Identity {
    pub fn new(email: impl Into<String>, account_id: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            account_id: account_id.into(),
        }
    }
}

/// `{"number": 2, "email": "…"}` in JSON payloads; `number` is null for an
/// unmanaged live login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRef {
    pub number: Option<u32>,
    pub email: String,
}

/// One usage window as stored in the usage cache and read by every decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowUsage {
    pub pct: f64,
    /// ISO-8601 seconds, `Z` suffix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

/// A model-specific quota pool (Codex `additional_rate_limits[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopedWindow {
    pub name: String,
    pub pct: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<f64>,
    #[serde(default)]
    pub unlimited: bool,
}

/// Claude's extra-usage spend window (`$$`): dollars, never cents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    pub used: f64,
    pub limit: f64,
    pub pct: f64,
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

fn default_currency() -> String {
    "USD".to_string()
}

impl Spend {
    /// `$12.50 / $50.00`.
    pub fn amounts(&self) -> String {
        format!("${:.2} / ${:.2}", self.used, self.limit)
    }
}

/// The normalized usage measurement (`lastGood` in the usage cache).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NormalizedUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<WindowUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<WindowUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scoped: Vec<ScopedWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<Credits>,
    /// Claude extra-usage spend; absent for Codex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<Spend>,
    /// The API flagged the account as limited; headroom is 0.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub limited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    /// Rate-limit reset credits ("reset cards") the account can still spend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<u32>,
}

impl NormalizedUsage {
    pub fn is_empty(&self) -> bool {
        self.five_hour.is_none()
            && self.seven_day.is_none()
            && self.scoped.is_empty()
            && self.credits.is_none()
            && self.spend.is_none()
            && !self.limited
    }
}

/// What `$CODEX_HOME/auth.json` currently holds, resolved against the roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentAccount {
    /// No live login (file missing or unreadable).
    NoLogin,
    /// A login that matches no managed slot.
    Unmanaged { email: String },
    /// A managed slot.
    Managed {
        slot: u32,
        email: String,
        api_key: bool,
    },
}

impl CurrentAccount {
    pub fn slot(&self) -> Option<u32> {
        match self {
            Self::Managed { slot, .. } => Some(*slot),
            _ => None,
        }
    }

    pub fn email(&self) -> Option<&str> {
        match self {
            Self::NoLogin => None,
            Self::Unmanaged { email } | Self::Managed { email, .. } => Some(email),
        }
    }
}

/// The active slot of each provider, as `list.active` / `status.active` report it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ActiveSlots {
    pub codex: Option<u32>,
    pub claude: Option<u32>,
}

impl ActiveSlots {
    pub fn get(&self, provider: Provider) -> Option<u32> {
        match provider {
            Provider::Codex => self.codex,
            Provider::Claude => self.claude,
        }
    }

    pub fn set(&mut self, provider: Provider, slot: Option<u32>) {
        match provider {
            Provider::Codex => self.codex = slot,
            Provider::Claude => self.claude = slot,
        }
    }

    /// `(provider, slot)` for every provider, in `Provider::ALL` order.
    pub fn iter(&self) -> impl Iterator<Item = (Provider, Option<u32>)> + '_ {
        Provider::ALL.into_iter().map(|p| (p, self.get(p)))
    }

    /// The lowest active slot of any provider (the TUI cursor's starting point).
    pub fn lowest(&self) -> Option<u32> {
        self.codex.into_iter().chain(self.claude).min()
    }
}

/// Result of one switch, in the shape of the `switch --json` payload body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchOutcome {
    pub switched: bool,
    /// The provider the switch acted on.
    #[serde(default)]
    pub provider: Provider,
    pub from: Option<AccountRef>,
    pub to: Option<AccountRef>,
    /// `rotation` | `best` | `next-available` | `direct`.
    pub strategy: String,
    /// `switched` | `already-active` | `activated` | `unmanaged-account` |
    /// `only-one-account` | `candidates-exhausted` | `no-valid-target` |
    /// `usage-unavailable` | `already-best`.
    pub reason: String,
    pub message: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Human label for a Codex plan wire value; `None` for an unknown or absent plan.
pub fn plan_label(wire: Option<&str>) -> Option<String> {
    let label = match wire? {
        "free" => "Free",
        "go" => "Go",
        "plus" => "Plus",
        "prolite" => "Pro 5×",
        "pro" => "Pro 20×",
        "team" => "Team",
        "business" | "self_serve_business_usage_based" => "Business",
        "enterprise" | "enterprise_cbp_usage_based" => "Enterprise",
        "edu" | "education" => "Edu",
        other => return Some(other.to_string()),
    };
    Some(label.to_string())
}

/// Current time as `%Y-%m-%dT%H:%M:%SZ`.
pub fn now_iso() -> String {
    format_iso(now_unix())
}

pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn format_iso(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

/// Parse an RFC-3339 timestamp (with or without fractional seconds) to unix seconds.
pub fn parse_iso(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_defaults_to_codex_on_read_and_is_always_written() {
        let raw = serde_json::json!({
            "activeAccountNumber": 1, "lastUpdated": "x", "sequence": [1],
            "accounts": {"1": {"email": "a@b.c"}}
        });
        let roster: Roster = serde_json::from_value(raw).unwrap();
        assert_eq!(roster.record(1).unwrap().provider, Provider::Codex);
        assert!(roster.active_by_provider.is_empty());
        let json = serde_json::to_value(&roster).unwrap();
        assert_eq!(json["accounts"]["1"]["provider"], "codex");
        assert!(
            json.get("activeByProvider").is_none(),
            "empty map is omitted"
        );
    }

    #[test]
    fn v01_active_slot_is_the_codex_active_and_follows_a_swap() {
        let raw = serde_json::json!({
            "activeAccountNumber": 2, "lastUpdated": "x", "sequence": [1, 2],
            "accounts": {"1": {"email": "a@b.c"}, "2": {"email": "d@b.c"}}
        });
        let mut roster: Roster = serde_json::from_value(raw).unwrap();
        assert_eq!(roster.active_for(Provider::Codex), Some(2));
        assert_eq!(roster.active_for(Provider::Claude), None);
        roster.swap_slots(2, 1).unwrap();
        assert_eq!(roster.active_for(Provider::Codex), Some(1));
        assert_eq!(roster.active_account_number, Some(1));
    }

    #[test]
    fn active_by_provider_mirrors_the_codex_slot() {
        let mut roster = Roster::empty();
        roster.add_record(1, AccountRecord::new("a@b.c"));
        let mut claude = AccountRecord::new("c@b.c");
        claude.provider = Provider::Claude;
        roster.add_record(5, claude);
        roster.set_active_for(Provider::Claude, Some(5));
        assert_eq!(roster.active_account_number, None);
        assert_eq!(roster.active_for(Provider::Claude), Some(5));
        roster.set_active_for(Provider::Codex, Some(1));
        assert_eq!(roster.active_account_number, Some(1));
        assert_eq!(roster.active_for(Provider::Codex), Some(1));
        roster.set_active(Some(1));
        let json = serde_json::to_value(&roster).unwrap();
        assert_eq!(
            json["activeByProvider"],
            serde_json::json!({"claude": 5, "codex": 1})
        );
        roster.set_active_for(Provider::Claude, None);
        assert_eq!(roster.active_for(Provider::Claude), None);
    }

    #[test]
    fn find_slot_is_scoped_by_provider() {
        let mut roster = Roster::empty();
        let mut codex = AccountRecord::new("me@example.com");
        codex.organization_uuid = "acct-1".into();
        let mut claude = AccountRecord::new("me@example.com");
        claude.organization_uuid = "acct-1".into();
        claude.provider = Provider::Claude;
        roster.add_record(1, codex);
        roster.add_record(2, claude);
        let identity = Identity::new("me@example.com", "acct-1");
        assert_eq!(roster.find_slot(Provider::Codex, &identity), Some(1));
        assert_eq!(roster.find_slot(Provider::Claude, &identity), Some(2));
        assert_eq!(roster.slots_of(Provider::Claude), vec![2]);
        assert_eq!(roster.slots_of(Provider::Codex), vec![1]);
    }

    #[test]
    fn claude_display_tag_is_org_or_personal() {
        let mut record = AccountRecord::new("a@b.c");
        record.provider = Provider::Claude;
        record.plan_type = Some("pro".into());
        assert_eq!(
            record.display_tag(),
            "personal",
            "no plan labels for Claude"
        );
        record.organization_name = "Acme".into();
        assert_eq!(record.display_tag(), "Acme");
    }

    #[test]
    fn roster_round_trips_and_omits_absence_signals() {
        let mut roster = Roster::empty();
        let mut record = AccountRecord::new("a@example.com");
        record.organization_uuid = "acct-1".into();
        roster.accounts.insert("1".into(), record);
        roster.sequence.push(1);
        let json = serde_json::to_value(&roster).unwrap();
        let account = &json["accounts"]["1"];
        assert!(account.get("alias").is_none());
        assert!(account.get("kind").is_none());
        assert!(account.get("disabled").is_none());
        assert_eq!(account["organizationUuid"], "acct-1");
        let back: Roster = serde_json::from_value(json).unwrap();
        assert_eq!(back, roster);
    }

    #[test]
    fn api_key_kind_and_disabled_serialize_like_cswap() {
        let mut record = AccountRecord::new("k@example.com");
        record.kind = Some(AccountKind::ApiKey);
        record.disabled = true;
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["kind"], "api_key");
        assert_eq!(json["disabled"], true);
        assert!(record.is_api_key());
    }

    #[test]
    fn unknown_keys_survive() {
        let raw = serde_json::json!({
            "activeAccountNumber": null, "lastUpdated": "x", "sequence": [3],
            "accounts": {"3": {"email": "a@b.c", "future": 1}}, "vendor": "y"
        });
        let roster: Roster = serde_json::from_value(raw).unwrap();
        let json = serde_json::to_value(&roster).unwrap();
        assert_eq!(json["vendor"], "y");
        assert_eq!(json["accounts"]["3"]["future"], 1);
        assert_eq!(
            json["accounts"]["3"]["uuid"], "",
            "defaults are written out"
        );
        assert_eq!(roster.next_free_slot(), 4);
    }

    #[test]
    fn display_tag_prefers_workspace_then_plan() {
        let mut record = AccountRecord::new("a@b.c");
        assert_eq!(record.display_tag(), "personal");
        record.plan_type = Some("pro".into());
        assert_eq!(record.display_tag(), "Pro 20×");
        record.organization_name = "Acme".into();
        assert_eq!(record.display_tag(), "Acme");
        record.alias = Some("dev".into());
        assert_eq!(record.label(), "dev (a@b.c)");
    }

    #[test]
    fn iso_helpers_round_trip() {
        let ts = 1_790_000_000;
        let text = format_iso(ts);
        assert_eq!(text, "2026-09-21T14:13:20Z");
        assert_eq!(parse_iso(&text), Some(ts));
        assert_eq!(parse_iso("2026-09-21T14:13:20.5+00:00"), Some(ts));
        assert_eq!(parse_iso("nope"), None);
    }

    #[test]
    fn spend_serializes_and_counts_as_usage() {
        let usage = NormalizedUsage {
            spend: Some(Spend {
                used: 12.5,
                limit: 50.0,
                pct: 25.0,
                currency: "USD".into(),
                resets_at: None,
            }),
            ..NormalizedUsage::default()
        };
        assert!(!usage.is_empty());
        let json = serde_json::to_value(&usage).unwrap();
        assert_eq!(json["spend"]["used"], 12.5);
        assert!(json["spend"].get("resets_at").is_none());
        let back: NormalizedUsage = serde_json::from_value(json).unwrap();
        assert_eq!(back.spend.as_ref().unwrap().amounts(), "$12.50 / $50.00");
        let legacy: NormalizedUsage =
            serde_json::from_str("{\"five_hour\": {\"pct\": 1.0}}").unwrap();
        assert_eq!(legacy.spend, None);
    }
}
