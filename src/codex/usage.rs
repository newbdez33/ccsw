//! Usage API client and normalization.
//!
//! One fetch is: proactive refresh when a token is about to expire, `GET`
//! the usage endpoint, and on 401/403 one reactive refresh plus one retry.
//! Every rotation lands in `FetchOutcome::refreshed` before any later step
//! can fail, because the previous refresh token is dead the moment the
//! server issues a new one.

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::errors::{CswitchError, Result};
use crate::model::{Credits, NormalizedUsage, ScopedWindow, WindowUsage, format_iso};

use super::auth::AuthJson;
use super::jwt::{AccountInfo, is_expiring};
use super::oauth::{self, Presented, RefreshError, RefreshedTokens};

pub const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// The upstream Codex release whose wire behavior this client mirrors.
pub const ALIGNED_CODEX_VERSION: &str = "0.144.1";
/// Refresh proactively when the access or id token expires within this.
pub const REFRESH_MARGIN_SECS: i64 = 1800;

const SECS_7D: i64 = 7 * 86_400;

/// `codex_cli_rs/<version> (<os>; <arch>)`, what Codex itself sends.
pub fn user_agent() -> String {
    format!(
        "codex_cli_rs/{ALIGNED_CODEX_VERSION} ({}; {})",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// The usage endpoint, or the `CSWITCH_USAGE_URL` override (tests).
pub fn usage_url() -> String {
    std::env::var("CSWITCH_USAGE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| USAGE_URL.to_string())
}

/// HTTP client for the usage and token endpoints: Codex's user agent,
/// 30 s connect / 60 s total, rustls with the OS trust store plus bundled
/// roots. Proxy: the argument, then `CSWITCH_PROXY`, then reqwest's own
/// `HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY` handling.
pub fn build_client(proxy: Option<&str>) -> Result<reqwest::Client> {
    let proxy = proxy
        .map(str::to_string)
        .or_else(|| std::env::var("CSWITCH_PROXY").ok())
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty());
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .user_agent(user_agent())
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60));
    if let Some(url) = proxy {
        let proxy = reqwest::Proxy::all(&url).map_err(|err| {
            CswitchError::config(format!(
                "invalid proxy URL '{}': {err}",
                mask_userinfo(&url)
            ))
        })?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|err| CswitchError::config(format!("could not build HTTP client: {err}")))
}

/// `scheme://user:pass@host` → `scheme://***:***@host`, for messages and logs.
fn mask_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://***:***@{host}{}", &rest[authority_end..]),
        None => url.to_string(),
    }
}

/// Why a usage fetch produced no measurement (spec §8.1).
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// A non-2xx the client did not recover from; `retry_after` (seconds)
    /// is the server's hint on a 429.
    Http {
        status: u16,
        retry_after: Option<f64>,
    },
    Timeout,
    Network(String),
    /// 2xx whose body is not JSON or carries no recognized quota fields.
    BadResponse(String),
    /// The token refresh that the fetch needed was refused.
    Auth(RefreshError),
    /// Neither an access token nor a refresh token (an API-key login).
    NoAccessToken,
}

impl FetchError {
    /// The spec's short classification: `http-<code>`, `timeout`, `network`,
    /// `bad-response`, `auth`, `no-access-token`.
    pub fn label(&self) -> String {
        match self {
            Self::Http { status, .. } => format!("http-{status}"),
            Self::Timeout => "timeout".into(),
            Self::Network(_) => "network".into(),
            Self::BadResponse(_) => "bad-response".into(),
            Self::Auth(_) => "auth".into(),
            Self::NoAccessToken => "no-access-token".into(),
        }
    }

