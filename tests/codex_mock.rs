//! `codex::usage::fetch_usage` against an axum mock of the usage, reset credit
//! list and token endpoints, reached through `CCSW_USAGE_URL` / `CCSW_TOKEN_URL`
//! overrides.
//!
//! The mock answers by the credential it is shown: the bearer token picks the
//! usage reply and the refresh token picks the token reply, so every scenario
//! is a different `auth.json` against the same server.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use ccsw::codex::auth::AuthJson;
use ccsw::codex::oauth::RefreshError;
use ccsw::codex::usage::{FetchError, FetchOutcome, build_client, fetch_usage};
use ccsw::model::now_unix;

/// The endpoint overrides are process-wide, so scenarios run one at a time.
static ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const ACCOUNT_ID: &str = "acct-1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Recorded {
    path: String,
    bearer: Option<String>,
    account_id: Option<String>,
    fedramp: Option<String>,
    user_agent: Option<String>,
    accept: Option<String>,
    beta: Option<String>,
    originator: Option<String>,
    body: Value,
}

#[derive(Default)]
struct Mock {
    requests: Mutex<Vec<Recorded>>,
}

impl Mock {
    fn record(&self, path: &str, headers: &HeaderMap, body: Value) {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        self.requests.lock().unwrap().push(Recorded {
            path: path.to_string(),
            bearer: header("authorization")
                .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string)),
            account_id: header("chatgpt-account-id"),
            fedramp: header("x-openai-fedramp"),
            user_agent: header("user-agent"),
            accept: header("accept"),
            beta: header("openai-beta"),
            originator: header("originator"),
            body,
        });
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    fn trail(&self) -> Vec<(String, Option<String>)> {
        self.requests()
            .into_iter()
            .map(|r| (r.path, r.bearer))
            .collect()
    }
}

fn usage_body() -> Value {
    json!({
        "plan_type": "pro",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {"used_percent": 42.0, "limit_window_seconds": 18000, "reset_at": 1783843614},
            "secondary_window": {"used_percent": 84.0, "limit_window_seconds": 604800, "reset_at": 1784430414}
        },
        "credits": {"has_credits": true, "unlimited": false, "balance": "12.5"},
        "additional_rate_limits": [{
            "limit_name": "GPT-5.3-Codex-Spark",
            "rate_limit": {
                "primary_window": {"used_percent": 3, "limit_window_seconds": 18000},
                "secondary_window": {"used_percent": 9, "limit_window_seconds": 604800, "reset_at": 1784430414}
            }
        }]
    })
}

async fn usage(State(mock): State<Arc<Mock>>, headers: HeaderMap) -> Response {
    mock.record("/usage", &headers, Value::Null);
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if headers
        .get("chatgpt-account-id")
        .and_then(|v| v.to_str().ok())
        != Some(ACCOUNT_ID)
    {
        return (StatusCode::BAD_REQUEST, "missing ChatGPT-Account-ID").into_response();
    }
    match bearer {
        "at-good" => Json(usage_body()).into_response(),
        bearer if bearer.starts_with("at-reset-") => {
            let mut body = usage_body();
            body["rate_limit_reset_credits"] = match bearer {
                "at-reset-zero" => json!({"available_count": 0}),
                "at-reset-inline" => json!({"available_count": 2, "credits": [
                    {"id": "inline", "expires_at": "2026-10-22T16:00:00Z"}
                ]}),
                _ => json!({"available_count": 2, "applicable_available_count": 2}),
            };
            Json(body).into_response()
        }
        "at-limited" => (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", "7")],
            Json(json!({"detail": {"type": "rate_limit"}})),
        )
            .into_response(),
        "at-limited-body-hint" => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": {"message": "Rate limited. Please try again in 12 seconds."}})),
        )
            .into_response(),
        "at-broken" => (StatusCode::OK, "not json").into_response(),
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"detail": "Unauthorized"})),
        )
            .into_response(),
    }
}

