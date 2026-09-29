//! Token refresh against auth.openai.com.
//!
//! The refresh token is single-use: the server rotates it on every call and
//! answers a replay with `refresh_token_reused`. Callers must persist a
//! rotation before anything else can fail.

use serde::Deserialize;
use tracing::debug;

/// Codex CLI's OAuth client id.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// Auth-server verdicts no retry can change, independent of HTTP status.
const TERMINAL_CODES: &[&str] = &[
    "refresh_token_reused",
    "refresh_token_invalidated",
    "invalid_grant",
    "invalid_client",
    "unauthorized_client",
    "access_denied",
];

/// The terminal verdicts worth remembering across invocations. The rest of
/// `TERMINAL_CODES` is standard OAuth wording that proxies and gateways also
/// emit for transient conditions, so remembering those would leave a working
/// account marked dead.
const MEMORABLE_CODES: &[&str] = &["refresh_token_reused", "refresh_token_invalidated"];

/// The token endpoint, or the `CSWITCH_TOKEN_URL` override (tests).
pub fn token_url() -> String {
    std::env::var("CSWITCH_TOKEN_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| TOKEN_URL.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshedTokens {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshError {
    /// The credential itself was rejected; retrying cannot help.
    Terminal {
        /// Server code, or `http_<status>` when the body had none.
        code: String,
        message: Option<String>,
        /// Whether the verdict should outlive this invocation.
        memorable: bool,
    },
    /// Transport failure, 5xx, 429/408, or an unreadable response.
    Transient(String),
}

impl RefreshError {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal { .. })
    }

    pub fn is_memorable(&self) -> bool {
        matches!(
            self,
            Self::Terminal {
                memorable: true,
                ..
            }
        )
    }

    fn terminal(code: String, message: Option<String>) -> Self {
        let memorable = MEMORABLE_CODES.contains(&code.as_str());
        Self::Terminal {
            code,
            message,
            memorable,
        }
    }
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Terminal { code, message, .. } => {
                write!(f, "token refresh rejected ({code})")?;
                if let Some(message) = message {
                    write!(f, ": {message}")?;
                }
                Ok(())
            }
            Self::Transient(detail) => write!(f, "token refresh failed: {detail}"),
        }
    }
}

impl std::error::Error for RefreshError {}

/// The auth server reports failures in two shapes: OAuth's
/// `{"error": "invalid_grant", "error_description": "…"}` and OpenAI's
/// `{"error": {"code": …, "message": …, "type": …}}`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WireError {
    Code(String),
    Detail {
        code: Option<String>,
        message: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    error: Option<WireError>,
    error_description: Option<String>,
}

impl WireResponse {
    fn error_parts(&self) -> Option<(String, Option<String>)> {
        match self.error.as_ref()? {
            WireError::Code(code) => Some((code.clone(), self.error_description.clone())),
            WireError::Detail {
                code,
                message,
                kind,
            } => Some((
                code.clone()
                    .or_else(|| kind.clone())
                    .unwrap_or_else(|| "unknown_error".to_string()),
                message.clone().or_else(|| self.error_description.clone()),
            )),
        }
    }
}

/// Tokens the caller already holds, used when the response omits a field.
#[derive(Debug, Clone, Copy, Default)]
pub struct Presented<'a> {
    pub id_token: Option<&'a str>,
    pub access_token: Option<&'a str>,
}

/// `POST` the refresh grant as Codex 0.144.1 does (JSON body). A refresh
/// token omitted from the response means it was not rotated.
pub async fn refresh(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<RefreshedTokens, RefreshError> {
    refresh_with(client, refresh_token, Presented::default()).await
}

/// Like [`refresh`], falling back to the presented id/access tokens when the
/// response omits them.
pub async fn refresh_with(
    client: &reqwest::Client,
    refresh_token: &str,
    presented: Presented<'_>,
) -> Result<RefreshedTokens, RefreshError> {
    let url = token_url();
    debug!("sending token refresh request to {url}");
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }))
        .send()
        .await
        .map_err(|err| RefreshError::Transient(format!("request failed: {err}")))?;
    let status = response.status();
    debug!("token refresh response: HTTP {status}");
    // The body is parsed but never logged: unknown error bodies may carry credentials.
    let body = response.text().await.map_err(|err| {
        RefreshError::Transient(format!("unreadable response (HTTP {status}): {err}"))
    })?;
    resolve(status, &body, refresh_token, presented)
}

/// Classify a token-endpoint response.
fn resolve(
    status: reqwest::StatusCode,
    body: &str,
    presented_refresh_token: &str,
    presented: Presented<'_>,
) -> Result<RefreshedTokens, RefreshError> {
    let wire: WireResponse = serde_json::from_str(body).map_err(|err| {
        RefreshError::Transient(format!("unparseable response (HTTP {status}): {err}"))
    })?;

    if let Some((code, message)) = wire.error_parts() {
        if is_terminal(&code, status) {
            return Err(RefreshError::terminal(code, message));
        }
        return Err(RefreshError::Transient(match message {
            Some(message) => format!("{code}: {message}"),
            None => code,
        }));
    }

    // A non-2xx without an error body still issued no tokens.
    if !status.is_success() {
        let code = format!("http_{}", status.as_u16());
        if is_terminal(&code, status) {
            return Err(RefreshError::terminal(code, None));
        }
        return Err(RefreshError::Transient(format!("HTTP {status}")));
    }

    let id_token = wire
        .id_token
        .or_else(|| presented.id_token.map(str::to_string))
        .ok_or_else(|| RefreshError::Transient("response omitted id_token".to_string()))?;
    let access_token = wire
        .access_token
        .or_else(|| presented.access_token.map(str::to_string))
        .ok_or_else(|| RefreshError::Transient("response omitted access_token".to_string()))?;
    Ok(RefreshedTokens {
        id_token,
        access_token,
        refresh_token: wire
            .refresh_token
            .unwrap_or_else(|| presented_refresh_token.to_string()),
    })
}