    /// The account needs a fresh login: the refresh token was rejected for good.
    pub fn is_terminal_auth(&self) -> bool {
        matches!(self, Self::Auth(err) if err.is_terminal())
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http {
                status,
                retry_after: Some(secs),
            } => write!(f, "HTTP {status} (retry after {secs}s)"),
            Self::Http { status, .. } => write!(f, "HTTP {status}"),
            Self::Timeout => write!(f, "request timed out"),
            Self::Network(detail) => write!(f, "network error: {detail}"),
            Self::BadResponse(detail) => write!(f, "bad response: {detail}"),
            Self::Auth(err) => write!(f, "{err}"),
            Self::NoAccessToken => write!(f, "no access token"),
        }
    }
}

impl std::error::Error for FetchError {}

/// `refreshed` is set whenever the auth server rotated the tokens during the
/// fetch, including when `result` is an error; the caller must persist it
/// before doing anything else with the error.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchOutcome {
    pub refreshed: Option<RefreshedTokens>,
    pub result: std::result::Result<NormalizedUsage, FetchError>,
}

/// Fetch and normalize the usage of one login (see the module docs).
pub async fn fetch_usage(client: &reqwest::Client, auth: &AuthJson) -> FetchOutcome {
    let mut refreshed = None;
    let result = fetch_capturing_refresh(client, auth, &mut refreshed).await;
    FetchOutcome { refreshed, result }
}

struct Routing {
    account_id: Option<String>,
    fedramp: bool,
}

async fn fetch_capturing_refresh(
    client: &reqwest::Client,
    auth: &AuthJson,
    refreshed: &mut Option<RefreshedTokens>,
) -> std::result::Result<NormalizedUsage, FetchError> {
    let info: AccountInfo = auth.account_info();
    let routing = Routing {
        account_id: info.account_id.filter(|id| !id.trim().is_empty()),
        fedramp: info.is_fedramp,
    };
    let id_token = auth.id_token();
    let refresh_token = auth.refresh_token();
    let bearer = auth.access_token().map(str::to_string);
    let mut refresh_failure: Option<RefreshError> = None;

    if let Some(rt) = refresh_token {
        // Either JWT near expiry triggers the refresh so the identity claims
        // do not go stale while the access token still works.
        let expiring = |token: &str| is_expiring(token, REFRESH_MARGIN_SECS).unwrap_or(false);
        let proactive = match bearer.as_deref() {
            None => true,
            Some(access_token) => expiring(access_token) || id_token.is_some_and(expiring),
        };
        if proactive {
            info!("token expiring soon, proactively refreshing");
            let presented = Presented {
                id_token,
                access_token: bearer.as_deref(),
            };
            match oauth::refresh_with(client, rt, presented).await {
                Ok(tokens) => {
                    let new_bearer = tokens.access_token.clone();
                    *refreshed = Some(tokens);
                    // A bearer the server just issued is not refreshed again on 401.
                    return get_usage(client, &new_bearer, &routing).await;
                }
                Err(err) => {
                    warn!("proactive token refresh failed: {err}");
                    refresh_failure = Some(err);
                }
            }
        }
    }

    let Some(bearer) = bearer else {
        return Err(refresh_failure.map_or(FetchError::NoAccessToken, FetchError::Auth));
    };

    let first = get_usage(client, &bearer, &routing).await;
    let (Err(FetchError::Http { status, .. }), Some(rt)) = (&first, refresh_token) else {
        return first;
    };
    if !matches!(status, 401 | 403) {
        return first;
    }
    if let Some(err) = refresh_failure.filter(RefreshError::is_terminal) {
        // The auth server rejected this refresh token moments ago; asking
        // again can only re-trigger reuse detection.
        return Err(FetchError::Auth(err));
    }

    info!("usage API returned HTTP {status}, attempting token refresh");
    let presented = Presented {
        id_token,
        access_token: Some(&bearer),
    };
    let tokens = oauth::refresh_with(client, rt, presented)
        .await
        .map_err(FetchError::Auth)?;
    let new_bearer = tokens.access_token.clone();
    *refreshed = Some(tokens);
    get_usage(client, &new_bearer, &routing).await
}

