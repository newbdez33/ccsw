use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::codex::app_server::DaemonRestart;
use crate::collect::CollectMode;
use crate::errors::CcswError;
use crate::jsonout;
use crate::model::{ActiveSlots, format_iso, now_unix};
use crate::provider::Provider;
use crate::switcher::{SwitchEffect, Switcher};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub slot: u32,
    pub provider: Provider,
    pub email: String,
    pub alias: Option<String>,
    pub organization: String,
    pub plan: String,
    pub disabled: bool,
    pub active: bool,
    pub switchable: bool,
    pub usage_status: &'static str,
    pub usage: Option<Value>,
    pub last_good_usage: Option<Value>,
    pub fetched_at: Option<String>,
    pub age_seconds: Option<f64>,
    pub reset_credits: Option<u32>,
    pub reset_credits_end_at: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub revision: String,
    pub active: ActiveSlots,
    pub unmanaged: Vec<Provider>,
    pub accounts: Vec<Account>,
    pub taken_at: i64,
    pub stale: bool,
}

impl Snapshot {
    pub fn capture(switcher: &Switcher) -> crate::errors::Result<Self> {
        let before = switcher.switch_state()?;
        let list = switcher.list_snapshot(CollectMode::StoreOnly)?;
        let after = switcher.switch_state()?;
        if before.revision != after.revision {
            return Err(CcswError::Conflict("state_changed"));
        }
        let now = now_unix();
        let accounts = list
            .into_iter()
            .flat_map(|snapshot| snapshot.rows)
            .map(|row| {
                let active = after.actives.get(row.record.provider) == Some(row.slot);
                let blocked = (!active)
                    .then(|| {
                        switcher
                            .check_switch_credentials(row.slot, &row.record)
                            .err()
                    })
                    .flatten();
                let status = match blocked {
                    Some(CcswError::Conflict("token_expired")) => "token_expired",
                    Some(CcswError::Conflict("relogin_required")) => "relogin_required",
                    Some(_) => "no_credentials",
                    None => jsonout::usage_status(&row.usage),
                };
                let usage = row.usage.decision_value();
                let last_good = row.usage.last_good.as_ref();
                Account {
                    slot: row.slot,
                    provider: row.record.provider,
                    email: row.record.email.clone(),
                    alias: row.record.alias.clone(),
                    organization: row.record.organization_name.clone(),
                    plan: row.record.display_tag(),
                    disabled: row.record.disabled,
                    active,
                    switchable: !row.record.disabled
                        && !after.unmanaged.contains(&row.record.provider)
                        && !matches!(
                            status,
                            "no_credentials"
                                | "token_expired"
                                | "relogin_required"
                                | "keychain_unavailable"
                        ),
                    usage_status: status,
                    usage: usage.map(|u| jsonout::usage_projection(u, row.usage.fetched_at, now)),
                    last_good_usage: if usage.is_none() {
                        last_good.map(|u| jsonout::usage_projection(u, row.usage.fetched_at, now))
                    } else {
                        None
                    },
                    fetched_at: row.usage.fetched_at.map(|at| format_iso(at as i64)),
                    age_seconds: row.usage.age_s,
                    reset_credits: last_good.and_then(|u| u.reset_credits),
                    reset_credits_end_at: last_good.and_then(|u| u.reset_credits_end_at.clone()),
                }
            })
            .collect();
        Ok(Self {
            revision: after.revision,
            active: after.actives,
            unmanaged: after.unmanaged,
            accounts,
            taken_at: now,
            stale: false,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SwitchRequest {
    pub slot: u32,
    pub provider: Provider,
    pub expected_revision: String,
    pub acknowledge_interruption: bool,
    pub request_id: String,
}

impl SwitchRequest {
    pub fn valid(&self) -> bool {
        self.slot > 0
            && self.expected_revision.len() == 64
            && self
                .expected_revision
                .bytes()
                .all(|c| c.is_ascii_hexdigit())
            && (16..=64).contains(&self.request_id.len())
            && self
                .request_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: String,
    pub provider: Provider,
    pub slot: u32,
    pub state: &'static str,
    pub error: Option<Failure>,
    pub effects: Option<Effects>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Effects {
    pub credentials_changed: bool,
    pub daemon: &'static str,
    pub followup: &'static str,
}

impl Effects {
    pub fn from_effect(effect: Option<&SwitchEffect>) -> Self {
        let (changed, daemon, followup) = match effect {
            None | Some(SwitchEffect::Codex(DaemonRestart::Unchanged)) => {
                (false, "unchanged", "The account is already active.")
            }
            Some(SwitchEffect::Codex(DaemonRestart::Restarted)) => (
                true,
                "restarted",
                "The Codex daemon restarted. Restart any codex exec or --no-daemon sessions yourself.",
            ),
            Some(SwitchEffect::Codex(DaemonRestart::NotRunning)) => (
                true,
                "not_running",
                "New Codex sessions use this account. Restart any codex exec or --no-daemon sessions yourself.",
            ),
            Some(SwitchEffect::Codex(DaemonRestart::Failed(_))) => (
                true,
                "failed",
                "Credentials changed, but the Codex daemon still needs a restart. Run codex app-server daemon restart on the host.",
            ),
            Some(SwitchEffect::Claude(backend)) => (true, "not_applicable", backend.followup()),
        };
        Self {
            credentials_changed: changed,
            daemon,
            followup,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct Failure {
    pub code: &'static str,
    pub message: &'static str,
}

impl Failure {
    pub fn core(error: &CcswError) -> Self {
        match error {
            CcswError::Conflict(code) => Self::new(code),
            CcswError::Lock(_) | CcswError::Session(_) => Self::new("host_busy"),
            CcswError::CredentialRead(_) => Self::new("credentials_unavailable"),
            _ => Self::new("operation_failed"),
        }
    }

    pub fn new(code: &'static str) -> Self {
        let message = match code {
            "state_changed" => {
                "Account state changed on the host. Review the latest accounts and try again."
            }
            "account_disabled" => "This account is disabled. Enable it on the host first.",
            "interruption_required" => "Acknowledge that a Codex turn can be interrupted.",
            "unmanaged_login" => {
                "Save the current login with ccsw add on the host before switching remotely."
            }
            "login_unavailable" => {
                "The host login cannot be verified. Unlock its credential store and try again."
            }
            "credentials_unavailable" => {
                "Refresh usage or sign in again on the host to make this account available."
            }
            "token_expired" => {
                "This account's access token expired. Refresh usage or sign in again on the host."
            }
            "relogin_required" => "This account needs a new login. Sign in again on the host.",
            "host_busy" => {
                "The account is in use or the host is busy. Try again after the local operation ends."
            }
            "unauthorized" => "Pair this browser with the code shown in the host terminal.",
            "forbidden" => "The request origin or security token is invalid. Reload this page.",
            "read_only" => "This console is read-only.",
            "pairing_invalid" => {
                "The pairing code is invalid, expired, or already used. Press Enter in the host terminal for a new code."
            }
            "pairing_rate_limited" => "Too many pairing attempts. Wait one minute and try again.",
            "session_limit" => {
                "The server has reached its session limit. Restart it to revoke existing sessions."
            }
            "request_id_reused" => "This request ID was used for a different action.",
            "operation_limit" => {
                "This session has reached its operation limit. Pair a new session from the host terminal."
            }
            "not_found" => {
                "This operation is no longer available. Verify the latest account state before trying again."
            }
            "unavailable" => "The host state is not ready. Wait for the connection to recover.",
            "invalid_request" => "The request is invalid.",
            "switch_partial" => {
                "The active login changed, but the operation did not finish. Check the host before switching again."
            }
            _ => "The operation did not complete. Check the host and refresh the account state.",
        };
        Self { code, message }
    }
}

pub struct ApiError(pub StatusCode, pub Failure);

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str) -> Self {
        Self(status, Failure::new(code))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
