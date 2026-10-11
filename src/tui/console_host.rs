//! The TUI-owned listener. Stopping it also closes streams and joins its worker.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use tokio::sync::mpsc;

use super::console::{Access, Event, Options, PairingLink};
use super::worker::now_s;
use crate::paths::Paths;
use crate::web::{Server, config, listener};

enum Control {
    NewLink { open_browser: bool },
    Stop,
}

pub(super) struct Handle {
    control: mpsc::Sender<Control>,
    stopped: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Handle {
    pub fn start(
        paths: Paths,
        options: Options,
        open_browser: bool,
        on_event: impl Fn(Event) + Send + 'static,
    ) -> Self {
        let (control, receiver) = mpsc::channel(4);
        let stopped = Arc::new(AtomicBool::new(false));
        let stopping = stopped.clone();
        let thread = thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()?
                    .block_on(run(
                        paths,
                        options,
                        open_browser,
                        receiver,
                        &stopping,
                        &on_event,
                    ))
            })();
            on_event(Event::Stopped(result.map_err(|error| error.to_string())));
        });
        Self {
            control,
            stopped,
            thread: Some(thread),
        }
    }

    pub fn new_link(&self, open_browser: bool) {
        let _ = self.control.try_send(Control::NewLink { open_browser });
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.control.try_send(Control::Stop);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn run(
    paths: Paths,
    options: Options,
    open_browser: bool,
    mut control: mpsc::Receiver<Control>,
    stopped: &AtomicBool,
    on_event: &impl Fn(Event),
) -> anyhow::Result<()> {
    let startup = async {
        let ip = match options.access {
            Access::Local => IpAddr::V4(Ipv4Addr::LOCALHOST),
            Access::Tailscale => {
                let addresses = config::tailscale_addresses().await?;
                *addresses
                    .iter()
                    .find(|ip| ip.is_ipv4())
                    .unwrap_or(&addresses[0])
            }
        };
        listener::bind(paths, SocketAddr::new(ip, 0), None, options.read_only).await
    };
    let (server, socket) = tokio::select! {
        biased;
        _ = control.recv() => return Ok(()),
        result = startup => result?,
    };
    let result = listener::serve(&server, socket, async {
        if !stopped.load(Ordering::SeqCst) {
            issue_link(&server, open_browser, on_event).await;
        }
        while !stopped.load(Ordering::SeqCst) {
            match control.recv().await {
                Some(Control::NewLink { open_browser }) if !stopped.load(Ordering::SeqCst) => {
                    issue_link(&server, open_browser, on_event).await;
                }
                _ => break,
            }
        }
    })
    .await;
    server.stop();
    result
}

async fn issue_link(server: &Server, open_browser: bool, on_event: &impl Fn(Event)) {
    let link = PairingLink {
        origin: server.origin().to_string(),
        code: server.pairing_code(),
        expires_at: now_s() + crate::web::PAIRING_LIFETIME.as_secs_f64(),
    };
    let url = link.url();
    on_event(Event::Link(link));
    if open_browser {
        let opened = tokio::task::spawn_blocking(move || {
            // Browser launchers can log their command line, which includes the pairing code.
            tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
                webbrowser::open(&url).is_ok()
            })
        })
        .await
        .unwrap_or(false);
        if !opened {
            on_event(Event::BrowserFailed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[tokio::test]
    async fn hosted_listener_rotates_pairs_enforces_read_only_and_stops() {
        let temp = tempfile::tempdir().unwrap();
        let mut paths = Paths::from_values(None, None, None, temp.path()).unwrap();
        paths.keychain_enabled = false;
        let (tx, rx) = channel();
        let host = Handle::start(
            paths,
            Options {
                read_only: true,
                ..Options::default()
            },
            false,
            move |event| {
                tx.send(event).unwrap();
            },
        );
        let Event::Link(first) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else {
            panic!("no pairing link")
        };
        host.new_link(false);
        let Event::Link(link) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else {
            panic!("no replacement link")
        };
        assert_ne!(first.code, link.code);
        let client = reqwest::Client::new();
        let pair = |code: String| {
            client
                .post(format!("{}/api/v1/session", link.origin))
                .header("Origin", &link.origin)
                .json(&serde_json::json!({"code": code}))
                .send()
        };
        assert_eq!(
            pair(first.code).await.unwrap().status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        let response = pair(link.code.clone()).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let session: serde_json::Value = response.json().await.unwrap();
        assert_eq!(session["readOnly"], true);
        let response = client
            .post(format!("{}/api/v1/refresh", link.origin))
            .header("Origin", &link.origin)
            .header("Cookie", &cookie)
            .header("X-CCSW-CSRF", session["csrfToken"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        // An open event stream must not keep shutdown pending.
        let stream = client
            .get(format!("{}/api/v1/events", link.origin))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        host.stop();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Stopped(Ok(()))
        ));
        drop(host);
        assert!(client.get(&link.origin).send().await.is_err());
        drop(stream);
    }
}