async fn get_usage(
    client: &reqwest::Client,
    bearer: &str,
    routing: &Routing,
) -> std::result::Result<NormalizedUsage, FetchError> {
    let mut request = client.get(usage_url()).bearer_auth(bearer);
    if let Some(account_id) = &routing.account_id {
        request = request.header("ChatGPT-Account-ID", account_id);
    }
    if routing.fedramp {
        request = request.header("X-OpenAI-Fedramp", "true");
    }
    let response = request.send().await.map_err(transport_error)?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.map_err(transport_error)?;
    debug!("usage API: HTTP {status}, {} bytes", body.len());
    if status.is_success() {
        let value: Value = serde_json::from_slice(&body).map_err(|err| {
            FetchError::BadResponse(format!("invalid JSON (HTTP {status}): {err}"))
        })?;
        return parse_usage(&value).map_err(FetchError::BadResponse);
    }
    let retry_after = (status == reqwest::StatusCode::TOO_MANY_REQUESTS)
        .then(|| retry_after_hint(&headers, &body))
        .flatten();
    Err(FetchError::Http {
        status: status.as_u16(),
        retry_after,
    })
}

fn transport_error(err: reqwest::Error) -> FetchError {
    if err.is_timeout() {
        FetchError::Timeout
    } else {
        FetchError::Network(err.to_string())
    }
}

/// Seconds to wait after a 429: the `Retry-After` header, then the body's
/// `retry_after[_seconds]` (top level or under `error`), then any nested
/// `message` saying "try again in <n> seconds".
pub fn retry_after_hint(headers: &HeaderMap, body: &[u8]) -> Option<f64> {
    if let Some(secs) = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_delay)
    {
        return Some(secs);
    }
    let body: Value = serde_json::from_slice(body).ok()?;
    for pointer in [
        "/retry_after",
        "/retry_after_seconds",
        "/error/retry_after",
        "/error/retry_after_seconds",
    ] {
        if let Some(value) = body.pointer(pointer) {
            if let Some(secs) = value
                .as_f64()
                .filter(|secs| secs.is_finite() && *secs >= 0.0)
            {
                return Some(secs);
            }
            if let Some(secs) = value.as_str().and_then(parse_delay) {
                return Some(secs);
            }
        }
    }
    find_message(&body).and_then(parse_message_delay)
}

fn find_message(value: &Value) -> Option<&str> {
    match value {
        Value::Object(map) => map
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| map.values().find_map(find_message)),
        Value::Array(values) => values.iter().find_map(find_message),
        _ => None,
    }
}

fn parse_message_delay(message: &str) -> Option<f64> {
    let lowercase = message.to_ascii_lowercase();
    let start = lowercase.find("try again in ")? + "try again in ".len();
    let mut parts = message[start..].split_whitespace();
    let value = clean_delay_token(parts.next()?);
    if let Some(secs) = parse_delay(value) {
        return Some(secs);
    }
    let unit = clean_delay_token(parts.next()?);
    parse_delay(&format!("{value} {unit}"))
}

fn clean_delay_token(value: &str) -> &str {
    value
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .trim_end_matches('.')
}

/// `N`, `N seconds`, `Ns`, `Nms`.
fn parse_delay(value: &str) -> Option<f64> {
    let value = value.trim();
    let secs = if let Ok(secs) = value.parse::<u64>() {
        secs as f64
    } else if let Some((number, unit)) = value.split_once(' ')
        && matches!(
            unit.trim().to_ascii_lowercase().as_str(),
            "second" | "seconds"
        )
    {
        number.parse::<f64>().ok()?
    } else if let Some(millis) = value.strip_suffix("ms") {
        millis.parse::<f64>().ok()? / 1_000.0
    } else {
        value.strip_suffix('s')?.parse::<f64>().ok()?
    };
    (secs.is_finite() && secs >= 0.0).then_some(secs)
}

