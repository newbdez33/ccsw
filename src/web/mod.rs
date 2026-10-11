//! An opt-in remote console. Provider work stays on one bounded worker queue.

mod api;
mod assets;
mod auth;
pub mod config;
pub mod listener;
mod worker;

pub(crate) use auth::PAIRING_LIFETIME;

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::Instant;

use axum::Json;
use axum::extract::{DefaultBodyLimit, Extension, Path, Request, State, rejection::JsonRejection};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response, Sse, sse::Event};
use axum::routing::{get, post};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::sync::{Semaphore, broadcast};

use api::{ApiError, Operation, Snapshot, SwitchRequest};
use auth::{Auth, Session};
use config::Config;
use worker::{Command, EventRecord};

type Shared = Arc<App>;
type ApiResult<T> = Result<T, ApiError>;

struct StoredOperation {
    request: SwitchRequest,
    operation: Operation,
    expires: Instant,
}

struct App {
    config: Config,
    auth: Mutex<Auth>,
    snapshot: RwLock<Option<Snapshot>>,
    operations: Mutex<HashMap<(String, String), StoredOperation>>,
    activity: Mutex<Vec<EventRecord>>,
    changes: broadcast::Sender<()>,
    commands: mpsc::SyncSender<Command>,
    refreshing: AtomicBool,
    stopped: AtomicBool,
    streams: Arc<Semaphore>,
}

pub struct Server {
    app: Shared,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn origin(&self) -> &str {
        &self.app.config.origin
    }

    pub fn new(paths: crate::paths::Paths, config: Config) -> Self {
        let (commands, receiver) = mpsc::sync_channel(16);
        let (changes, _) = broadcast::channel(32);
        let app = Arc::new(App {
            config,
            auth: Mutex::new(Auth::new()),
            snapshot: RwLock::new(None),
            operations: Mutex::new(HashMap::new()),
            activity: Mutex::new(Vec::new()),
            changes,
            commands,
            refreshing: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            streams: Arc::new(Semaphore::new(64)),
        });
        let worker_app = app.clone();
        let worker = std::thread::spawn(move || worker::run(worker_app, paths, receiver));
        Self {
            app,
            worker: Some(worker),
        }
    }

    pub fn pairing_code(&self) -> String {
        self.app.auth.lock().expect("auth lock").rotate_pairing()
    }

    pub fn read_pairing_input(&self) {
        let app = Arc::downgrade(&self.app);
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                if line.is_err() {
                    break;
                }
                let Some(app) = app.upgrade() else {
                    break;
                };
                if app.stopped.load(Ordering::Relaxed) {
                    break;
                }
                let code = app.auth.lock().expect("auth lock").rotate_pairing();
                println!("Pairing code: {code}");
            }
        });
    }

    pub fn router(&self) -> axum::Router {
        axum::Router::new()
            .route(
                "/api/v1/session",
                get(session_info).post(pair).delete(logout),
            )
            .route("/api/v1/snapshot", get(snapshot))
            .route("/api/v1/events", get(events))
            .route("/api/v1/refresh", post(refresh))
            .route("/api/v1/switches", post(switch))
            .route("/api/v1/operations/{id}", get(operation))
            .fallback(assets::serve)
            .layer(DefaultBodyLimit::max(8192))
            .layer(middleware::from_fn_with_state(self.app.clone(), guard))
            .with_state(self.app.clone())
    }

    pub fn shutdown_handle(&self) -> impl Fn() + Send + 'static {
        let app = self.app.clone();
        move || {
            app.stopped.store(true, Ordering::Relaxed);
            let _ = app.commands.try_send(Command::Stop);
            let _ = app.changes.send(());
        }
    }

    pub fn stop(mut self) {
        self.app.stopped.store(true, Ordering::Relaxed);
        let _ = self.app.commands.try_send(Command::Stop);
        let _ = self.app.changes.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.app.stopped.store(true, Ordering::Relaxed);
        let _ = self.app.commands.try_send(Command::Stop);
        let _ = self.app.changes.send(());
    }
}

