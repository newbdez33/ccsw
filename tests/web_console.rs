mod support;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Stdio};
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use support::Cli;

struct Console {
    child: Child,
    origin: String,
    code: String,
    client: Client,
    output: BufReader<ChildStdout>,
}

impl Drop for Console {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Console {
    fn start(cli: &Cli, read_only: bool) -> Self {
        let mut command = cli.command();
        command.args(["serve", "--bind", "127.0.0.1:0"]);
        if read_only {
            command.arg("--read-only");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut origin = String::new();
        let mut code = String::new();
        for _ in 0..2 {
            let mut line = String::new();
            assert!(
                output.read_line(&mut line).unwrap() > 0,
                "server exited before startup"
            );
            if let Some(value) = line.strip_prefix("Console URL: ") {
                origin = value.trim().trim_end_matches('/').to_string();
            }
            if let Some(value) = line.strip_prefix("Pairing code: ") {
                code = value.trim().to_string();
            }
        }
        assert!(!origin.is_empty() && !code.is_empty());
        Self {
            child,
            origin,
            code,
            client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            output,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    fn rotate_pairing(&mut self) {
        self.child.stdin.as_mut().unwrap().write_all(b"\n").unwrap();
        loop {
            let mut line = String::new();
            assert!(self.output.read_line(&mut line).unwrap() > 0);
            if let Some(value) = line.strip_prefix("Pairing code: ") {
                self.code = value.trim().to_string();
                break;
            }
        }
    }

    async fn pair(&self) -> (String, String) {
        let response = self
            .client
            .post(self.url("/api/v1/session"))
            .header("Origin", &self.origin)
            .json(&json!({"code": self.code}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .to_string();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        assert!(!cookie.contains("Secure"));
        let body: Value = response.json().await.unwrap();
        (
            cookie.split(';').next().unwrap().to_string(),
            body["csrfToken"].as_str().unwrap().to_string(),
        )
    }

    async fn snapshot(&self, cookie: &str) -> Value {
        for _ in 0..40 {
            let response = self
                .client
                .get(self.url("/api/v1/snapshot"))
                .header("Cookie", cookie)
                .send()
                .await
                .unwrap();
            if response.status() == StatusCode::OK {
                return response.json().await.unwrap();
            }
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("snapshot did not become ready")
    }

    async fn operation(&self, cookie: &str, id: &str) -> Value {
        for _ in 0..80 {
            let response: Value = self
                .client
                .get(self.url(&format!("/api/v1/operations/{id}")))
                .header("Cookie", cookie)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if response["state"] != "pending" {
                return response;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("operation did not finish")
    }
}

#[tokio::test]
async fn pairing_origin_csrf_and_private_projection() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "alice", "secret-refresh-token");
    let server = Console::start(&cli, false);
    let response = server
        .client
        .get(server.url("/api/v1/snapshot"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    for origin in [None, Some("https://outside.example")] {
        let mut request = server
            .client
            .post(server.url("/api/v1/session"))
            .json(&json!({"code": server.code}));
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        server
            .client
            .get(server.url("/"))
            .header("Host", "outside.example")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let (cookie, csrf) = server.pair().await;
    assert_eq!(
        server
            .client
            .post(server.url("/api/v1/session"))
            .header("Origin", &server.origin)
            .json(&json!({"code": server.code}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let snapshot = server.snapshot(&cookie).await;
    let text = snapshot.to_string();
    assert!(text.contains("alice@example.com"));
    for secret in [
        "secret-refresh-token",
        "access_token",
        "id_token",
        "struck_fingerprint",
        &cli.codex_home.display().to_string(),
    ] {
        assert!(!text.contains(secret), "private data: {secret}");
    }
    for token in [None, Some("wrong")] {
        let mut request = server
            .client
            .post(server.url("/api/v1/refresh"))
            .header("Origin", &server.origin)
            .header("Cookie", &cookie);
        if let Some(token) = token {
            request = request.header("X-CCSW-CSRF", token);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        server
            .client
            .post(server.url("/api/v1/refresh"))
            .header("Origin", &server.origin)
            .header("Cookie", &cookie)
            .header("X-CCSW-CSRF", &csrf)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        server
            .client
            .get(server.url("/auth.json"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let stream = server
        .client
        .get(server.url("/api/v1/events"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert!(
        stream.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("text/event-stream")
    );
    drop(stream);
    assert_eq!(
        server
            .client
            .delete(server.url("/api/v1/session"))
            .header("Origin", &server.origin)
            .header("Cookie", &cookie)
            .header("X-CCSW-CSRF", &csrf)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        server
            .client
            .get(server.url("/api/v1/snapshot"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn read_only_scope_cannot_be_overridden_by_a_browser() {
    let cli = Cli::new();
    let server = Console::start(&cli, true);
    let (cookie, csrf) = server.pair().await;
    for route in ["/api/v1/refresh", "/api/v1/switches"] {
        let response = server
            .client
            .post(server.url(route))
            .header("Cookie", &cookie)
            .header("Origin", &server.origin)
            .header("X-CCSW-CSRF", &csrf)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "read_only"
        );
    }
}

#[tokio::test]
async fn switch_requests_are_guarded_and_idempotent() {
    let mut cli = Cli::new();
    cli.daemon_running = true;
    cli.add_chatgpt("alice@example.com", "alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "bob", "rt-b");
    cli.clear_codex_calls();
    let mut server = Console::start(&cli, false);
    let (cookie, csrf) = server.pair().await;
    let snapshot = server.snapshot(&cookie).await;
    let request = json!({"slot":1,"provider":"codex","expectedRevision":snapshot["snapshot"]["revision"],
        "acknowledgeInterruption":true,"requestId":"unique-request-0001"});
    for _ in 0..2 {
        let response = server
            .client
            .post(server.url("/api/v1/switches"))
            .header("Cookie", &cookie)
            .header("Origin", &server.origin)
            .header("X-CCSW-CSRF", &csrf)
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    let operation = server.operation(&cookie, "unique-request-0001").await;
    assert_eq!(operation["state"], "succeeded", "{operation}");
    assert_eq!(operation["effects"]["daemon"], "restarted");
    assert_eq!(cli.live(), cli.credential(1));
    assert_eq!(
        cli.codex_calls()
            .iter()
            .filter(|call| call.contains("daemon restart"))
            .count(),
        1
    );
    let mut changed = request.clone();
    changed["slot"] = json!(2);
    assert_eq!(
        server
            .client
            .post(server.url("/api/v1/switches"))
            .header("Cookie", &cookie)
            .header("Origin", &server.origin)
            .header("X-CCSW-CSRF", &csrf)
            .json(&changed)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    changed["requestId"] = json!("unique-request-0002");
    assert_eq!(
        server
            .client
            .post(server.url("/api/v1/switches"))
            .header("Cookie", &cookie)
            .header("Origin", &server.origin)
            .header("X-CCSW-CSRF", &csrf)
            .json(&changed)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );

    let mut stream = server
        .client
        .get(server.url("/api/v1/events"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    let first = stream.chunk().await.unwrap().unwrap();
    let first = String::from_utf8(first.to_vec()).unwrap();
    assert!(first.contains("event: snapshot"));
    assert!(first.contains("\"active\":{\"claude\":null,\"codex\":1}"));
    drop(stream);

    server.rotate_pairing();
    let (other_cookie, _) = server.pair().await;
    assert_eq!(
        server
            .client
            .get(server.url("/api/v1/operations/unique-request-0001"))
            .header("Cookie", &other_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let other = server.snapshot(&other_cookie).await;
    assert_eq!(other["operations"], json!([]));
    assert_eq!(
        server
            .client
            .post(server.url("/api/v1/refresh"))
            .header("Cookie", &other_cookie)
            .header("Origin", &server.origin)
            .header("X-CCSW-CSRF", &csrf)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn queued_switch_rechecks_the_roster_after_taking_the_store_lock() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "bob", "rt-b");
    let server = Console::start(&cli, false);
    let (cookie, csrf) = server.pair().await;
    let snapshot = server.snapshot(&cookie).await;
    let paths = ccsw::paths::Paths::from_values(
        Some(cli.ccsw_home.clone()),
        Some(cli.codex_home.clone()),
        Some(cli.claude_home.clone()),
        cli.root.path(),
    )
    .unwrap();
    let store = ccsw::store::Store::open(paths);
    let lock = store.lock().unwrap();
    let response = server
        .client
        .post(server.url("/api/v1/switches"))
        .header("Cookie", &cookie)
        .header("Origin", &server.origin)
        .header("X-CCSW-CSRF", &csrf)
        .json(
            &json!({"slot":1,"provider":"codex","expectedRevision":snapshot["snapshot"]["revision"],
            "acknowledgeInterruption":true,"requestId":"queued-request-0001"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let mut roster = cli.roster();
    roster["accounts"]["1"]["disabled"] = json!(true);
    std::fs::write(cli.ccsw_home.join("sequence.json"), roster.to_string()).unwrap();
    drop(lock);
    let operation = server.operation(&cookie, "queued-request-0001").await;
    assert_eq!(operation["state"], "failed");
    assert_eq!(operation["error"]["code"], "state_changed");
    assert_eq!(cli.live(), cli.credential(2));
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_restart_failure_is_partial_and_does_not_expose_raw_errors() {
    let mut cli = Cli::new();
    cli.daemon_running = true;
    cli.add_chatgpt("alice@example.com", "alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "bob", "rt-b");
    let path = cli.bin_dir.join("codex");
    let script = std::fs::read_to_string(&path).unwrap().replace(
        "echo '{\"status\":\"restarted\"}'; exit 0",
        "echo 'private-daemon-token /private/host/path' >&2; exit 1",
    );
    std::fs::write(path, script).unwrap();
    let server = Console::start(&cli, false);
    let (cookie, csrf) = server.pair().await;
    let snapshot = server.snapshot(&cookie).await;
    let response = server
        .client
        .post(server.url("/api/v1/switches"))
        .header("Cookie", &cookie)
        .header("Origin", &server.origin)
        .header("X-CCSW-CSRF", &csrf)
        .json(
            &json!({"slot":1,"provider":"codex","expectedRevision":snapshot["snapshot"]["revision"],
            "acknowledgeInterruption":true,"requestId":"partial-request-0001"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let operation = server.operation(&cookie, "partial-request-0001").await;
    assert_eq!(operation["state"], "partial");
    assert_eq!(operation["effects"]["credentialsChanged"], true);
    assert_eq!(operation["effects"]["daemon"], "failed");
    assert!(!operation.to_string().contains("private-daemon-token"));
    assert!(!operation.to_string().contains("/private/host/path"));
    assert_eq!(cli.live(), cli.credential(1));
    assert_eq!(
        server.snapshot(&cookie).await["snapshot"]["active"]["codex"],
        1
    );
}

#[tokio::test]
async fn an_external_switch_to_the_target_is_a_conflict_not_a_partial_write() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "bob", "rt-b");
    let server = Console::start(&cli, false);
    let (cookie, csrf) = server.pair().await;
    let snapshot = server.snapshot(&cookie).await;
    let paths = ccsw::paths::Paths::from_values(
        Some(cli.ccsw_home.clone()),
        Some(cli.codex_home.clone()),
        Some(cli.claude_home.clone()),
        cli.root.path(),
    )
    .unwrap();
    let store = ccsw::store::Store::open(paths);
    let lock = store.lock().unwrap();
    let response = server
        .client
        .post(server.url("/api/v1/switches"))
        .header("Cookie", &cookie)
        .header("Origin", &server.origin)
        .header("X-CCSW-CSRF", &csrf)
        .json(
            &json!({"slot":1,"provider":"codex","expectedRevision":snapshot["snapshot"]["revision"],
            "acknowledgeInterruption":true,"requestId":"external-request-0001"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    cli.write_live(&cli.credential(1));
    drop(lock);
    let operation = server.operation(&cookie, "external-request-0001").await;
    assert_eq!(operation["state"], "failed");
    assert_eq!(operation["error"]["code"], "state_changed");
    assert_eq!(
        server.snapshot(&cookie).await["snapshot"]["active"]["codex"],
        1
    );
    assert!(cli.live_backups().is_empty());
}

#[test]
fn serve_help_and_wildcard_refusal() {
    let cli = Cli::new();
    assert_eq!(cli.run(&["serve", "--help"]).status, 0);
    assert_eq!(cli.run(&["serve", "--bind", "0.0.0.0:3000"]).status, 1);
}

#[tokio::test]
async fn browsers_share_one_refresh_and_one_provider_request() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let mock = axum::Router::new().route(
        "/usage",
        axum::routing::get(move || {
            let calls = handler_calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(300)).await;
                axum::Json(json!({"rate_limit": {"primary_window": {
                    "used_percent": 35, "limit_window_seconds": 18000
                }}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/usage", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    let mut cli = Cli::new();
    cli.usage_url = Some(url);
    cli.add_chatgpt("alice@example.com", "alice", "rt-a");
    let mut server = Console::start(&cli, false);
    let (cookie, csrf) = server.pair().await;
    server.snapshot(&cookie).await;
    server.rotate_pairing();
    let (other_cookie, other_csrf) = server.pair().await;
    for (cookie, csrf) in [
        (&cookie, &csrf),
        (&other_cookie, &other_csrf),
        (&cookie, &csrf),
    ] {
        assert_eq!(
            server
                .client
                .post(server.url("/api/v1/refresh"))
                .header("Cookie", cookie)
                .header("Origin", &server.origin)
                .header("X-CCSW-CSRF", csrf)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
    }
    let mut completed = false;
    for _ in 0..50 {
        let snapshot = server.snapshot(&cookie).await;
        if snapshot["refreshing"] == false && calls.load(Ordering::SeqCst) > 0 {
            assert_eq!(
                snapshot["snapshot"]["accounts"][0]["usage"]["fiveHour"]["pct"],
                35.0
            );
            let events = snapshot["activity"].as_array().unwrap();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event["title"] == "Usage refresh completed")
                    .count(),
                1
            );
            completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[cfg(unix)]
#[test]
fn a_tailnet_range_address_is_not_enough_to_allow_a_listener() {
    use std::os::unix::fs::PermissionsExt;
    let cli = Cli::new();
    let tailscale = cli.bin_dir.join("tailscale");
    std::fs::write(&tailscale,
        "#!/bin/sh\necho '{\"BackendState\":\"Running\",\"Self\":{\"TailscaleIPs\":[\"100.64.0.77\"]}}'\n"
    ).unwrap();
    std::fs::set_permissions(&tailscale, std::fs::Permissions::from_mode(0o755)).unwrap();
    let result = cli.run(&["serve", "--bind", "100.64.0.78:3000"]);
    assert_eq!(result.status, 1);
    assert!(
        result
            .stderr
            .contains("this host's current Tailscale address")
    );
    assert!(!result.stdout.contains("Pairing code"));
}
