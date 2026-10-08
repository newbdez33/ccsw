//! Token refresh against platform.claude.com (spec §4). The refresh token
//! may rotate; a response without one keeps the presented token.

use serde_json::{Value, json};
use tracing::debug;

pub use crate::codex::oauth::RefreshError;
use crate::model::now_unix;

use super::credentials::{ClaudeCredential, OAUTH_KEY};

pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Claude Code's OAuth client id.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// The token endpoint, or the `CCSW_CLAUDE_TOKEN_URL` override (tests).
pub fn token_url() -> String {
    token_url_from(std::env::var("CCSW_CLAUDE_TOKEN_URL").ok().as_deref())
}

fn token_url_from(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map_or_else(|| TOKEN_URL.to_string(), str::to_string)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeTokens {
    pub access_token: String,
    pub expires_at_ms: i64,
    pub refresh_token: Option<String>,
    pub scopes: Option<Vec<String>>,
}

impl ClaudeTokens {
    /// Rewrite `claudeAiOauth` in place; other keys of the block (and of the
    /// object) are kept.
    pub fn apply_to(&self, credential: &mut ClaudeCredential) {
        if !credential.0.is_object() {
            credential.0 = json!({});
        }
        let root = credential.0.as_object_mut().expect("object");
        let block = root.entry(OAUTH_KEY).or_insert_with(|| json!({}));
        if !block.is_object() {
            *block = json!({});
        }
        let block = block.as_object_mut().expect("object");
        block.insert("accessToken".into(), json!(self.access_token));
        block.insert("expiresAt".into(), json!(self.expires_at_ms));
        if let Some(refresh) = &self.refresh_token {
            block.insert("refreshToken".into(), json!(refresh));
        }
        if let Some(scopes) = &self.scopes {
            block.insert("scopes".into(), json!(scopes));
        }
    }
}

pub async fn refresh(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<ClaudeTokens, RefreshError> {
    let url = token_url();
    debug!("sending Claude token refresh request to {url}");
    let response = client
        .post(&url)
        .json(&json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|err| RefreshError::Transient(format!("request failed: {err}")))?;
    let status = response.status();
    debug!("Claude token refresh response: HTTP {status}");
    let body = response.text().await.map_err(|err| {
        RefreshError::Transient(format!("unreadable response (HTTP {status}): {err}"))
    })?;
    resolve(status, &body, now_unix() * 1000)
}

/// Any 4xx but 429/408 rejected the grant; `invalid_grant` is worth remembering.
fn resolve(
    status: reqwest::StatusCode,
    body: &str,
    now_ms: i64,
) -> Result<ClaudeTokens, RefreshError> {
    let terminal = status.is_client_error()
        && !matches!(
            status,
            reqwest::StatusCode::TOO_MANY_REQUESTS | reqwest::StatusCode::REQUEST_TIMEOUT
        );
    let wire: Value = serde_json::from_str(body).map_err(|err| {
        RefreshError::Transient(format!("unparseable response (HTTP {status}): {err}"))
    })?;
    if let Some(code) = wire.get("error").and_then(Value::as_str) {
        let message = wire
            .get("error_description")
            .and_then(Value::as_str)
            .map(str::to_string);
        if terminal {
            return Err(RefreshError::Terminal {
                code: code.to_string(),
                memorable: code == "invalid_grant",
                message,
            });
        }
        let msg_part = if let Some(msg) = message {
            format!("{msg} ")
        } else {
            String::new()
        };
        return Err(RefreshError::Transient(format!(
            "{code}: {msg_part}(HTTP {status})"
        )));
    }
    if !status.is_success() {
        let code = format!("http_{}", status.as_u16());
        if terminal {
            return Err(RefreshError::Terminal {
                code,
                message: None,
                memorable: false,
            });
        }
        return Err(RefreshError::Transient(format!("HTTP {status}")));
    }
    let access_token = wire
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| RefreshError::Transient("response omitted access_token".into()))?;
    // A float such as `3600.0` is still a lifetime; losing it would lose the rotation.
    let expires_in = wire
        .get("expires_in")
        .and_then(Value::as_f64)
        .filter(|secs| secs.is_finite() && *secs >= 0.0)
        .map(|secs| secs.round() as i64)
        .ok_or_else(|| RefreshError::Transient("response omitted expires_in".into()))?;
    Ok(ClaudeTokens {
        access_token: access_token.to_string(),
        expires_at_ms: now_ms + expires_in * 1000,
        refresh_token: wire
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string),
        scopes: wire
            .get("scope")
            .and_then(Value::as_str)
            .map(|s| s.split_whitespace().map(str::to_string).collect()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use serde_json::json;

    #[test]
    fn success_parses_tokens_and_optional_rotation() {
        let tokens = resolve(
            StatusCode::OK,
            r#"{"access_token":"at-new","expires_in":3600,"refresh_token":"rt-new","scope":"user:inference user:profile"}"#,
            1_000,
        )
        .unwrap();
        assert_eq!(
            tokens,
            ClaudeTokens {
                access_token: "at-new".into(),
                expires_at_ms: 1_000 + 3_600_000,
                refresh_token: Some("rt-new".into()),
                scopes: Some(vec!["user:inference".into(), "user:profile".into()]),
            }
        );
        let kept = resolve(
            StatusCode::OK,
            r#"{"access_token":"at","expires_in":60}"#,
            0,
        )
        .unwrap();
        assert_eq!(kept.refresh_token, None);
        assert_eq!(kept.scopes, None);
        assert_eq!(
            resolve(StatusCode::OK, r#"{"expires_in":60}"#, 0),
            Err(RefreshError::Transient(
                "response omitted access_token".into()
            ))
        );
        assert_eq!(
            resolve(StatusCode::OK, r#"{"access_token":"at"}"#, 0),
            Err(RefreshError::Transient(
                "response omitted expires_in".into()
            ))
        );
    }

    #[test]
    fn a_fractional_expires_in_keeps_the_rotation() {
        let tokens = resolve(
            StatusCode::OK,
            r#"{"access_token":"at-new","expires_in":3600.0,"refresh_token":"rt-new"}"#,
            1_000,
        )
        .unwrap();
        assert_eq!(tokens.expires_at_ms, 1_000 + 3_600_000);
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt-new"));
        let rounded = resolve(
            StatusCode::OK,
            r#"{"access_token":"at","expires_in":59.6}"#,
            0,
        )
        .unwrap();
        assert_eq!(rounded.expires_at_ms, 60_000);
        assert_eq!(
            resolve(
                StatusCode::OK,
                r#"{"access_token":"at","expires_in":-1}"#,
                0
            ),
            Err(RefreshError::Transient(
                "response omitted expires_in".into()
            ))
        );
    }

    #[test]
    fn verdicts() {
        let dead = resolve(
            StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant","error_description":"revoked"}"#,
            0,
        )
        .unwrap_err();
        assert_eq!(
            dead,
            RefreshError::Terminal {
                code: "invalid_grant".into(),
                message: Some("revoked".into()),
                memorable: true
            }
        );
        let client =
            resolve(StatusCode::UNAUTHORIZED, r#"{"error":"invalid_client"}"#, 0).unwrap_err();
        assert!(client.is_terminal() && !client.is_memorable());
        let bare = resolve(StatusCode::FORBIDDEN, "{}", 0).unwrap_err();
        assert_eq!(
            bare,
            RefreshError::Terminal {
                code: "http_403".into(),
                message: None,
                memorable: false
            }
        );
        assert_eq!(
            resolve(
                StatusCode::TOO_MANY_REQUESTS,
                r#"{"error":"invalid_grant"}"#,
                0
            )
            .unwrap_err(),
            RefreshError::Transient("invalid_grant: (HTTP 429 Too Many Requests)".into())
        );
        assert!(
            !resolve(StatusCode::REQUEST_TIMEOUT, "{}", 0)
                .unwrap_err()
                .is_terminal()
        );
        assert!(
            !resolve(StatusCode::BAD_GATEWAY, "{}", 0)
                .unwrap_err()
                .is_terminal()
        );
        let garbage =
            resolve(StatusCode::INTERNAL_SERVER_ERROR, "<html>secret</html>", 0).unwrap_err();
        assert!(!garbage.is_terminal());
        assert!(!garbage.to_string().contains("secret"));
    }

    #[test]
    fn apply_to_rewrites_the_oauth_block_only() {
        let mut credential = ClaudeCredential::from_value(json!({
            OAUTH_KEY: {"accessToken": "old", "refreshToken": "rt-old", "expiresAt": 1, "scopes": ["user:inference"], "subscriptionType": "max"},
            "mcpOAuth": {"keep": true}
        }));
        ClaudeTokens {
            access_token: "new".into(),
            expires_at_ms: 99,
            refresh_token: None,
            scopes: None,
        }
        .apply_to(&mut credential);
        assert_eq!(credential.access_token(), Some("new"));
        assert_eq!(
            credential.refresh_token(),
            Some("rt-old"),
            "no rotation keeps the token"
        );
        assert_eq!(credential.expires_at_ms(), Some(99));
        assert_eq!(credential.0[OAUTH_KEY]["subscriptionType"], "max");
        ClaudeTokens {
            access_token: "n2".into(),
            expires_at_ms: 100,
            refresh_token: Some("rt-new".into()),
            scopes: Some(vec!["a".into()]),
        }
        .apply_to(&mut credential);
        assert_eq!(credential.refresh_token(), Some("rt-new"));
        assert_eq!(credential.scopes(), vec!["a"]);
        assert_eq!(credential.0["mcpOAuth"]["keep"], true);
    }

    #[test]
    fn constants_and_override() {
        assert_eq!(TOKEN_URL, "https://platform.claude.com/v1/oauth/token");
        assert_eq!(CLIENT_ID, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
        assert_eq!(token_url_from(None), TOKEN_URL);
        assert_eq!(token_url_from(Some(" ")), TOKEN_URL);
        assert_eq!(
            token_url_from(Some("http://127.0.0.1:1/t")),
            "http://127.0.0.1:1/t"
        );
    }
}
