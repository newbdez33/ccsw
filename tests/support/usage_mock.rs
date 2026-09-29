//! An axum mock of the Codex usage and token endpoints for end-to-end runs of
//! the binary. Usage replies are scripted per bearer token, so every account
//! in a scenario carries a distinct opaque access token that names its reply;
//! the token endpoint mints a rotation for any refresh token spelled
//! `rt-live|<email>|<account id>`.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use cswitch::model::now_unix;

/// 5h 35 %, 7d 60 %, a credit balance.
pub const OK: &str = "at-ok-35";
/// 5h at 100 % (resets in 90 min), 7d 40 %. The bodies never set
/// `limit_reached`: that flag marks every window as binding.
pub const LIMIT_5H: &str = "at-limit-5h";
/// 5h 20 %, 7d at 100 % (resets tomorrow).
pub const LIMIT_7D: &str = "at-limit-7d";
/// 5h 10 %, 7d 20 %, plus a model pool whose weekly window is at 100 %.
pub const POOL: &str = "at-pool";
/// 5h 95 %, 7d 30 %: over the default auto-switch threshold.
pub const HOT: &str = "at-hot-95";
/// 5h 62 %, 7d 30 %: under the default threshold, over 50 %.
pub const WARM: &str = "at-warm-62";
/// 5h 20 %, 7d 10 %.
pub const COOL: &str = "at-cool-20";
/// HTTP 429 with `Retry-After: 120`.
pub const THROTTLED: &str = "at-429";
/// HTTP 401: the bearer is dead, a refresh is needed.
pub const STALE: &str = "at-stale";
/// The bearer the token endpoint issues: 5h 50 %, 7d 50 %.
pub const REFRESHED: &str = "at-refreshed";
/// The refresh token the token endpoint issues.
pub const ROTATED_REFRESH: &str = "rt-next";

pub const POOL_NAME: &str = "GPT-5.3-Codex-Spark";

/// A refresh token the mock accepts; the rotation keeps this identity.
pub fn live_refresh_token(email: &str, account_id: &str) -> String {
    format!("rt-live|{email}|{account_id}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub path: String,
    pub bearer: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Default)]
struct Log {
    requests: Mutex<Vec<Recorded>>,
}

pub struct UsageMock {
    _runtime: tokio::runtime::Runtime,
    log: Arc<Log>,
    pub usage_url: String,
    pub token_url: String,
}

impl UsageMock {
    pub fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let log = Arc::new(Log::default());
        let app = Router::new()
            .route("/wham/usage", get(usage))
            .route("/oauth/token", post(token))
            .with_state(log.clone());
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        runtime.spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            _runtime: runtime,
            log,
            usage_url: format!("http://{addr}/wham/usage"),
            token_url: format!("http://{addr}/oauth/token"),
        }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.log.requests.lock().unwrap().clone()
    }

    /// `usage:<bearer>` and `token:<refresh token>` in request order.
    pub fn trail(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|r| match r.path.as_str() {
                "/wham/usage" => format!("usage:{}", r.bearer.unwrap_or_default()),
                _ => format!("token:{}", r.refresh_token.unwrap_or_default()),
            })
            .collect()
    }

    pub fn usage_calls(&self, bearer: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.path == "/wham/usage" && r.bearer.as_deref() == Some(bearer))
            .count()
    }
}

fn window(pct: f64, seconds: i64, reset_in: Option<i64>) -> Value {
    let mut w = json!({"used_percent": pct, "limit_window_seconds": seconds});
    if let Some(delta) = reset_in {
        w["reset_at"] = json!(now_unix() + delta);
    }
    w
}

fn body(five_hour: (f64, Option<i64>), seven_day: (f64, Option<i64>)) -> Value {
    json!({
        "plan_type": "plus",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": window(five_hour.0, 18_000, five_hour.1),
            "secondary_window": window(seven_day.0, 604_800, seven_day.1)
        }
    })
}

fn scripted(bearer: &str) -> Response {
    match bearer {
        OK => {
            let mut b = body((35.0, Some(5400)), (60.0, Some(3 * 86_400)));
            b["credits"] = json!({"has_credits": true, "unlimited": false, "balance": "12.5"});
            Json(b).into_response()
        }
        LIMIT_5H => Json(body((100.0, Some(5400)), (40.0, Some(3 * 86_400)))).into_response(),
        LIMIT_7D => Json(body((20.0, Some(5400)), (100.0, Some(86_400)))).into_response(),
        POOL => {
            let mut b = body((10.0, Some(5400)), (20.0, Some(3 * 86_400)));
            b["additional_rate_limits"] = json!([{
                "limit_name": POOL_NAME,
                "rate_limit": {
                    "primary_window": window(3.0, 18_000, Some(5400)),
                    "secondary_window": window(100.0, 604_800, Some(2 * 86_400))
                }
            }]);
            Json(b).into_response()
        }
        HOT => Json(body((95.0, Some(5400)), (30.0, Some(3 * 86_400)))).into_response(),
        WARM => Json(body((62.0, Some(5400)), (30.0, Some(3 * 86_400)))).into_response(),
        COOL => Json(body((20.0, Some(5400)), (10.0, Some(3 * 86_400)))).into_response(),
        REFRESHED => Json(body((50.0, Some(5400)), (50.0, Some(3 * 86_400)))).into_response(),
        THROTTLED => (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", "120")],
            Json(json!({"detail": {"type": "rate_limit"}})),
        )
            .into_response(),
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"detail": "Unauthorized"})),
        )
            .into_response(),
    }
}

async fn usage(State(log): State<Arc<Log>>, headers: HeaderMap) -> Response {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    log.requests.lock().unwrap().push(Recorded {
        path: "/wham/usage".into(),
        bearer: bearer.clone(),
        refresh_token: None,
    });
    if headers.get("chatgpt-account-id").is_none() {
        return (StatusCode::BAD_REQUEST, "missing ChatGPT-Account-ID").into_response();
    }
    scripted(bearer.as_deref().unwrap_or(""))
}

async fn token(State(log): State<Arc<Log>>, Json(body): Json<Value>) -> Response {
    let refresh = body["refresh_token"].as_str().unwrap_or("").to_string();
    log.requests.lock().unwrap().push(Recorded {
        path: "/oauth/token".into(),
        bearer: None,
        refresh_token: Some(refresh.clone()),
    });
    if body["grant_type"] != "refresh_token" || body["client_id"] != "app_EMoamEEZ73f0CkXaXp7hrann"
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_request"})),
        )
            .into_response();
    }
    let mut parts = refresh.split('|');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("rt-live"), Some(email), Some(account_id)) => Json(json!({
            "id_token": super::jwt(email, account_id),
            "access_token": REFRESHED,
            "refresh_token": ROTATED_REFRESH
        }))
        .into_response(),
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": "invalid_grant", "message": "unknown refresh token"}})),
        )
            .into_response(),
    }
}
