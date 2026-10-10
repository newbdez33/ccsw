//! `--json` payload builders (schemaVersion 2: cswap's schema plus `provider` on rows and
//! switch references, a per-provider `active` map, and additive Codex fields).

use serde_json::{Map, Value, json};

use crate::errors::CcswError;
use crate::model::{
    AccountRecord, ActiveSlots, CurrentAccount, NormalizedUsage, SCHEMA_VERSION, SwitchOutcome,
    WindowUsage, format_iso,
};
use crate::printer::countdown_and_clock;
use crate::store::usage_store::UsageEntry;
use crate::switcher::ProviderStatus;
use crate::usage_math::{compute_pace, parse_reset, projected_exhaustion_ts, will_last_to_reset};

fn one_decimal(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// One window: `pct`, `resetsAt`, `countdown`/`clock` recomputed from `resetsAt`
/// now, and (weekly windows only) the pace fields as of the measurement.
fn window_json(window: &WindowUsage, weekly: bool, fetched_at: Option<f64>, now: i64) -> Value {
    let mut out = Map::new();
    out.insert("pct".into(), json!(window.pct));
    if let Some(resets_at) = &window.resets_at {
        out.insert("resetsAt".into(), json!(resets_at));
        if let Some(ts) = parse_reset(resets_at) {
            let (countdown, clock) = countdown_and_clock(ts, now);
            out.insert("countdown".into(), json!(countdown));
            out.insert("clock".into(), json!(clock));
        }
    }
    if weekly
        && let Some(fetched_at) = fetched_at
        && let Some(pace) = compute_pace(window, fetched_at)
    {
        out.insert("expectedPct".into(), json!(one_decimal(pace.expected_pct)));
        out.insert("aheadOfPace".into(), json!(pace.ahead));
        if let Some(ts) = projected_exhaustion_ts(&pace, fetched_at) {
            out.insert("projectedExhaustionAt".into(), json!(format_iso(ts as i64)));
        }
        if let Some(lasts) = will_last_to_reset(&pace) {
            out.insert("willLastToReset".into(), json!(lasts));
        }
    }
    Value::Object(out)
}

/// The `usage` object: `fiveHour`, `sevenDay`, `scoped[]`, `credits`.
pub fn usage_projection(usage: &NormalizedUsage, fetched_at: Option<f64>, now: i64) -> Value {
    let mut out = Map::new();
    if let Some(window) = &usage.five_hour {
        out.insert(
            "fiveHour".into(),
            window_json(window, false, fetched_at, now),
        );
    }
    if let Some(window) = &usage.seven_day {
        out.insert(
            "sevenDay".into(),
            window_json(window, true, fetched_at, now),
        );
    }
    if !usage.scoped.is_empty() {
        let scoped: Vec<Value> = usage
            .scoped
            .iter()
            .map(|pool| {
                let window = WindowUsage {
                    pct: pool.pct,
                    resets_at: pool.resets_at.clone(),
                };
                let mut value = window_json(&window, true, fetched_at, now);
                value["name"] = json!(pool.name);
                value
            })
            .collect();
        out.insert("scoped".into(), Value::Array(scoped));
    }
    if let Some(credits) = &usage.credits {
        out.insert(
            "credits".into(),
            json!({"balance": credits.balance, "unlimited": credits.unlimited}),
        );
    }
    if let Some(spend) = &usage.spend {
        let mut value = Map::new();
        value.insert("used".into(), json!(spend.used));
        value.insert("limit".into(), json!(spend.limit));
        value.insert("pct".into(), json!(spend.pct));
        value.insert("currency".into(), json!(spend.currency));
        if let Some(resets_at) = &spend.resets_at {
            value.insert("resetsAt".into(), json!(resets_at));
        }
        out.insert("spend".into(), Value::Object(value));
    }
    Value::Object(out)
}

/// The JSON `usageStatus` of a row.
pub fn usage_status(entry: &UsageEntry) -> &'static str {
    match entry.sentinel {
        Some(sentinel) => sentinel.usage_status(),
        None if entry.decision_value().is_some() => "ok",
        None => "unavailable",
    }
}

