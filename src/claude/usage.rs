//! The Anthropic usage API and its normalization (spec §4, §8). No refresh
//! happens here: the collector decides whether a Claude credential may be
//! refreshed (never the active one).

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use serde_json::Value;
use tracing::debug;

use crate::codex::app_server::command_on_path;
pub use crate::codex::usage::FetchError;
use crate::codex::usage::{build_client_with_agent, retry_after_hint, transport_error};
use crate::errors::Result;
use crate::model::{NormalizedUsage, ScopedWindow, Spend, WindowUsage, now_unix};
use crate::usage_math::parse_reset;

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const BETA_HEADER: &str = "oauth-2025-04-20";
/// Asks for the `cedar_ember` block: the saved limit resets (`/limit-reset`).
pub const RESETS_QUERY: &str = "cedar_ember=1";
/// The Claude Code version the request claims when the installed one cannot be
/// read from the `claude` executable. Release checklist: set it to the current
/// Claude Code release, since the server refuses the reset block to old clients.
pub const FALLBACK_CLI_VERSION: &str = "2.1.296";

/// The usage endpoint only lists limit resets for the Claude Code CLI surface,
/// so the request identifies as the installed Claude Code, then as ccsw.
pub fn user_agent() -> String {
    user_agent_for(
        installed_cli_version()
            .as_deref()
            .unwrap_or(FALLBACK_CLI_VERSION),
    )
}

pub fn user_agent_for(cli_version: &str) -> String {
    format!(
        "claude-cli/{cli_version} (external, cli) ccsw/{}",
        crate::VERSION
    )
}

/// The version of the `claude` on `PATH`, read from its install layout; no
/// process is started.
pub fn installed_cli_version() -> Option<String> {
    cli_version_of(&command_on_path("claude")?)
}

/// Native installer: `…/versions/<version>`; Homebrew cask:
/// `…/claude-code/<version>/claude`; npm: the package's `package.json`.
/// Symlinks are followed first.
pub fn cli_version_of(path: &Path) -> Option<String> {
    let target = fs::canonicalize(path).ok()?;
    if let Some(version) = target.file_name().and_then(version_name) {
        return Some(version);
    }
    if let Some(version) = target.parent()?.file_name().and_then(version_name) {
        return Some(version);
    }
    let package = target
        .ancestors()
        .skip(1)
        .find(|dir| dir.file_name().is_some_and(|name| name == "claude-code"))?
        .join("package.json");
    let manifest: Value = serde_json::from_slice(&fs::read(package).ok()?).ok()?;
    manifest
        .get("version")?
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// `2.1.294`: digits separated by dots, at least two parts.
fn version_name(name: &OsStr) -> Option<String> {
    let name = name.to_str()?;
    let parts: Vec<&str> = name.split('.').collect();
    (parts.len() >= 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())))
    .then(|| name.to_string())
}

/// The usage endpoint, or the `CCSW_CLAUDE_USAGE_URL` override (tests), with
/// the limit-reset query added.
pub fn usage_url() -> String {
    usage_url_from(std::env::var("CCSW_CLAUDE_USAGE_URL").ok().as_deref())
}