/// A 4xx from the token endpoint means the credential was rejected; 429 and
/// 408 are load/timing signals and stay retryable.
fn is_terminal(code: &str, status: reqwest::StatusCode) -> bool {
    if matches!(
        status,
        reqwest::StatusCode::TOO_MANY_REQUESTS | reqwest::StatusCode::REQUEST_TIMEOUT
    ) {
        return false;
    }
    TERMINAL_CODES.contains(&code) || status.is_client_error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    fn held() -> Presented<'static> {
        Presented {
            id_token: Some("id-old"),
            access_token: Some("at-old"),
        }
    }

    #[test]
    fn success_uses_response_tokens_and_falls_back_to_presented_ones() {
        let full = resolve(
            StatusCode::OK,
            r#"{"id_token":"id-new","access_token":"at-new","refresh_token":"rt-new"}"#,
            "rt-old",
            held(),
        )
        .unwrap();
        assert_eq!(
            full,
            RefreshedTokens {
                id_token: "id-new".into(),
                access_token: "at-new".into(),
                refresh_token: "rt-new".into()
            }
        );

        let partial = resolve(
            StatusCode::OK,
            r#"{"access_token":"at-new"}"#,
            "rt-old",
            held(),
        )
        .unwrap();
        assert_eq!(partial.id_token, "id-old");
        assert_eq!(partial.access_token, "at-new");
        assert_eq!(partial.refresh_token, "rt-old");

        let none_held = resolve(
            StatusCode::OK,
            r#"{"id_token":"x"}"#,
            "rt-old",
            Presented::default(),
        );
        assert_eq!(
            none_held,
            Err(RefreshError::Transient(
                "response omitted access_token".into()
            ))
        );
    }

    #[test]
    fn both_error_shapes_are_classified() {
        let oauth = resolve(
            StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant","error_description":"expired"}"#,
            "rt",
            held(),
        );
        assert_eq!(
            oauth,
            Err(RefreshError::Terminal {
                code: "invalid_grant".into(),
                message: Some("expired".into()),
                memorable: false
            })
        );

        let openai = resolve(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"refresh_token_reused","message":"already used","param":null,"type":"invalid_request_error"}}"#,
            "rt",
            held(),
        );
        assert_eq!(
            openai,
            Err(RefreshError::Terminal {
                code: "refresh_token_reused".into(),
                message: Some("already used".into()),
                memorable: true
            })
        );
        assert!(openai.as_ref().unwrap_err().is_memorable());

        let typed_only = resolve(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"m","type":"server_error"}}"#,
            "rt",
            held(),
        )
        .unwrap_err();
        assert!(
            matches!(&typed_only, RefreshError::Terminal { code, .. } if code == "server_error")
        );
    }

    #[test]
    fn terminal_codes_win_over_status_and_429_408_stay_transient() {
        let on_500 = resolve(
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":"access_denied"}"#,
            "rt",
            held(),
        )
        .unwrap_err();
        assert!(on_500.is_terminal());
        assert!(!on_500.is_memorable());

        let on_429 = resolve(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":"invalid_grant"}"#,
            "rt",
            held(),
        )
        .unwrap_err();
        assert_eq!(on_429, RefreshError::Transient("invalid_grant".into()));
        let on_408 = resolve(StatusCode::REQUEST_TIMEOUT, "{}", "rt", held()).unwrap_err();
        assert_eq!(
            on_408,
            RefreshError::Transient("HTTP 408 Request Timeout".into())
        );

        let bare_401 = resolve(StatusCode::UNAUTHORIZED, "{}", "rt", held()).unwrap_err();
        assert_eq!(
            bare_401,
            RefreshError::Terminal {
                code: "http_401".into(),
                message: None,
                memorable: false
            }
        );
        let bare_503 = resolve(StatusCode::SERVICE_UNAVAILABLE, "{}", "rt", held()).unwrap_err();
        assert!(!bare_503.is_terminal());
        let unknown_code_200 =
            resolve(StatusCode::OK, r#"{"error":"weird"}"#, "rt", held()).unwrap_err();
        assert_eq!(unknown_code_200, RefreshError::Transient("weird".into()));
    }

    #[test]
    fn unparseable_bodies_are_transient_and_not_echoed() {
        let err = resolve(StatusCode::FORBIDDEN, "<html>secret</html>", "rt", held()).unwrap_err();
        assert!(!err.is_terminal());
        assert!(!err.to_string().contains("secret"), "{err}");
    }

    #[test]
    fn display_and_url_override() {
        assert_eq!(
            RefreshError::terminal("invalid_client".into(), Some("nope".into())).to_string(),
            "token refresh rejected (invalid_client): nope"
        );
        assert_eq!(
            RefreshError::Transient("HTTP 500".into()).to_string(),
            "token refresh failed: HTTP 500"
        );
        assert_eq!(TOKEN_URL, "https://auth.openai.com/oauth/token");
    }
}
