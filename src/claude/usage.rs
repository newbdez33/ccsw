//! The Anthropic usage API and its normalization (spec §4, §8). No refresh
//! happens here: the collector decides whether a Claude credential may be
//! refreshed (never the active one).

use serde_json::Value;
use tracing::debug;

pub use crate::codex::usage::FetchError;
use crate::codex::usage::{build_client_with_agent, retry_after_hint, transport_error};
use crate::errors::Result;
use crate::model::{NormalizedUsage, ScopedWindow, Spend, WindowUsage};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const BETA_HEADER: &str = "oauth-2025-04-20";

pub fn user_agent() -> String {
    format!("ccsw/{}", crate::VERSION)
}

/// The usage endpoint, or the `CCSW_CLAUDE_USAGE_URL` override (tests).
pub fn usage_url() -> String {
    usage_url_from(std::env::var("CCSW_CLAUDE_USAGE_URL").ok().as_deref())
}

fn usage_url_from(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map_or_else(|| USAGE_URL.to_string(), str::to_string)
}

pub fn build_client(proxy: Option<&str>) -> Result<reqwest::Client> {
    build_client_with_agent(proxy, &user_agent())
}

/// `GET` the usage of one access token.
pub async fn get_usage(
    client: &reqwest::Client,
    bearer: &str,
) -> std::result::Result<NormalizedUsage, FetchError> {
    let response = client
        .get(usage_url())
        .bearer_auth(bearer)
        .header("anthropic-beta", BETA_HEADER)
        .send()
        .await
        .map_err(transport_error)?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.map_err(transport_error)?;
    debug!("Claude usage API: HTTP {status}, {} bytes", body.len());
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

fn window(value: Option<&Value>) -> std::result::Result<Option<WindowUsage>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(raw) = value.get("utilization") else {
        return Ok(None);
    };
    let pct = raw
        .as_f64()
        .ok_or_else(|| format!("utilization is not a number: {raw}"))?;
    Ok(Some(WindowUsage {
        pct,
        resets_at: value
            .get("resets_at")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }))
}

/// `extra_usage` → `spend` only when enabled and complete; credits are cents.
/// The percentage is the API's own `utilization`, never computed from the
/// limit, so a zero limit cannot divide.
fn spend(value: Option<&Value>) -> Option<Spend> {
    let extra = value?.as_object()?;
    if !extra
        .get("is_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let used = extra.get("used_credits")?.as_f64()?;
    let limit = extra.get("monthly_limit")?.as_f64()?;
    let pct = extra.get("utilization")?.as_f64()?;
    Some(Spend {
        used: used / 100.0,
        limit: limit / 100.0,
        pct,
        currency: extra
            .get("currency")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .unwrap_or("USD")
            .to_string(),
        resets_at: extra
            .get("resets_at")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// `limits[]` entries with a model display name and a numeric percent.
fn scoped(value: Option<&Value>) -> Vec<ScopedWindow> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item.pointer("/scope/model/display_name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            let pct = item.get("percent")?.as_f64()?;
            Some(ScopedWindow {
                name: name.to_string(),
                pct,
                resets_at: item
                    .get("resets_at")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            })
        })
        .collect()
}

/// Normalize a usage body (spec §8). A body with no window at all is not a measurement.
pub fn parse_usage(body: &Value) -> std::result::Result<NormalizedUsage, String> {
    let usage = NormalizedUsage {
        five_hour: window(body.get("five_hour"))?,
        seven_day: window(body.get("seven_day"))?,
        scoped: scoped(body.get("limits")),
        credits: None,
        limited: false,
        plan_type: None,
        reset_credits: None,
        spend: spend(body.get("extra_usage")),
    };
    if usage.five_hour.is_none() && usage.seven_day.is_none() && usage.scoped.is_empty() {
        return Err("usage response missing recognized quota fields".to_string());
    }
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> Value {
        json!({
            "five_hour": {"utilization": 22.0, "resets_at": "2026-10-08T15:00:00Z"},
            "seven_day": {"utilization": 61, "resets_at": null},
            "seven_day_opus": null,
            "extra_usage": {"is_enabled": true, "used_credits": 72900, "monthly_limit": 500000, "utilization": 14.58, "currency": "USD"},
            "limits": [
                {"kind": "weekly_scoped", "group": "weekly", "percent": 100, "resets_at": "2026-10-10T00:00:00Z", "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}, "is_active": true},
                {"kind": "session", "percent": 5, "scope": null},
                {"kind": "weekly_scoped", "percent": "7", "scope": {"model": {"display_name": "Opus"}}},
                {"kind": "weekly_scoped", "percent": 3, "scope": {"model": {"display_name": ""}}}
            ]
        })
    }

    #[test]
    fn full_body_normalizes() {
        let usage = parse_usage(&body()).unwrap();
        assert_eq!(
            usage.five_hour,
            Some(WindowUsage {
                pct: 22.0,
                resets_at: Some("2026-10-08T15:00:00Z".into())
            })
        );
        assert_eq!(
            usage.seven_day,
            Some(WindowUsage {
                pct: 61.0,
                resets_at: None
            })
        );
        let spend = usage.spend.unwrap();
        assert_eq!(
            (spend.used, spend.limit, spend.pct, spend.currency.as_str()),
            (729.0, 5000.0, 14.58, "USD")
        );
        assert_eq!(spend.resets_at, None);
        assert_eq!(
            usage.scoped,
            vec![ScopedWindow {
                name: "Fable".into(),
                pct: 100.0,
                resets_at: Some("2026-10-10T00:00:00Z".into())
            }]
        );
        assert!(!usage.limited);
        assert_eq!(usage.credits, None);
        assert_eq!(usage.plan_type, None);
    }

    #[test]
    fn spend_needs_every_field_and_is_disabled_without_the_flag() {
        let mut b = body();
        b["extra_usage"]["is_enabled"] = json!(false);
        assert_eq!(parse_usage(&b).unwrap().spend, None);
        let mut b = body();
        b["extra_usage"]["monthly_limit"] = Value::Null;
        assert_eq!(parse_usage(&b).unwrap().spend, None);
        let mut b = body();
        b["extra_usage"]["currency"] = Value::Null;
        b["extra_usage"]["resets_at"] = json!("2026-11-01T00:00:00Z");
        let spend = parse_usage(&b).unwrap().spend.unwrap();
        assert_eq!(spend.currency, "USD");
        assert_eq!(spend.resets_at.as_deref(), Some("2026-11-01T00:00:00Z"));
    }

    #[test]
    fn bodies_without_windows_are_rejected() {
        assert_eq!(
            parse_usage(&json!({"five_hour": null, "seven_day": null})).unwrap_err(),
            "usage response missing recognized quota fields"
        );
        assert!(parse_usage(&json!({"five_hour": {"utilization": "x"}})).is_err());
        assert!(
            parse_usage(
                &json!({"limits": [{"percent": 1, "scope": {"model": {"display_name": "Fable"}}}]})
            )
            .is_ok()
        );
    }

    #[test]
    fn client_and_urls() {
        assert_eq!(USAGE_URL, "https://api.anthropic.com/api/oauth/usage");
        assert_eq!(BETA_HEADER, "oauth-2025-04-20");
        assert_eq!(user_agent(), format!("ccsw/{}", crate::VERSION));
        assert!(build_client(None).is_ok());
        assert_eq!(usage_url_from(None), USAGE_URL);
        assert_eq!(
            usage_url_from(Some("http://127.0.0.1:1/u")),
            "http://127.0.0.1:1/u"
        );
    }
}
