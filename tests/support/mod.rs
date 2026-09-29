//! Drives the built `cswitch` binary against a temporary store, a temporary
//! `CODEX_HOME`, a fake `codex` on `PATH`, and usage/token endpoints that
//! refuse connections so every fetch fails fast.
#![allow(dead_code)]

pub mod usage_mock;

use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

const FAKE_CODEX: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$CS_FAKE_LOG"
case "$1" in
  --help)
    echo "Codex CLI"
    echo "      --no-daemon  Run without the app-server daemon"
    exit 0 ;;
  app-server)
    if [ "$2 $3" = "daemon version" ]; then
      if [ "$CS_FAKE_DAEMON" = "running" ]; then echo '{"status":"running"}'; exit 0; fi
      echo "daemon is not running" >&2
      exit 1
    fi
    if [ "$2 $3" = "daemon restart" ]; then echo '{"status":"restarted"}'; exit 0; fi ;;
esac
exit 0
"#;

/// Windows cannot run the shell script; the batch twin answers the same
/// three shapes (argv log, `--help`, `app-server daemon version|restart`).
#[cfg(windows)]
const FAKE_CODEX_CMD: &str = "@echo off\r\n\
>> \"%CS_FAKE_LOG%\" echo %*\r\n\
if \"%~1\"==\"--help\" (\r\n\
  echo Codex CLI\r\n\
  echo       --no-daemon  Run without the app-server daemon\r\n\
  exit /b 0\r\n\
)\r\n\
if \"%~1\"==\"app-server\" (\r\n\
  if \"%~2 %~3\"==\"daemon version\" (\r\n\
    if \"%CS_FAKE_DAEMON%\"==\"running\" (\r\n\
      echo {\"status\":\"running\"}\r\n\
      exit /b 0\r\n\
    )\r\n\
    echo daemon is not running 1>&2\r\n\
    exit /b 1\r\n\
  )\r\n\
  if \"%~2 %~3\"==\"daemon restart\" (\r\n\
    echo {\"status\":\"restarted\"}\r\n\
    exit /b 0\r\n\
  )\r\n\
)\r\n\
exit /b 0\r\n";

pub struct Run {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|err| panic!("stdout is not JSON ({err}): {:?}", self.stdout))
    }

    pub fn lines(&self) -> Vec<&str> {
        self.stdout.lines().collect()
    }
}

pub struct Cli {
    pub root: tempfile::TempDir,
    pub cswitch_home: PathBuf,
    pub codex_home: PathBuf,
    pub bin_dir: PathBuf,
    pub log: PathBuf,
    pub dead_url: String,
    pub daemon_running: bool,
    /// `CSWITCH_USAGE_URL` / `CSWITCH_TOKEN_URL` overrides; `None` refuses connections.
    pub usage_url: Option<String>,
    pub token_url: Option<String>,
}