fn error(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::new(status, code)
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    if headers.get_all(name).iter().count() != 1 {
        return None;
    }
    headers.get(name)?.to_str().ok()
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let mut tokens = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .filter(|(name, _)| *name == auth::COOKIE)
        .map(|(_, token)| token);
    let token = tokens.next()?;
    if tokens.next().is_some() || token.len() != 64 {
        return None;
    }
    Some(token.to_string())
}

fn authorize(app: &App, request: &mut Request) -> ApiResult<()> {
    let headers = request.headers();
    if header_text(headers, "host") != Some(app.config.authority.as_str()) {
        return Err(error(StatusCode::FORBIDDEN, "forbidden"));
    }
    if headers.contains_key(header::ORIGIN)
        && header_text(headers, "origin") != Some(app.config.origin.as_str())
    {
        return Err(error(StatusCode::FORBIDDEN, "forbidden"));
    }
    if header_text(headers, "sec-fetch-site") == Some("cross-site") {
        return Err(error(StatusCode::FORBIDDEN, "forbidden"));
    }
    let mutation = !matches!(*request.method(), Method::GET | Method::HEAD);
    if mutation && header_text(headers, "origin") != Some(app.config.origin.as_str()) {
        return Err(error(StatusCode::FORBIDDEN, "forbidden"));
    }
    let path = request.uri().path();
    if !path.starts_with("/api/")
        || (path == "/api/v1/session" && *request.method() == Method::POST)
    {
        return Ok(());
    }
    let session = cookie_token(headers)
        .and_then(|token| app.auth.lock().expect("auth lock").session(&token))
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    if mutation {
        let csrf = header_text(headers, "x-ccsw-csrf").unwrap_or_default();
        if !bool::from(csrf.as_bytes().ct_eq(session.csrf.as_bytes())) {
            return Err(error(StatusCode::FORBIDDEN, "forbidden"));
        }
        if app.config.read_only && path != "/api/v1/session" {
            return Err(error(StatusCode::FORBIDDEN, "read_only"));
        }
    }
    request.extensions_mut().insert(session);
    Ok(())
}