async fn reset_credits(State(mock): State<Arc<Mock>>, headers: HeaderMap) -> Response {
    mock.record("/rate-limit-reset-credits", &headers, Value::Null);
    match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some("Bearer at-reset-denied") => StatusCode::UNAUTHORIZED.into_response(),
        Some("Bearer at-reset-forbidden") => StatusCode::FORBIDDEN.into_response(),
        Some("Bearer at-reset-throttled") => {
            (StatusCode::TOO_MANY_REQUESTS, [("Retry-After", "300")]).into_response()
        }
        Some("Bearer at-reset-unavailable") => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Some("Bearer at-reset-invalid-json") => "not json".into_response(),
        Some("Bearer at-reset-invalid-body") => Json(json!({"credits": null})).into_response(),
        _ => Json(json!({"available_count": 3, "credits": [
            {"id": "later", "reset_type": "codex_rate_limits", "status": "available",
             "expires_at": "2026-10-25T16:00:00Z"},
            {"id": "first", "reset_type": "codex_rate_limits", "status": "available",
             "expires_at": "2026-10-22T16:00:00.123456Z"},
            {"id": "used", "reset_type": "codex_rate_limits", "status": "consumed",
             "expires_at": "2026-10-20T16:00:00Z"}
        ]}))
        .into_response(),
    }
}

async fn token(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    mock.record("/token", &headers, body.clone());
    if body["grant_type"] != "refresh_token" || body["client_id"] != "app_EMoamEEZ73f0CkXaXp7hrann"
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_request"})),
        )
            .into_response();
    }
    match body["refresh_token"].as_str().unwrap_or("") {
        "rt-live" => Json(json!({
            "id_token": id_token(now_unix() + 86_400),
            "access_token": "at-good",
            "refresh_token": "rt-next"
        }))
        .into_response(),
        "rt-reset-live" | "rt-reset-denied" => Json(json!({
            "id_token": id_token(now_unix() + 86_400),
            "access_token": if body["refresh_token"] == "rt-reset-live" {
                "at-reset-live"
            } else {
                "at-reset-denied"
            },
            "refresh_token": "rt-next"
        }))
        .into_response(),
        "rt-rotate-only" => Json(json!({"refresh_token": "rt-next"})).into_response(),
        "rt-dead" => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {
                "code": "refresh_token_reused",
                "message": "This refresh token has already been used.",
                "param": null,
                "type": "invalid_request_error"
            }})),
        )
            .into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
    }
}

async fn start(mock: Arc<Mock>) -> SocketAddr {
    let app = Router::new()
        .route("/usage", get(usage))
        .route("/rate-limit-reset-credits", get(reset_credits))
        .route("/token", post(token))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn id_token(exp: i64) -> String {
    let claims = json!({
        "email": "User@Example.com",
        "exp": exp,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": ACCOUNT_ID,
            "chatgpt_plan_type": "pro",
            "chatgpt_account_is_fedramp": true
        }
    });
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    format!("header.{payload}.sig")
}

fn chatgpt_auth(access_token: Option<&str>, refresh_token: Option<&str>, id_exp: i64) -> AuthJson {
    let mut tokens = json!({"id_token": id_token(id_exp), "account_id": ACCOUNT_ID});
    if let Some(at) = access_token {
        tokens["access_token"] = json!(at);
    }
    if let Some(rt) = refresh_token {
        tokens["refresh_token"] = json!(rt);
    }
    AuthJson::from_value(json!({
        "OPENAI_API_KEY": null,
        "auth_mode": "chatgpt",
        "tokens": tokens,
        "last_refresh": "2026-09-29T10:00:00Z"
    }))
}