impl Cli {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let cswitch_home = root.path().join("store");
        let codex_home = root.path().join("codex");
        let bin_dir = root.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&codex_home).unwrap();
        let script = bin_dir.join("codex");
        fs::write(&script, FAKE_CODEX).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        fs::write(bin_dir.join("codex.cmd"), FAKE_CODEX_CMD).unwrap();
        // A port nothing listens on: every usage/token request is refused at once.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        Self {
            log: root.path().join("codex-calls.log"),
            root,
            cswitch_home,
            codex_home,
            bin_dir,
            dead_url: format!("http://127.0.0.1:{port}"),
            daemon_running: false,
            usage_url: None,
            token_url: None,
        }
    }

    /// Point usage and token requests at a running mock.
    pub fn with_mock(mut self, mock: &usage_mock::UsageMock) -> Self {
        self.usage_url = Some(mock.usage_url.clone());
        self.token_url = Some(mock.token_url.clone());
        self
    }

    pub fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cswitch"));
        let path = format!(
            "{}:{}",
            self.bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.env_clear()
            .env("PATH", path)
            .env("HOME", self.root.path())
            .env("CSWITCH_HOME", &self.cswitch_home)
            .env("CODEX_HOME", &self.codex_home)
            .env(
                "CSWITCH_USAGE_URL",
                self.usage_url
                    .clone()
                    .unwrap_or_else(|| format!("{}/usage", self.dead_url)),
            )
            .env(
                "CSWITCH_TOKEN_URL",
                self.token_url
                    .clone()
                    .unwrap_or_else(|| format!("{}/token", self.dead_url)),
            )
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("CS_FAKE_LOG", &self.log)
            .env(
                "CS_FAKE_DAEMON",
                if self.daemon_running {
                    "running"
                } else {
                    "stopped"
                },
            );
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Run {
        self.run_with_stdin(args, "")
    }

    pub fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Run {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cswitch");
        {
            let mut pipe = child.stdin.take().unwrap();
            pipe.write_all(stdin.as_bytes()).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        Run {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    pub fn live_path(&self) -> PathBuf {
        self.codex_home.join("auth.json")
    }

    pub fn write_live(&self, auth: &Value) {
        fs::write(
            self.live_path(),
            serde_json::to_string_pretty(auth).unwrap(),
        )
        .unwrap();
    }

    pub fn remove_live(&self) {
        let _ = fs::remove_file(self.live_path());
    }

    pub fn live(&self) -> Value {
        read_json(&self.live_path())
    }

    pub fn roster(&self) -> Value {
        read_json(&self.cswitch_home.join("sequence.json"))
    }

    pub fn credential_path(&self, slot: u32) -> PathBuf {
        self.cswitch_home
            .join("credentials")
            .join(format!("{slot}.json"))
    }

    pub fn credential(&self, slot: u32) -> Value {
        read_json(&self.credential_path(slot))
    }

    pub fn live_backups(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.codex_home)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("auth.json.bak."))
            .collect();
        names.sort();
        names
    }

    pub fn codex_calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn clear_codex_calls(&self) {
        let _ = fs::remove_file(&self.log);
    }

    /// `add` the given ChatGPT login and return the run.
    pub fn add_chatgpt(&self, email: &str, account_id: &str, refresh: &str) -> Run {
        self.write_live(&chatgpt_auth(email, account_id, refresh));
        let run = self.run(&["add"]);
        assert_eq!(run.status, 0, "add failed: {}{}", run.stdout, run.stderr);
        run
    }

    /// `add` a ChatGPT login whose opaque access token picks the mock's reply.
    pub fn add_scripted(&self, email: &str, account_id: &str, access: &str) -> Run {
        let refresh = usage_mock::live_refresh_token(email, account_id);
        self.write_live(&chatgpt_auth_with_tokens(
            email, account_id, access, &refresh,
        ));
        let run = self.run(&["add"]);
        assert_eq!(run.status, 0, "add failed: {}{}", run.stdout, run.stderr);
        run
    }

    /// The backup store's log file.
    pub fn log_path(&self) -> PathBuf {
        self.cswitch_home.join("cswitch.log")
    }
}

pub fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap()
}

/// An unsigned JWT with the claims Codex puts in an id_token; `exp` is far away.
pub fn jwt(email: &str, account_id: &str) -> String {
    let title = if account_id.ends_with("-team") {
        "Acme"
    } else {
        ""
    };
    let claims = json!({
        "email": email,
        "exp": 4_102_444_800i64,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": format!("user-{}", email.to_lowercase()),
            "chatgpt_plan_type": "plus",
            "organizations": [{"id": account_id, "title": title, "role": "owner", "is_default": true}]
        }
    });
    format!(
        "hdr.{}.sig",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    )
}

pub fn chatgpt_auth(email: &str, account_id: &str, refresh: &str) -> Value {
    chatgpt_auth_at(email, account_id, refresh, "2026-09-29T10:00:00Z")
}

pub fn chatgpt_auth_at(email: &str, account_id: &str, refresh: &str, last_refresh: &str) -> Value {
    json!({
        "OPENAI_API_KEY": null,
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": jwt(email, account_id),
            "access_token": jwt(email, account_id),
            "refresh_token": refresh,
            "account_id": account_id
        },
        "last_refresh": last_refresh
    })
}

/// A ChatGPT login with an explicit (opaque) access token: the identity still
/// comes from the id_token, so the mock can key its replies on the bearer.
pub fn chatgpt_auth_with_tokens(
    email: &str,
    account_id: &str,
    access: &str,
    refresh: &str,
) -> Value {
    json!({
        "OPENAI_API_KEY": null,
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": jwt(email, account_id),
            "access_token": access,
            "refresh_token": refresh,
            "account_id": account_id
        },
        "last_refresh": "2026-09-29T10:00:00Z"
    })
}

pub fn api_key_auth(key: &str) -> Value {
    json!({"auth_mode": "apikey", "OPENAI_API_KEY": key})
}