/// The fields shared by list rows and the managed `status.active` object.
fn row_fields(
    slot: u32,
    record: &AccountRecord,
    entry: &UsageEntry,
    now: i64,
) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("number".into(), json!(slot));
    row.insert("provider".into(), json!(record.provider.as_str()));
    row.insert("email".into(), json!(record.email));
    row.insert("organizationName".into(), json!(record.organization_name));
    row.insert("organizationUuid".into(), json!(record.organization_uuid));
    row.insert(
        "isOrganization".into(),
        json!(!record.organization_uuid.is_empty()),
    );
    row.insert("accountId".into(), json!(record.organization_uuid));
    row.insert("planType".into(), json!(record.plan_type));
    if let Some(alias) = record.alias.as_deref().filter(|a| !a.is_empty()) {
        row.insert("alias".into(), json!(alias));
    }
    if record.disabled {
        row.insert("disabled".into(), json!(true));
    }
    row.insert("usageStatus".into(), json!(usage_status(entry)));
    match entry.decision_value() {
        Some(usage) => {
            row.insert(
                "usage".into(),
                usage_projection(usage, entry.fetched_at, now),
            );
            if let Some(fetched_at) = entry.fetched_at {
                row.insert(
                    "usageFetchedAt".into(),
                    json!(format_iso(fetched_at as i64)),
                );
                row.insert(
                    "usageAgeSeconds".into(),
                    json!(one_decimal(entry.age_s.unwrap_or(0.0))),
                );
            }
        }
        None => {
            row.insert("usage".into(), Value::Null);
            if let Some(last_good) = &entry.last_good {
                row.insert(
                    "lastGoodUsage".into(),
                    usage_projection(last_good, entry.fetched_at, now),
                );
                if let Some(fetched_at) = entry.fetched_at {
                    row.insert(
                        "lastGoodFetchedAt".into(),
                        json!(format_iso(fetched_at as i64)),
                    );
                    row.insert(
                        "lastGoodAgeSeconds".into(),
                        json!(one_decimal(entry.age_s.unwrap_or(0.0))),
                    );
                }
            }
        }
    }
    row
}

/// A `list.accounts[]` row.
pub fn account_row(
    slot: u32,
    record: &AccountRecord,
    entry: &UsageEntry,
    is_active: bool,
    now: i64,
) -> Value {
    let mut row = row_fields(slot, record, entry, now);
    row.insert("active".into(), json!(is_active));
    Value::Object(row)
}

pub fn list_payload(actives: &ActiveSlots, rows: Vec<Value>, warnings: &[String]) -> Value {
    let mut payload = json!({
        "schemaVersion": SCHEMA_VERSION,
        "activeAccountNumber": actives.codex,
        "active": actives,
        "accounts": rows,
    });
    if !warnings.is_empty() {
        payload["warnings"] = json!(warnings);
    }
    payload
}

/// `status --json`: one entry per provider under `active`: null, `{email, managed: false}`,
/// or the managed row (with `managed: true`).
pub fn status_payload(statuses: &[ProviderStatus], total: usize, now: i64) -> Value {
    let mut active = Map::new();
    for status in statuses {
        let value = match (&status.current, &status.row) {
            (CurrentAccount::NoLogin, _) => Value::Null,
            (CurrentAccount::Managed { slot, .. }, Some(row)) => {
                let mut fields = row_fields(*slot, &row.record, &row.usage, now);
                fields.insert("managed".into(), json!(true));
                Value::Object(fields)
            }
            (current, _) => json!({"email": current.email().unwrap_or(""), "managed": false}),
        };
        active.insert(status.provider.as_str().to_string(), value);
    }
    json!({
        "schemaVersion": SCHEMA_VERSION,
        "active": Value::Object(active),
        "totalManagedAccounts": total,
    })
}

/// `switch --json`; `models` is `(names, source)` when a model list was in effect.
pub fn switch_payload(outcome: &SwitchOutcome, models: Option<(&[String], &str)>) -> Value {
    let mut payload = serde_json::to_value(outcome).unwrap_or_else(|_| json!({}));
    payload["schemaVersion"] = json!(SCHEMA_VERSION);
    let provider = json!(outcome.provider.as_str());
    for key in ["from", "to"] {
        if let Some(reference) = payload.get_mut(key).filter(|v| v.is_object()) {
            reference["provider"] = provider.clone();
        }
    }
    if let Some((names, source)) = models
        && !names.is_empty()
    {
        payload["models"] = json!(names);
        payload["modelSource"] = json!(source);
    }
    payload
}

pub fn error_envelope(err: &CcswError) -> Value {
    json!({
        "schemaVersion": SCHEMA_VERSION,
        "error": {"type": err.type_name(), "message": err.to_string()},
    })
}