fn usage_url_from(value: Option<&str>) -> String {
    let base = value
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or(USAGE_URL);
    let separator = if base.contains('?') { '&' } else { '?' };
    format!("{base}{separator}{RESETS_QUERY}")
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

/// `cedar_ember` → the resets left on grants that can be spent now: not paused,
/// started, not ended. A block the server marked ineligible describes the
/// request (surface, CLI version), not the account, so it stays unknown.
fn reset_credits(block: Option<&Value>, now: i64) -> Option<u32> {
    let block = block?.as_object()?;
    if block.get("eligible") == Some(&Value::Bool(false)) {
        return None;
    }
    let grants = block.get("grants").and_then(Value::as_array);
    let mut total: u32 = 0;
    for grant in grants.into_iter().flatten() {
        if grant.get("paused") == Some(&Value::Bool(true)) {
            continue;
        }
        let left = grant
            .get("resets_left")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let time = |key: &str| grant.get(key).and_then(Value::as_str).and_then(parse_reset);
        if left == 0
            || time("starts_at").is_some_and(|starts| starts > now)
            || time("ends_at").is_some_and(|ends| ends <= now)
        {
            continue;
        }
        total = total.saturating_add(u32::try_from(left).unwrap_or(u32::MAX));
    }
    Some(total)
}

/// Normalize a usage body (spec §8). A body with no window at all is not a measurement.
pub fn parse_usage(body: &Value) -> std::result::Result<NormalizedUsage, String> {
    parse_usage_at(body, now_unix())
}

/// `parse_usage` against a given clock, which decides which limit resets are live.
pub fn parse_usage_at(body: &Value, now: i64) -> std::result::Result<NormalizedUsage, String> {
    let usage = NormalizedUsage {
        five_hour: window(body.get("five_hour"))?,
        seven_day: window(body.get("seven_day"))?,
        scoped: scoped(body.get("limits")),
        credits: None,
        limited: false,
        plan_type: None,
        reset_credits: reset_credits(body.get("cedar_ember"), now),
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

    fn with_resets(block: Value) -> Value {
        let mut b = body();
        b["cedar_ember"] = block;
        b
    }

    #[test]
    fn usage_url_asks_for_limit_resets() {
        assert_eq!(usage_url_from(None), format!("{USAGE_URL}?cedar_ember=1"));
        assert_eq!(
            usage_url_from(Some("http://127.0.0.1:1/u")),
            "http://127.0.0.1:1/u?cedar_ember=1"
        );
        assert_eq!(
            usage_url_from(Some("http://127.0.0.1:1/u?x=1")),
            "http://127.0.0.1:1/u?x=1&cedar_ember=1"
        );
    }

    #[test]
    fn user_agent_presents_as_claude_code() {
        assert_eq!(
            user_agent_for("2.1.294"),
            format!("claude-cli/2.1.294 (external, cli) ccsw/{}", crate::VERSION)
        );
        assert!(user_agent().starts_with("claude-cli/"), "{}", user_agent());
        assert!(
            FALLBACK_CLI_VERSION
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
                && FALLBACK_CLI_VERSION.split('.').count() == 3
        );
    }

    #[test]
    fn cli_version_is_read_from_the_install_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Native installer: the executable is named after its version.
        let native = root.join("share/claude/versions/2.1.294");
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        std::fs::write(&native, "bin").unwrap();
        assert_eq!(cli_version_of(&native).as_deref(), Some("2.1.294"));
        // Homebrew cask: the version is the parent directory.
        let cask = root.join("Caskroom/claude-code/2.1.20/claude");
        std::fs::create_dir_all(cask.parent().unwrap()).unwrap();
        std::fs::write(&cask, "bin").unwrap();
        assert_eq!(cli_version_of(&cask).as_deref(), Some("2.1.20"));
        // npm: the package's package.json.
        let pkg = root.join("lib/node_modules/@anthropic-ai/claude-code");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name": "@anthropic-ai/claude-code", "version": "2.1.100"}"#,
        )
        .unwrap();
        std::fs::write(pkg.join("cli.js"), "js").unwrap();
        assert_eq!(
            cli_version_of(&pkg.join("cli.js")).as_deref(),
            Some("2.1.100")
        );
        // Anything else (a wrapper script, a missing file) has no version.
        let script = root.join("bin/claude");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        assert_eq!(cli_version_of(&script), None);
        assert_eq!(cli_version_of(&root.join("missing")), None);
        #[cfg(unix)]
        {
            // Symlinks are followed to the real file.
            let link = root.join("bin/claude-link");
            std::os::unix::fs::symlink(&native, &link).unwrap();
            assert_eq!(cli_version_of(&link).as_deref(), Some("2.1.294"));
        }
    }

    #[test]
    fn limit_resets_count_spendable_grants() {
        let now = crate::model::parse_iso("2026-10-08T12:00:00Z").unwrap();
        let grants = json!({"eligible": true, "grants": [
            {"id": "g1", "resets_left": 2, "starts_at": "2026-10-01T00:00:00Z",
             "ends_at": "2026-10-22T16:00:00Z", "paused": false},
            {"id": "g2", "resets_left": 3, "paused": true},
            {"id": "g3", "resets_left": 1, "ends_at": "2026-10-01T00:00:00Z"},
            {"id": "g4", "resets_left": 1, "starts_at": "2026-11-01T00:00:00Z"},
            {"id": "g5", "resets_left": 0},
            {"id": "g6", "resets_left": 1},
            "junk"
        ]});
        assert_eq!(
            parse_usage_at(&with_resets(grants), now)
                .unwrap()
                .reset_credits,
            Some(3)
        );
        assert_eq!(
            parse_usage_at(&with_resets(json!({"eligible": true, "grants": []})), now)
                .unwrap()
                .reset_credits,
            Some(0)
        );
        // An ineligible answer describes the request, not the account: unknown.
        assert_eq!(
            parse_usage_at(
                &with_resets(
                    json!({"eligible": false, "ineligible_reason": "surface", "grants": []})
                ),
                now
            )
            .unwrap()
            .reset_credits,
            None
        );
        assert_eq!(
            parse_usage_at(&with_resets(Value::Null), now)
                .unwrap()
                .reset_credits,
            None
        );
        assert_eq!(parse_usage(&body()).unwrap().reset_credits, None);
    }

    #[test]
    fn client_and_urls() {
        assert_eq!(USAGE_URL, "https://api.anthropic.com/api/oauth/usage");
        assert_eq!(BETA_HEADER, "oauth-2025-04-20");
        assert!(build_client(None).is_ok());
    }
}