async fn guard(State(app): State<Shared>, mut request: Request, next: Next) -> Response {
    let mut response = match authorize(&app, &mut request) {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("content-security-policy", HeaderValue::from_static(
        "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    ));
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pairing {
    code: String,
}

fn session_payload(app: &App, session: &Session) -> Value {
    json!({"csrfToken": session.csrf, "expiresAt": session.expires_at,
        "readOnly": app.config.read_only, "host": app.config.host, "origin": app.config.origin})
}

async fn pair(
    State(app): State<Shared>,
    body: Result<Json<Pairing>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_request"))?;
    let (token, session) = app
        .auth
        .lock()
        .expect("auth lock")
        .pair(&body.code, Instant::now())
        .map_err(|code| {
            error(
                if code == "pairing_rate_limited" {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::UNAUTHORIZED
                },
                code,
            )
        })?;
    let mut response = Json(session_payload(&app, &session)).into_response();
    let secure = if app.config.secure { "; Secure" } else { "" };
    let cookie = format!(
        "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        auth::COOKIE,
        auth::SESSION_LIFETIME.as_secs(),
        secure
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("hex cookie"),
    );
    Ok(response)
}

async fn session_info(
    State(app): State<Shared>,
    Extension(session): Extension<Session>,
) -> Json<Value> {
    Json(session_payload(&app, &session))
}

async fn logout(State(app): State<Shared>, Extension(session): Extension<Session>) -> Response {
    app.auth.lock().expect("auth lock").revoke(&session);
    let _ = app.changes.send(());
    let mut response = StatusCode::NO_CONTENT.into_response();
    let secure = if app.config.secure { "; Secure" } else { "" };
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "ccsw_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}"
        ))
        .expect("static cookie"),
    );
    response
}

impl App {
    fn payload(&self, session: &Session) -> ApiResult<Value> {
        let mut snapshot = self
            .snapshot
            .read()
            .expect("snapshot lock")
            .clone()
            .ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"))?;
        snapshot.stale |= crate::model::now_unix() - snapshot.taken_at > 15;
        let operations: Vec<_> = self
            .operations
            .lock()
            .expect("operation lock")
            .iter()
            .filter(|((owner, _), _)| *owner == session.id)
            .map(|(_, entry)| entry.operation.clone())
            .collect();
        Ok(
            json!({"snapshot": snapshot, "activity": *self.activity.lock().expect("activity lock"),
            "operations": operations, "refreshing": self.refreshing.load(Ordering::Relaxed)}),
        )
    }
}

async fn snapshot(
    State(app): State<Shared>,
    Extension(session): Extension<Session>,
) -> ApiResult<Json<Value>> {
    Ok(Json(app.payload(&session)?))
}

async fn refresh(State(app): State<Shared>) -> ApiResult<StatusCode> {
    if !app.refreshing.swap(true, Ordering::Relaxed)
        && app.commands.try_send(Command::Refresh).is_err()
    {
        app.refreshing.store(false, Ordering::Relaxed);
        return Err(error(StatusCode::SERVICE_UNAVAILABLE, "host_busy"));
    }
    let _ = app.changes.send(());
    Ok(StatusCode::ACCEPTED)
}

async fn switch(
    State(app): State<Shared>,
    Extension(session): Extension<Session>,
    body: Result<Json<SwitchRequest>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Operation>)> {
    let Json(request) = body.map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_request"))?;
    if !request.valid() {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let key = (session.id.clone(), request.request_id.clone());
    let mut operations = app.operations.lock().expect("operation lock");
    operations.retain(|_, entry| entry.expires > Instant::now());
    if let Some(existing) = operations.get(&key) {
        if existing.request != request {
            return Err(error(StatusCode::CONFLICT, "request_id_reused"));
        }
        return Ok((StatusCode::ACCEPTED, Json(existing.operation.clone())));
    }
    if operations.len() >= 1024
        || operations
            .keys()
            .filter(|(owner, _)| *owner == session.id)
            .count()
            >= 128
    {
        return Err(error(StatusCode::TOO_MANY_REQUESTS, "operation_limit"));
    }
    {
        let snapshot = app.snapshot.read().expect("snapshot lock");
        let snapshot = snapshot
            .as_ref()
            .filter(|s| !s.stale && crate::model::now_unix() - s.taken_at <= 15)
            .ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"))?;
        if snapshot.revision != request.expected_revision {
            return Err(error(StatusCode::CONFLICT, "state_changed"));
        }
    }
    let operation = Operation {
        id: request.request_id.clone(),
        provider: request.provider,
        slot: request.slot,
        state: "pending",
        error: None,
        effects: None,
    };
    operations.insert(
        key.clone(),
        StoredOperation {
            request: request.clone(),
            operation: operation.clone(),
            expires: session.expires,
        },
    );
    if app
        .commands
        .try_send(Command::Switch { session, request })
        .is_err()
    {
        operations.remove(&key);
        return Err(error(StatusCode::SERVICE_UNAVAILABLE, "host_busy"));
    }
    Ok((StatusCode::ACCEPTED, Json(operation)))
}

async fn operation(
    State(app): State<Shared>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
) -> ApiResult<Json<Operation>> {
    app.operations
        .lock()
        .expect("operation lock")
        .get(&(session.id, id))
        .map(|stored| Json(stored.operation.clone()))
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "not_found"))
}

async fn events(
    State(app): State<Shared>,
    Extension(session): Extension<Session>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let permit = app
        .streams
        .clone()
        .try_acquire_owned()
        .map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "host_busy"))?;
    let mut changes = app.changes.subscribe();
    let stream = async_stream::stream! {
        let _permit = permit;
        loop {
            if app.stopped.load(Ordering::Relaxed) || !app.auth.lock().expect("auth lock").valid(&session) { break; }
            match app.payload(&session) {
                Ok(payload) => yield Ok(Event::default().event("snapshot").data(payload.to_string())),
                Err(_) => yield Ok(Event::default().event("unavailable").data("{}")),
            }
            tokio::select! {
                result = changes.recv() => {
                    if matches!(result, Err(broadcast::error::RecvError::Closed)) { break; }
                    // Lagged clients receive a full current snapshot on the next iteration.
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            }
        }
    };
    Ok(Sse::new(stream))
}
