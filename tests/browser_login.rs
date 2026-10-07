//! Run the TUI worker in a separate process with a fake Codex executable.
#![cfg(unix)]

mod support;

use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cswitch::cli::tui::TuiStart;
use cswitch::paths::Paths;
use cswitch::tui::app::{App, Inbound};
use cswitch::tui::theme::ThemeName;
use cswitch::tui::worker::Runtime;
use support::{Cli, chatgpt_auth_at};

const LOGIN_SCRIPT: &str = r#"#!/bin/sh
if [ "$1" != login ]; then
  echo '{"status":"stopped"}'
  exit 1
fi
test "$2" = -c || exit 20
test "$3" = 'cli_auth_credentials_store="file"' || exit 21
test ! -e "$CODEX_HOME/auth.json" || exit 22
cmp "$CS_LOGIN_CONFIG" "$CODEX_HOME/config.toml" || exit 23
printf '%s\n' "$CODEX_HOME" > "$CS_LOGIN_HOME"
printf '%s\n' "$$" > "$CS_LOGIN_PID"
if [ "$CS_LOGIN_TEST" = cancel-wrapper ]; then
  sleep 30 &
  native=$!
  trap 'kill "$native"; wait "$native"; exit 0' TERM
  printf '%s\n' "$native" >> "$CS_LOGIN_PID"
fi
echo 'https://auth.openai.com/oauth/authorize?state=test' >&2
case "$CS_LOGIN_TEST" in
  cancel|drop) exec sleep 30 ;;
  cancel-wrapper) wait "$native"; exit 0 ;;
esac
cp "$CS_LOGIN_AUTH" "$CODEX_HOME/auth.json"
"#;

fn run_flow(mode: &str) -> Cli {
    let cli = Cli::new();
    cli.add_chatgpt("old@example.com", "old", "saved");
    let departing = chatgpt_auth_at("old@example.com", "old", "rotated", "2099-01-01T00:00:00Z");
    cli.write_live(&departing);
    let incoming = if mode == "relogin" {
        chatgpt_auth_at(
            "old@example.com",
            "old",
            "new-login",
            "2026-10-07T00:00:00Z",
        )
    } else {
        chatgpt_auth_at(
            "new@example.com",
            "new",
            "new-login",
            "2026-10-07T00:00:00Z",
        )
    };
    let auth_path = cli.root.path().join("incoming.json");
    fs::write(&auth_path, incoming.to_string()).unwrap();
    let config = cli.codex_home.join("config.toml");
    fs::write(
        &config,
        "cli_auth_credentials_store = \"file\"\nforced_login_method = \"chatgpt\"\n",
    )
    .unwrap();
    fs::write(cli.bin_dir.join("codex"), LOGIN_SCRIPT).unwrap();
    let isolated_env = cli.command();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child.env_clear();
    for (key, value) in isolated_env.get_envs() {
        if let Some(value) = value {
            child.env(key, value);
        }
    }
    let home_log = cli.root.path().join("login-home");
    let pid_log = cli.root.path().join("login-pid");
    let output = child
        .args(["--exact", "login_worker_child", "--nocapture"])
        .env("CS_LOGIN_TEST", mode)
        .env("CS_LOGIN_AUTH", &auth_path)
        .env("CS_LOGIN_CONFIG", config)
        .env("CS_LOGIN_HOME", &home_log)
        .env("CS_LOGIN_PID", &pid_log)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let temporary_home = fs::read_to_string(home_log).unwrap();
    assert!(!std::path::Path::new(temporary_home.trim()).exists());
    for pid in fs::read_to_string(pid_log).unwrap().lines() {
        let pid: i32 = pid.parse().unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "login child must be reaped"
        );
    }
    if matches!(mode, "cancel" | "cancel-wrapper" | "drop") {
        assert_eq!(cli.live(), departing);
        assert_eq!(cli.credential(1)["tokens"]["refresh_token"], "saved");
        assert_eq!(cli.roster()["sequence"].as_array().unwrap().len(), 1);
        assert!(cli.live_backups().is_empty());
    } else {
        assert_eq!(cli.live(), incoming);
        assert_eq!(cli.live_backups().len(), 1);
        let backup = support::read_json(&cli.codex_home.join(&cli.live_backups()[0]));
        assert_eq!(backup, departing);
        if mode == "relogin" {
            assert_eq!(
                cli.credential(1),
                incoming,
                "fresh tokens must win over clock skew"
            );
            assert_eq!(cli.roster()["sequence"].as_array().unwrap().len(), 1);
        } else {
            assert_eq!(
                cli.credential(1),
                departing,
                "preserve the departing rotation"
            );
            assert_eq!(cli.credential(2), incoming);
            assert_eq!(cli.roster()["sequence"].as_array().unwrap().len(), 2);
        }
    }
    cli
}

#[test]
fn login_saves_and_activates_the_account() {
    run_flow("new");
    run_flow("relogin");
}

#[test]
fn cancelling_or_closing_the_tui_preserves_credentials() {
    run_flow("cancel");
    run_flow("cancel-wrapper");
    run_flow("drop");
}

#[test]
fn login_worker_child() {
    let Ok(mode) = std::env::var("CS_LOGIN_TEST") else {
        return;
    };
    let mut app = App::new(TuiStart::Dashboard, ThemeName::Dark, 90.0, None);
    let mut runtime = Runtime::new(Paths::from_env().unwrap());
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Down), 0.0);
    }
    app.handle_key(key(KeyCode::Enter), 0.0);
    let commands = app.handle_key(key(KeyCode::Enter), 0.0);
    runtime.execute(commands, &mut app, 0.0);
    let started = Instant::now();
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "login did not finish"
        );
        for message in runtime.drain() {
            if matches!(message, Inbound::LoginUrl(_)) {
                if mode == "drop" {
                    drop(runtime);
                    return;
                }
                if mode.starts_with("cancel") {
                    let commands = app.handle_key(key(KeyCode::Esc), 0.0);
                    runtime.execute(commands, &mut app, 0.0);
                }
            }
            if let Inbound::ActionDone(result) = &message {
                assert!(result.ok, "{:?}", result.lines);
                app.receive(message, 0.0);
                assert!(!app.busy());
                return;
            }
            app.receive(message, 0.0);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