/// One scenario: fresh server, endpoint overrides pointed at it, one fetch.
async fn run(auth: &AuthJson) -> (FetchOutcome, Arc<Mock>) {
    let _serialized = ENV.lock().await;
    let mock = Arc::new(Mock::default());
    let addr = start(mock.clone()).await;
    // SAFETY: the ENV mutex serializes every scenario, and no other thread in
    // this test binary reads or writes the environment concurrently.
    unsafe {
        std::env::set_var("CCSW_USAGE_URL", format!("http://{addr}/usage"));
        std::env::set_var("CCSW_TOKEN_URL", format!("http://{addr}/token"));
    }
    let client = build_client(None).unwrap();
    let outcome = fetch_usage(&client, auth).await;
    (outcome, mock)
}

const FAR: i64 = 4_102_444_800; // 2100-01-01

#[tokio::test]
async fn usage_200_parses_and_sends_the_routing_headers() {
    let auth = chatgpt_auth(Some("at-good"), Some("rt-live"), FAR);
    let (outcome, mock) = run(&auth).await;
    assert_eq!(outcome.refreshed, None);
    let usage = outcome.result.expect("usage parsed");
    assert_eq!(usage.five_hour.as_ref().unwrap().pct, 42.0);
    assert_eq!(usage.seven_day.as_ref().unwrap().pct, 84.0);
    assert_eq!(usage.plan_type.as_deref(), Some("pro"));
    assert_eq!(usage.credits.as_ref().unwrap().balance, Some(12.5));
    assert_eq!(usage.scoped.len(), 1);
    assert_eq!(usage.scoped[0].name, "GPT-5.3-Codex-Spark");
    assert_eq!(usage.scoped[0].pct, 9.0);
    assert!(!usage.limited);

    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/usage");
    assert_eq!(requests[0].bearer.as_deref(), Some("at-good"));
    assert_eq!(requests[0].account_id.as_deref(), Some(ACCOUNT_ID));
    assert_eq!(requests[0].fedramp.as_deref(), Some("true"));
    let ua = requests[0].user_agent.clone().unwrap();
    assert!(ua.starts_with("codex_cli_rs/0.144.1 ("), "{ua}");
}