/// The one document a `--json` command prints: 2-space indent plus a newline.
pub fn render_document(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string());
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccountRef, ActiveSlots, Credits, ScopedWindow};
    use crate::provider::Provider;
    use crate::store::usage_store::UsageSentinel;
    use crate::switcher::{AccountRow, ProviderStatus};

    const NOW: i64 = 1_790_000_000; // 2026-09-21T14:13:20Z

    fn entry(last_good: Option<NormalizedUsage>, age_s: Option<f64>) -> UsageEntry {
        UsageEntry {
            sentinel: None,
            last_good,
            fetched_at: age_s.map(|age| NOW as f64 - age),
            age_s,
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

    fn usage() -> NormalizedUsage {
        NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: 25.0,
                resets_at: Some(format_iso(NOW + 3600)),
            }),
            seven_day: Some(WindowUsage {
                pct: 60.0,
                resets_at: Some(format_iso(NOW + 2 * 86_400)),
            }),
            scoped: vec![ScopedWindow {
                name: "Spark".into(),
                pct: 100.0,
                resets_at: None,
            }],
            credits: Some(Credits {
                balance: Some(12.5),
                unlimited: false,
            }),
            limited: false,
            plan_type: Some("pro".into()),
            reset_credits: None,
            reset_credits_end_at: None,
            spend: None,
        }
    }

    #[test]
    fn projection_carries_windows_pace_and_credits() {
        let value = usage_projection(&usage(), Some(NOW as f64), NOW);
        assert_eq!(value["fiveHour"]["pct"], 25.0);
        assert_eq!(value["fiveHour"]["countdown"], "1h 0m");
        assert!(value["fiveHour"].get("expectedPct").is_none());
        assert_eq!(value["sevenDay"]["countdown"], "2d 0h");
        // Five days into the week: expected 71.4 %, actual 60 → not ahead.
        assert_eq!(value["sevenDay"]["expectedPct"], 71.4);
        assert_eq!(value["sevenDay"]["aheadOfPace"], false);
        assert_eq!(value["sevenDay"]["willLastToReset"], true);
        assert_eq!(value["scoped"][0]["name"], "Spark");
        assert_eq!(value["scoped"][0]["pct"], 100.0);
        assert!(value["scoped"][0].get("resetsAt").is_none());
        assert_eq!(
            value["credits"],
            json!({"balance": 12.5, "unlimited": false})
        );
        let mut with_spend = usage();
        with_spend.spend = Some(crate::model::Spend {
            used: 7.29,
            limit: 50.0,
            pct: 14.58,
            currency: "USD".into(),
            resets_at: Some(format_iso(NOW + 86_400)),
        });
        let value = usage_projection(&with_spend, Some(NOW as f64), NOW);
        assert_eq!(
            value["spend"],
            json!({"used": 7.29, "limit": 50.0, "pct": 14.58, "currency": "USD", "resetsAt": format_iso(NOW + 86_400)})
        );
        assert!(
            usage_projection(&NormalizedUsage::default(), None, NOW)
                .as_object()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn row_status_and_additive_fields() {
        let mut record = AccountRecord::new("a@x.com");
        record.organization_uuid = "acct-1".into();
        record.plan_type = Some("pro".into());
        record.alias = Some("dev".into());
        record.disabled = true;

        let fresh = entry(Some(usage()), Some(10.0));
        let row = account_row(2, &record, &fresh, true, NOW);
        assert_eq!(row["number"], 2);
        assert_eq!(row["provider"], "codex");
        assert_eq!(row["isOrganization"], true);
        assert_eq!(row["accountId"], "acct-1");
        assert_eq!(row["planType"], "pro");
        assert_eq!(row["alias"], "dev");
        assert_eq!(row["disabled"], true);
        assert_eq!(row["active"], true);
        assert_eq!(row["usageStatus"], "ok");
        assert_eq!(row["usage"]["fiveHour"]["pct"], 25.0);
        assert_eq!(row["usageFetchedAt"], format_iso(NOW - 10));
        assert_eq!(row["usageAgeSeconds"], 10.0);
        assert!(row.get("lastGoodUsage").is_none());

        let stale = entry(Some(usage()), Some(4000.0));
        let row = account_row(2, &record, &stale, false, NOW);
        assert_eq!(row["usageStatus"], "unavailable");
        assert!(row["usage"].is_null());
        assert!(row.get("usageFetchedAt").is_none());
        assert_eq!(row["lastGoodUsage"]["sevenDay"]["pct"], 60.0);
        assert_eq!(row["lastGoodAgeSeconds"], 4000.0);

        let mut plain = AccountRecord::new("b@x.com");
        plain.alias = Some(String::new());
        let mut expired = entry(None, None);
        expired.sentinel = Some(UsageSentinel::TokenExpired);
        let row = account_row(3, &plain, &expired, false, NOW);
        assert_eq!(row["usageStatus"], "token_expired");
        assert!(row["usage"].is_null());
        assert!(row.get("alias").is_none());
        assert!(row.get("disabled").is_none());
        assert!(row["planType"].is_null());
        assert_eq!(row["isOrganization"], false);
    }

    #[test]
    fn payload_shapes() {
        let list = list_payload(&ActiveSlots::default(), vec![], &[]);
        assert_eq!(
            list,
            json!({"schemaVersion": 2, "activeAccountNumber": null, "active": {"codex": null, "claude": null}, "accounts": []})
        );
        let mut actives = ActiveSlots::default();
        actives.set(Provider::Codex, Some(1));
        actives.set(Provider::Claude, Some(5));
        let list = list_payload(&actives, vec![json!({"number": 1})], &["w".to_string()]);
        assert_eq!(list["activeAccountNumber"], 1);
        assert_eq!(list["active"], json!({"codex": 1, "claude": 5}));
        assert_eq!(list["warnings"], json!(["w"]));

        let none = [
            ProviderStatus {
                provider: Provider::Codex,
                current: CurrentAccount::NoLogin,
                row: None,
            },
            ProviderStatus {
                provider: Provider::Claude,
                current: CurrentAccount::NoLogin,
                row: None,
            },
        ];
        assert_eq!(
            status_payload(&none, 0, NOW),
            json!({"schemaVersion": 2, "active": {"codex": null, "claude": null}, "totalManagedAccounts": 0})
        );
        let unmanaged = [
            ProviderStatus {
                provider: Provider::Codex,
                current: CurrentAccount::Unmanaged {
                    email: "u@x.com".into(),
                },
                row: None,
            },
            ProviderStatus {
                provider: Provider::Claude,
                current: CurrentAccount::NoLogin,
                row: None,
            },
        ];
        assert_eq!(
            status_payload(&unmanaged, 2, NOW)["active"],
            json!({"codex": {"email": "u@x.com", "managed": false}, "claude": null})
        );
        let mut record = AccountRecord::new("a@x.com");
        record.provider = Provider::Claude;
        let managed = [
            ProviderStatus {
                provider: Provider::Codex,
                current: CurrentAccount::NoLogin,
                row: None,
            },
            ProviderStatus {
                provider: Provider::Claude,
                current: CurrentAccount::Managed {
                    slot: 5,
                    email: "a@x.com".into(),
                    api_key: false,
                },
                row: Some(AccountRow {
                    slot: 5,
                    record,
                    usage: entry(None, None),
                    is_active: true,
                }),
            },
        ];
        let value = status_payload(&managed, 2, NOW);
        assert!(value["active"]["codex"].is_null());
        assert_eq!(value["active"]["claude"]["managed"], true);
        assert_eq!(value["active"]["claude"]["provider"], "claude");
        assert_eq!(value["active"]["claude"]["number"], 5);
        assert!(value["active"]["claude"].get("active").is_none());
        assert_eq!(value["totalManagedAccounts"], 2);

        let outcome = SwitchOutcome {
            switched: true,
            from: Some(AccountRef {
                number: Some(1),
                email: "a@x.com".into(),
            }),
            to: Some(AccountRef {
                number: Some(2),
                email: "b@x.com".into(),
            }),
            strategy: "best".into(),
            reason: "switched".into(),
            message: "Switched to Account-2 (b@x.com)".into(),
            warnings: vec![],
            provider: Provider::Claude,
        };
        let value = switch_payload(&outcome, Some((&["Spark".to_string()], "cli")));
        assert_eq!(value["schemaVersion"], 2);
        assert_eq!(value["provider"], "claude");
        assert_eq!(value["from"]["provider"], "claude");
        assert_eq!(value["to"]["provider"], "claude");
        assert_eq!(value["from"]["number"], 1);
        assert_eq!(value["models"], json!(["Spark"]));
        assert_eq!(value["modelSource"], "cli");
        assert_eq!(value["warnings"], json!([]));
        let value = switch_payload(&outcome, Some((&[], "cli")));
        assert!(value.get("models").is_none());

        let envelope = error_envelope(&CcswError::not_found("x"));
        assert_eq!(
            envelope,
            json!({"schemaVersion": 2, "error": {"type": "AccountNotFoundError", "message": "No account found with identifier: x"}})
        );
        assert!(render_document(&json!({"a": 1})).ends_with("\n"));
        assert_eq!(render_document(&json!({"a": 1})), "{\n  \"a\": 1\n}\n");
    }
}
