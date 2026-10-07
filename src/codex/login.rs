//! Browser login through Codex in a private, temporary home.
//!
//! Recent Codex versions revoke the previous login before authentication.
//! An empty home keeps that step away from managed credentials.

use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::errors::{CswitchError, Result};
use crate::fsutil;
use crate::paths::Paths;

use super::app_server::command_on_path;
use super::auth::AuthJson;

const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

struct LoginChild(Child);

impl Drop for LoginChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        // The npm launcher forwards SIGTERM to its native child. SIGKILL
        // would leave that child listening for the OAuth callback.
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline {
                if matches!(self.0.try_wait(), Ok(Some(_))) {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &self.0.id().to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `None` means the user cancelled. Codex opens the browser; `on_url` supplies
/// the manual fallback without forwarding subprocess output to the terminal.
pub fn browser_login(
    paths: &Paths,
    cancel: &AtomicBool,
    on_url: impl FnMut(String),
) -> Result<Option<AuthJson>> {
    let codex = command_on_path("codex").ok_or_else(|| {
        CswitchError::config("Codex CLI was not found on PATH. Install Codex to sign in.")
    })?;
    run_login(paths, &codex, cancel, on_url, LOGIN_TIMEOUT)
}

fn run_login(
    paths: &Paths,
    codex: &Path,
    cancel: &AtomicBool,
    mut on_url: impl FnMut(String),
    timeout: Duration,
) -> Result<Option<AuthJson>> {
    paths.validate_credential_store()?;
    if cancel.load(Ordering::SeqCst) {
        return Ok(None);
    }
    let home = tempfile::Builder::new()
        .prefix("cswitch-login-")
        .tempdir()
        .map_err(|err| CswitchError::config(format!("Could not prepare login: {err}")))?;
    // Keep workspace and login policies, but never copy auth.json. Starting
    // outside the project also avoids loading project-local Codex settings.
    match std::fs::read(paths.codex_config_file()) {
        Ok(config) => fsutil::atomic_write_private(&home.path().join("config.toml"), &config)
            .map_err(|err| CswitchError::config(format!("Could not copy login settings: {err}")))?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(CswitchError::config(format!(
                "Could not read login settings: {err}"
            )));
        }
    }
    let codex = codex
        .canonicalize()
        .map_err(|err| CswitchError::config(format!("Could not locate Codex CLI: {err}")))?;
    let mut child = LoginChild(
        Command::new(codex)
            .args(["login", "-c", "cli_auth_credentials_store=\"file\""])
            .env("CODEX_HOME", home.path())
            .current_dir(home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| CswitchError::config(format!("Could not start Codex login: {err}")))?,
    );
    let stderr = child.0.stderr.take().expect("piped stderr");
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if let Some(url) = authorize_url(&line) {
                let _ = tx.send(url);
            }
        }
    });
    let started = Instant::now();
    loop {
        for url in rx.try_iter() {
            on_url(url);
        }
        if cancel.load(Ordering::SeqCst) {
            return Ok(None);
        }
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|err| CswitchError::config(format!("Could not check Codex login: {err}")))?
        {
            let _ = reader.join();
            for url in rx.try_iter() {
                on_url(url);
            }
            if cancel.load(Ordering::SeqCst) {
                return Ok(None);
            }
            if !status.success() {
                return Err(CswitchError::config(format!(
                    "Codex login failed ({status}). Check your Codex login settings and try again."
                )));
            }
            let auth = AuthJson::read(&home.path().join("auth.json"))?.ok_or_else(|| {
                CswitchError::credential_read("Codex login returned no credentials")
            })?;
            validate_login(&auth)?;
            return Ok(Some(auth));
        }
        if started.elapsed() >= timeout {
            return Err(CswitchError::config("Login timed out. Please try again."));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

pub(crate) fn validate_login(auth: &AuthJson) -> Result<()> {
    if auth.identity().is_none() || auth.access_token().is_none() || auth.refresh_token().is_none()
    {
        return Err(CswitchError::credential_read(
            "Codex login returned incomplete ChatGPT credentials",
        ));
    }
    Ok(())
}

fn authorize_url(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|word| {
        let url = reqwest::Url::parse(word).ok()?;
        (matches!(url.scheme(), "https" | "http") && url.path() == "/oauth/authorize")
            .then(|| url.to_string())
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_login(
        script: &str,
        timeout: Duration,
        cancel_on_url: bool,
    ) -> (Result<Option<AuthJson>>, String, i32) {
        let (root, store) = crate::store::temp_store();
        let codex = root.path().join("codex-test");
        std::fs::write(&codex, format!(
            "#!/bin/sh\necho \"https://auth.openai.com/oauth/authorize?home=$CODEX_HOME&pid=$$\" >&2\n{script}\n"
        )).unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancel = AtomicBool::new(false);
        let mut home = String::new();
        let mut pid = 0;
        let result = run_login(
            &store.paths,
            &codex,
            &cancel,
            |url| {
                let url = reqwest::Url::parse(&url).unwrap();
                for (key, value) in url.query_pairs() {
                    match key.as_ref() {
                        "home" => home = value.into_owned(),
                        "pid" => pid = value.parse().unwrap(),
                        _ => {}
                    }
                }
                if cancel_on_url {
                    cancel.store(true, Ordering::SeqCst);
                }
            },
            timeout,
        );
        assert!(!home.is_empty());
        assert!(
            !Path::new(&home).exists(),
            "temporary credentials must be removed"
        );
        assert!(!store.paths.sequence_file().exists());
        assert!(!store.paths.live_auth_file().exists());
        (result, home, pid)
    }

    #[test]
    fn cancel_and_timeout_stop_the_login_process() {
        let (result, _, pid) = fake_login("exec sleep 30", LOGIN_TIMEOUT, true);
        assert!(result.unwrap().is_none());
        // Signal zero checks that the child was killed and reaped.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        let (result, _, pid) = fake_login("exec sleep 30", Duration::from_millis(150), false);
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[test]
    fn failed_or_incomplete_login_is_rejected() {
        for (script, expected) in [
            ("exit 1", "Codex login failed"),
            ("exit 0", "no credentials"),
            ("echo '{}' > auth.json", "incomplete ChatGPT credentials"),
            ("echo 'invalid' > auth.json", "invalid JSON"),
        ] {
            let (result, _, _) = fake_login(script, LOGIN_TIMEOUT, false);
            assert!(result.unwrap_err().to_string().contains(expected));
        }
    }

    #[test]
    fn only_authorization_urls_are_forwarded() {
        assert_eq!(
            authorize_url("https://auth.openai.com/oauth/authorize?state=test"),
            Some("https://auth.openai.com/oauth/authorize?state=test".into())
        );
        assert!(authorize_url("http://localhost:1455/auth/callback?code=secret").is_none());
        assert!(authorize_url("Error: secret").is_none());
    }
}