/// Normalize a usage body (spec §8.2). Errs when the body carries no window,
/// no limited signal and no credits: that is not a usage measurement.
pub fn parse_usage(body: &Value) -> std::result::Result<NormalizedUsage, String> {
    let (five_hour, seven_day) = main_windows(body.get("rate_limit"));
    let scoped = body
        .get("additional_rate_limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(scoped_window)
        .collect();
    let reached_type = body.get("rate_limit_reached_type").and_then(|value| {
        value
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| value.as_str())
    });
    let flag = |pointer: &str| {
        body.pointer(pointer)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let limited = matches!(
        reached_type,
        Some(
            "rate_limit_reached"
                | "workspace_owner_credits_depleted"
                | "workspace_member_credits_depleted"
                | "workspace_owner_usage_limit_reached"
                | "workspace_member_usage_limit_reached"
        )
    ) || flag("/spend_control/reached")
        || flag("/rate_limit/limit_reached");

    let usage = NormalizedUsage {
        five_hour,
        seven_day,
        scoped,
        credits: parse_credits(body.get("credits")),
        limited,
        plan_type: body
            .get("plan_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        reset_credits: parse_reset_credits(body),
    };
    if usage.is_empty() {
        return Err("usage response missing recognized quota fields".to_string());
    }
    Ok(usage)
}

struct RawWindow {
    window: WindowUsage,
    window_secs: Option<i64>,
}

/// A window counts only with `used_percent`; `reset_at` is the epoch.
fn parse_window(value: Option<&Value>) -> Option<RawWindow> {
    let value = value.filter(|v| !v.is_null())?;
    let pct = value.get("used_percent")?.as_f64()?;
    Some(RawWindow {
        window: WindowUsage {
            pct,
            resets_at: value
                .get("reset_at")
                .and_then(Value::as_i64)
                .map(format_iso),
        },
        window_secs: value.get("limit_window_seconds").and_then(Value::as_i64),
    })
}

/// `(five_hour, seven_day)`. A free plan reports its only (weekly) window as
/// `primary_window`; that is remapped to `seven_day`.
fn main_windows(rate_limit: Option<&Value>) -> (Option<WindowUsage>, Option<WindowUsage>) {
    let primary = parse_window(rate_limit.and_then(|r| r.get("primary_window")));
    let secondary = parse_window(rate_limit.and_then(|r| r.get("secondary_window")));
    let primary_is_weekly = primary
        .as_ref()
        .and_then(|w| w.window_secs)
        .is_some_and(|secs| secs >= SECS_7D);
    match (primary, secondary) {
        (Some(weekly), None) if primary_is_weekly => (None, Some(weekly.window)),
        (primary, secondary) => (primary.map(|w| w.window), secondary.map(|w| w.window)),
    }
}

/// One `additional_rate_limits[]` pool: its weekly window when present, else
/// its 5-hour window; malformed entries are skipped.
fn scoped_window(item: &Value) -> Option<ScopedWindow> {
    let rate_limit = item.get("rate_limit").filter(|r| r.is_object())?;
    let primary = parse_window(rate_limit.get("primary_window"));
    let secondary = parse_window(rate_limit.get("secondary_window"));
    let window = secondary.or(primary)?.window;
    let name = item
        .get("limit_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("pool");
    Some(ScopedWindow {
        name: name.to_string(),
        pct: window.pct,
        resets_at: window.resets_at,
    })
}

/// `rate_limit_reset_credits` (or camelCase): the server's `available_count`,
/// else the number of `credits[]` entries that are usable Codex resets (an
/// `id`, `status` available or absent, `reset_type` codex or absent).
fn parse_reset_credits(body: &Value) -> Option<u32> {
    let reset = body
        .get("rate_limit_reset_credits")
        .or_else(|| body.get("rateLimitResetCredits"))?
        .as_object()?;
    if let Some(count) = reset
        .get("available_count")
        .or_else(|| reset.get("availableCount"))
        .and_then(Value::as_u64)
    {
        return Some(count.min(u32::MAX as u64) as u32);
    }
    let text = |item: &Value, snake: &str, camel: &str| -> Option<String> {
        item.get(snake)
            .or_else(|| item.get(camel))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let available = reset
        .get("credits")?
        .as_array()?
        .iter()
        .filter(|item| text(item, "id", "id").is_some_and(|id| !id.trim().is_empty()))
        .filter(|item| {
            text(item, "reset_type", "resetType").is_none_or(|kind| kind == "codex_rate_limits")
        })
        .filter(|item| text(item, "status", "status").is_none_or(|status| status == "available"))
        .count();
    Some(available as u32)
}

/// `has_credits` defaults to true (older API); when false the balance is
/// hidden so an included-usage plan does not show `$0.00`. `balance` may be
/// a number or a numeric string.
fn parse_credits(credits: Option<&Value>) -> Option<Credits> {
    let credits = credits?.as_object()?;
    let has_credits = credits.get("has_credits").and_then(Value::as_bool);
    let unlimited = credits.get("unlimited").and_then(Value::as_bool);
    let balance = credits.get("balance").and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
    });
    if has_credits.is_none() && unlimited.is_none() && balance.is_none() {
        return None;
    }
    Some(Credits {
        balance: if has_credits.unwrap_or(true) {
            balance
        } else {
            None
        },
        unlimited: unlimited.unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;
    use serde_json::json;

    fn pro_body() -> Value {
        json!({
            "plan_type": "pro",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 42.0, "limit_window_seconds": 18000, "reset_after_seconds": 9876, "reset_at": 1783843614},
                "secondary_window": {"used_percent": 84.0, "limit_window_seconds": 604800, "reset_after_seconds": 400000, "reset_at": 1784430414}
            },
            "rate_limit_reached_type": null,
            "spend_control": {"reached": false, "individual_limit": {"remaining_percent": 68}},
            "credits": {"has_credits": false, "unlimited": false, "balance": "0"},
            "rate_limit_reset_credits": {
                "available_count": 2,
                "credits": [
                    {"id": "cred_1", "reset_type": "codex_rate_limits", "status": "available"},
                    {"id": "cred_2", "reset_type": "codex_rate_limits", "status": "consumed"}
                ]
            },
            "code_review_rate_limit": null,
            "additional_rate_limits": [
                {
                    "limit_name": "GPT-5.3-Codex-Spark",
                    "metered_feature": "codex_bengalfox",
                    "rate_limit": {
                        "allowed": true, "limit_reached": false,
                        "primary_window": {"used_percent": 10, "limit_window_seconds": 18000, "reset_at": 1783843614},
                        "secondary_window": {"used_percent": 5, "limit_window_seconds": 604800, "reset_at": 1784430414}
                    }
                },
                {"limit_name": "five-hour-only", "rate_limit": {"primary_window": {"used_percent": 7, "reset_at": 1783843614}}},
                {"limit_name": "", "rate_limit": {"secondary_window": {"used_percent": 1}}},
                {"limit_name": "broken", "rate_limit": "nope"},
                {"limit_name": "empty", "rate_limit": {"primary_window": {"reset_at": 1}}}
            ]
        })
    }

    #[test]
    fn full_pro_body_normalizes() {
        let usage = parse_usage(&pro_body()).unwrap();
        assert_eq!(
            usage.five_hour,
            Some(WindowUsage {
                pct: 42.0,
                resets_at: Some(format_iso(1783843614))
            })
        );
        assert_eq!(usage.seven_day.as_ref().unwrap().pct, 84.0);
        assert_eq!(
            usage.seven_day.as_ref().unwrap().resets_at,
            Some(format_iso(1784430414))
        );
        assert_eq!(usage.plan_type.as_deref(), Some("pro"));
        assert!(!usage.limited);
        assert_eq!(
            usage.credits,
            Some(Credits {
                balance: None,
                unlimited: false
            })
        );
        let names: Vec<(&str, f64)> = usage
            .scoped
            .iter()
            .map(|s| (s.name.as_str(), s.pct))
            .collect();
        assert_eq!(
            names,
            vec![
                ("GPT-5.3-Codex-Spark", 5.0),
                ("five-hour-only", 7.0),
                ("pool", 1.0)
            ]
        );
        assert_eq!(usage.scoped[0].resets_at, Some(format_iso(1784430414)));
        assert_eq!(usage.scoped[2].resets_at, None);
        assert_eq!(usage.reset_credits, Some(2));
    }

    #[test]
    fn reset_credits_variants() {
        let with = |reset_credits: Value| {
            let mut body = json!({"rate_limit": {"primary_window": {"used_percent": 1}}});
            body["rate_limit_reset_credits"] = reset_credits;
            parse_usage(&body).unwrap().reset_credits
        };
        assert_eq!(
            parse_usage(&json!({"rate_limit": {"primary_window": {"used_percent": 1}}}))
                .unwrap()
                .reset_credits,
            None,
            "absent field"
        );
        assert_eq!(with(json!(null)), None);
        assert_eq!(with(json!({"available_count": 0})), Some(0));
        assert_eq!(
            with(json!({"available_count": 1, "credits": [
                {"id": "a", "status": "available"}, {"id": "b", "status": "available"}
            ]})),
            Some(1),
            "the server's count wins over the list"
        );
        assert_eq!(
            with(json!({"credits": [
                {"id": "a", "reset_type": "codex_rate_limits", "status": "available"},
                {"id": "b", "reset_type": "codex_rate_limits", "status": "consumed"},
                {"id": "c"},
                {"id": "d", "reset_type": "other_product"},
                {"status": "available"}
            ]})),
            Some(2),
            "without a count: available codex entries with an id"
        );
        let camel = parse_usage(&json!({
            "rate_limit": {"primary_window": {"used_percent": 1}},
            "rateLimitResetCredits": {"availableCount": 3}
        }))
        .unwrap();
        assert_eq!(camel.reset_credits, Some(3));
        assert!(
            parse_usage(&json!({"rate_limit_reset_credits": {"available_count": 2}})).is_err(),
            "reset credits alone are not a usage measurement"
        );
    }

    #[test]
    fn free_plan_weekly_window_is_remapped() {
        let usage = parse_usage(&json!({
            "plan_type": "free",
            "rate_limit": {
                "allowed": false,
                "limit_reached": true,
                "primary_window": {"used_percent": 100, "limit_window_seconds": 604800, "reset_at": 1778468889},
                "secondary_window": null
            }
        }))
        .unwrap();
        assert_eq!(usage.five_hour, None);
        assert_eq!(usage.seven_day.as_ref().unwrap().pct, 100.0);
        assert!(usage.limited, "rate_limit.limit_reached");

        let both = parse_usage(&json!({"rate_limit": {
            "primary_window": {"used_percent": 1, "limit_window_seconds": 604800},
            "secondary_window": {"used_percent": 2, "limit_window_seconds": 604800}
        }}))
        .unwrap();
        assert_eq!(
            both.five_hour.unwrap().pct,
            1.0,
            "no remap when a secondary exists"
        );
    }

    #[test]
    fn limited_signals_and_credits_variants() {
        for reached in [
            json!({"type": "rate_limit_reached"}),
            json!("workspace_member_credits_depleted"),
        ] {
            let usage = parse_usage(&json!({"rate_limit_reached_type": reached})).unwrap();
            assert!(usage.limited);
            assert!(!usage.is_empty());
        }
        let spend = parse_usage(&json!({"spend_control": {"reached": true}})).unwrap();
        assert!(spend.limited);
        let unknown = parse_usage(&json!({
            "rate_limit_reached_type": "something_else",
            "credits": {"balance": 12.5, "unlimited": true}
        }))
        .unwrap();
        assert!(!unknown.limited);
        assert_eq!(
            unknown.credits,
            Some(Credits {
                balance: Some(12.5),
                unlimited: true
            })
        );
        let string_balance =
            parse_usage(&json!({"credits": {"has_credits": true, "balance": "3.25"}})).unwrap();
        assert_eq!(string_balance.credits.unwrap().balance, Some(3.25));
        let empty_credits = parse_usage(&json!({"credits": {"note": "x"}}));
        assert!(empty_credits.is_err());
    }

    #[test]
    fn bodies_without_quota_fields_are_rejected() {
        let err = parse_usage(&json!({"plan_type": "plus"})).unwrap_err();
        assert_eq!(err, "usage response missing recognized quota fields");
        assert!(parse_usage(&json!({"rate_limit": {"primary_window": {"reset_at": 5}}})).is_err());
        assert!(parse_usage(&json!({"additional_rate_limits": [{"limit_name": "p", "rate_limit": {"primary_window": {"used_percent": 0}}}]})).is_ok());
    }

    #[test]
    fn retry_after_sources() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("7"));
        assert_eq!(retry_after_hint(&headers, b"{}"), Some(7.0));
        headers.insert(RETRY_AFTER, HeaderValue::from_static("1500ms"));
        assert_eq!(retry_after_hint(&headers, b""), Some(1.5));
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        assert_eq!(
            retry_after_hint(&headers, b"{\"retry_after\": 12}"),
            Some(12.0)
        );

        let none = HeaderMap::new();
        assert_eq!(
            retry_after_hint(&none, b"{\"error\": {\"retry_after_seconds\": \"9s\"}}"),
            Some(9.0)
        );
        assert_eq!(
            retry_after_hint(
                &none,
                br#"{"detail": {"message": "Rate limited. Please try again in 45 seconds."}}"#
            ),
            Some(45.0)
        );
        assert_eq!(
            retry_after_hint(&none, br#"{"errors": [{"message": "try again in 2s"}]}"#),
            Some(2.0)
        );
        assert_eq!(retry_after_hint(&none, b"not json"), None);
        assert_eq!(retry_after_hint(&none, b"{\"retry_after\": -1}"), None);
    }

    #[test]
    fn client_builder_and_helpers() {
        assert_eq!(
            user_agent(),
            format!(
                "codex_cli_rs/0.144.1 ({}; {})",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        );
        assert!(build_client(None).is_ok());
        assert!(build_client(Some("socks5h://127.0.0.1:1080")).is_ok());
        let err = build_client(Some("::not a url::")).unwrap_err();
        assert_eq!(err.type_name(), "ConfigError");
        let err = build_client(Some("http://user:secret@proxy.local:3128/")).map(|_| ());
        if let Err(err) = err {
            assert!(!err.to_string().contains("secret"));
        }
        assert_eq!(
            mask_userinfo("socks5://u:p@host:1/a@b"),
            "socks5://***:***@host:1/a@b"
        );
        assert_eq!(mask_userinfo("http://host/x@y"), "http://host/x@y");
        assert_eq!(mask_userinfo("garbage"), "garbage");
    }

    #[test]
    fn fetch_error_labels() {
        assert_eq!(
            FetchError::Http {
                status: 429,
                retry_after: Some(3.0)
            }
            .label(),
            "http-429"
        );
        assert_eq!(FetchError::Timeout.label(), "timeout");
        assert_eq!(FetchError::Network("x".into()).label(), "network");
        assert_eq!(FetchError::BadResponse("x".into()).label(), "bad-response");
        let auth = FetchError::Auth(RefreshError::Terminal {
            code: "invalid_grant".into(),
            message: None,
            memorable: false,
        });
        assert_eq!(auth.label(), "auth");
        assert!(auth.is_terminal_auth());
        assert!(!FetchError::Auth(RefreshError::Transient("x".into())).is_terminal_auth());
        assert_eq!(FetchError::NoAccessToken.label(), "no-access-token");
        assert_eq!(
            FetchError::Http {
                status: 429,
                retry_after: Some(3.0)
            }
            .to_string(),
            "HTTP 429 (retry after 3s)"
        );
    }
}