#[tokio::test]
async fn reset_credit_expiry_is_fetched_when_usage_only_has_the_count() {
    let auth = chatgpt_auth(Some("at-reset-live"), Some("rt-live"), FAR);
    let (outcome, mock) = run(&auth).await;
    assert_eq!(outcome.refreshed, None);
    let usage = outcome.result.unwrap();
    assert_eq!(
        usage.reset_credits,
        Some(2),
        "keep the usage endpoint's count"
    );
    assert_eq!(
        usage.reset_credits_end_at.as_deref(),
        Some("2026-10-22T16:00:00Z")
    );
    assert_eq!(
        ccsw::tui::data::reset_cards_text(
            usage.reset_credits,
            usage
                .reset_credits_end_at
                .as_deref()
                .and_then(ccsw::usage_math::parse_reset),
            ccsw::usage_math::parse_reset("2026-10-12T16:00:00Z").unwrap(),
        )
        .as_deref(),
        Some("♥ 2 (in 10d)")
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let list = &requests[1];
    assert_eq!(list.path, "/rate-limit-reset-credits");
    assert_eq!(list.bearer.as_deref(), Some("at-reset-live"));
    assert_eq!(list.account_id.as_deref(), Some(ACCOUNT_ID));
    assert_eq!(list.fedramp.as_deref(), Some("true"));
    assert_eq!(list.accept.as_deref(), Some("application/json"));
    assert_eq!(list.beta.as_deref(), Some("codex-1"));
    assert_eq!(list.originator.as_deref(), Some("Codex Desktop"));
    assert_eq!(list.user_agent, requests[0].user_agent);
}

#[tokio::test]
async fn reset_credit_list_is_skipped_when_no_expiry_is_needed() {
    for (bearer, count, expiry) in [
        ("at-good", None, None),
        ("at-reset-zero", Some(0), None),
        ("at-reset-inline", Some(2), Some("2026-10-22T16:00:00Z")),
    ] {
        let (outcome, mock) = run(&chatgpt_auth(Some(bearer), None, FAR)).await;
        let usage = outcome.result.unwrap();
        assert_eq!(usage.reset_credits, count);
        assert_eq!(usage.reset_credits_end_at.as_deref(), expiry);
        assert_eq!(mock.requests().len(), 1, "{bearer}");
    }
}

#[tokio::test]
async fn reset_credit_list_failures_keep_successful_usage_without_refresh_or_retry() {
    for bearer in [
        "at-reset-denied",
        "at-reset-forbidden",
        "at-reset-throttled",
        "at-reset-unavailable",
        "at-reset-invalid-json",
        "at-reset-invalid-body",
    ] {
        let auth = chatgpt_auth(Some(bearer), Some("rt-live"), FAR);
        let (outcome, mock) = run(&auth).await;
        assert_eq!(outcome.refreshed, None);
        let usage = outcome.result.unwrap();
        assert_eq!(usage.five_hour.unwrap().pct, 42.0);
        assert_eq!(usage.reset_credits, Some(2));
        assert_eq!(usage.reset_credits_end_at, None);
        assert_eq!(mock.requests().len(), 2, "{bearer}");
    }
}

#[tokio::test]
async fn reset_credit_list_uses_the_rotated_token_and_keeps_the_rotation_on_failure() {
    for (refresh, bearer, expiry) in [
        (
            "rt-reset-live",
            "at-reset-live",
            Some("2026-10-22T16:00:00Z"),
        ),
        ("rt-reset-denied", "at-reset-denied", None),
    ] {
        let auth = chatgpt_auth(Some("at-stale"), Some(refresh), FAR);
        let (outcome, mock) = run(&auth).await;
        let rotated = outcome.refreshed.unwrap();
        assert_eq!(rotated.access_token, bearer);
        assert_eq!(rotated.refresh_token, "rt-next");
        assert_eq!(
            outcome.result.unwrap().reset_credits_end_at.as_deref(),
            expiry
        );
        assert_eq!(
            mock.trail(),
            vec![
                ("/usage".into(), Some("at-stale".into())),
                ("/token".into(), None),
                ("/usage".into(), Some(bearer.into())),
                ("/rate-limit-reset-credits".into(), Some(bearer.into())),
            ]
        );
    }
}

#[tokio::test]
async fn unauthorized_then_refresh_then_ok_reports_the_rotation() {
    let auth = chatgpt_auth(Some("at-stale"), Some("rt-live"), FAR);
    let (outcome, mock) = run(&auth).await;
    let refreshed = outcome.refreshed.expect("rotation captured");
    assert_eq!(refreshed.access_token, "at-good");
    assert_eq!(refreshed.refresh_token, "rt-next");
    assert!(refreshed.id_token.starts_with("header."));
    assert_eq!(
        outcome
            .result
            .expect("usage after refresh")
            .seven_day
            .unwrap()
            .pct,
        84.0
    );
    assert_eq!(
        mock.trail(),
        vec![
            ("/usage".to_string(), Some("at-stale".to_string())),
            ("/token".to_string(), None),
            ("/usage".to_string(), Some("at-good".to_string())),
        ]
    );
    let token_request = &mock.requests()[1];
    assert_eq!(token_request.body["refresh_token"], "rt-live");
    assert_eq!(token_request.body["grant_type"], "refresh_token");
}

#[tokio::test]
async fn terminal_refresh_verdict_is_reported_once_and_remembered() {
    let auth = chatgpt_auth(Some("at-dead"), Some("rt-dead"), FAR);
    let (outcome, mock) = run(&auth).await;
    assert_eq!(outcome.refreshed, None);
    assert_eq!(
        outcome.result,
        Err(FetchError::Auth(RefreshError::Terminal {
            code: "refresh_token_reused".to_string(),
            message: Some("This refresh token has already been used.".to_string()),
            memorable: true,
        }))
    );
    assert_eq!(
        mock.trail(),
        vec![
            ("/usage".to_string(), Some("at-dead".to_string())),
            ("/token".to_string(), None),
        ],
        "no second usage call after a terminal verdict"
    );
}

#[tokio::test]
async fn rate_limited_surfaces_retry_after_from_header_or_body() {
    let auth = chatgpt_auth(Some("at-limited"), Some("rt-live"), FAR);
    let (outcome, mock) = run(&auth).await;
    assert_eq!(outcome.refreshed, None);
    assert_eq!(
        outcome.result,
        Err(FetchError::Http {
            status: 429,
            retry_after: Some(7.0)
        })
    );
    assert_eq!(mock.requests().len(), 1, "a 429 is never retried in-call");

    let auth = chatgpt_auth(Some("at-limited-body-hint"), None, FAR);
    let (outcome, _) = run(&auth).await;
    assert_eq!(
        outcome.result,
        Err(FetchError::Http {
            status: 429,
            retry_after: Some(12.0)
        })
    );
}

#[tokio::test]
async fn expiring_token_is_refreshed_before_the_usage_call() {
    let auth = chatgpt_auth(Some("at-stale"), Some("rt-live"), now_unix() + 60);
    let (outcome, mock) = run(&auth).await;
    assert_eq!(
        outcome.refreshed.as_ref().map(|t| t.refresh_token.as_str()),
        Some("rt-next")
    );
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(
        mock.trail(),
        vec![
            ("/token".to_string(), None),
            ("/usage".to_string(), Some("at-good".to_string())),
        ]
    );

    // A rotation that omits id/access tokens keeps the presented ones.
    let auth = chatgpt_auth(Some("at-good"), Some("rt-rotate-only"), now_unix() + 60);
    let (outcome, mock) = run(&auth).await;
    let refreshed = outcome.refreshed.expect("rotation captured");
    assert_eq!(refreshed.access_token, "at-good");
    assert_eq!(refreshed.refresh_token, "rt-next");
    assert_eq!(refreshed.id_token, auth.id_token().unwrap());
    assert!(outcome.result.is_ok());
    assert_eq!(mock.trail()[1].1.as_deref(), Some("at-good"));
}

#[tokio::test]
async fn refresh_token_only_credential_refreshes_first() {
    let auth = chatgpt_auth(None, Some("rt-live"), FAR);
    let (outcome, mock) = run(&auth).await;
    assert!(outcome.refreshed.is_some());
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(mock.trail()[0].0, "/token");
}

#[tokio::test]
async fn failures_without_a_refresh_path() {
    let auth = chatgpt_auth(Some("at-stale"), None, FAR);
    let (outcome, mock) = run(&auth).await;
    assert_eq!(
        outcome.result,
        Err(FetchError::Http {
            status: 401,
            retry_after: None
        })
    );
    assert_eq!(mock.requests().len(), 1);

    let auth = chatgpt_auth(Some("at-broken"), None, FAR);
    let (outcome, _) = run(&auth).await;
    assert!(
        matches!(outcome.result, Err(FetchError::BadResponse(_))),
        "{:?}",
        outcome.result
    );

    let api_key = AuthJson::api_key_auth("sk-test");
    let (outcome, mock) = run(&api_key).await;
    assert_eq!(outcome.result, Err(FetchError::NoAccessToken));
    assert!(mock.requests().is_empty());

    // Transient refresh failures (5xx) are reported as such and nothing rotates.
    let auth = chatgpt_auth(Some("at-stale"), Some("rt-unknown"), FAR);
    let (outcome, _) = run(&auth).await;
    assert_eq!(outcome.refreshed, None);
    assert!(
        matches!(
            outcome.result,
            Err(FetchError::Auth(RefreshError::Transient(_)))
        ),
        "{:?}",
        outcome.result
    );
}
