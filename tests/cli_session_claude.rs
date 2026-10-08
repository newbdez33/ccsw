mod support;

use ccsw::fsutil::read_json;
use support::Cli;

fn mixed() -> Cli {
    let cli = Cli::new();
    cli.add_chatgpt("code@example.com", "acct", "rt-code");
    cli.add_claude("session@example.com", "org", "", "rt-session");
    cli.write_claude_live("default@example.com", "default-org", "", "rt-default");
    cli
}

#[test]
fn runs_the_selected_provider_and_forwards_arguments_without_daemon_flags() {
    let cli = mixed();
    // A setting for the other provider cannot block this launch.
    std::fs::write(
        cli.codex_home.join("config.toml"),
        "cli_auth_credentials_store = \"keyring\"\n",
    )
    .unwrap();
    let result = cli.run(&["run", "2", "--", "--resume", "conversation-id"]);
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert_eq!(
        std::fs::read_to_string(&cli.claude_log).unwrap(),
        "--resume conversation-id\n"
    );
    let profile = cli.ccsw_home.join("sessions/2-session_example.com");
    assert_eq!(
        read_json(&profile.join(".credentials.json"))
            .unwrap()
            .unwrap()["claudeAiOauth"]["refreshToken"],
        "rt-session"
    );
    assert!(!profile.join("auth.json").exists());
}

#[test]
fn mixed_mappings_need_a_selector_and_unmap_can_remove_one_provider() {
    let cli = mixed();
    let work = cli.root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let target = work.to_str().unwrap();
    assert_eq!(cli.run(&["map", "1", target]).status, 0);
    assert_eq!(cli.run(&["map", "2", target]).status, 0);
    let ambiguous = cli
        .command()
        .current_dir(&work)
        .arg("run")
        .output()
        .unwrap();
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("provider"));
    let selected = cli
        .command()
        .current_dir(&work)
        .args(["run", "claude", "--", "--resume"])
        .output()
        .unwrap();
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    assert_eq!(cli.run(&["unmap", target, "claude"]).status, 0);
    let listed = cli.run(&["map"]);
    assert!(listed.stdout.contains("code@example.com"));
    assert!(!listed.stdout.contains("session@example.com"));
    assert_eq!(cli.run(&["unmap", target]).status, 0);
    assert!(cli.run(&["map"]).stdout.contains("No directory mappings"));
}

#[test]
fn env_outputs_the_selected_home_and_auth_override_unsets() {
    let cli = mixed();
    let output = cli
        .command()
        .args(["env", "2", "--shell", "fish"])
        .env("ANTHROPIC_API_KEY", "test-only-key")
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", "")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = String::from_utf8(output.stdout).unwrap();
    assert!(lines.contains("set -gx CLAUDE_CONFIG_DIR "));
    assert!(lines.contains("set -e ANTHROPIC_API_KEY"));
    assert!(lines.contains("set -e CLAUDE_SECURESTORAGE_CONFIG_DIR"));
    assert!(!lines.contains("CODEX_HOME"));
    let result = cli.run(&["env", "claude", "--unset"]);
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert_eq!(result.stdout, "unset CLAUDE_CONFIG_DIR\n");
}

#[test]
fn require_session_refuses_the_default_login_fast_path() {
    let cli = mixed();
    cli.write_claude_live("session@example.com", "org", "", "rt-session");
    // Use the true default home inside the temporary HOME.
    let home = cli.root.path().join(".claude");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::copy(
        cli.claude_home.join(".credentials.json"),
        home.join(".credentials.json"),
    )
    .unwrap();
    std::fs::copy(
        cli.claude_home.join(".claude.json"),
        cli.root.path().join(".claude.json"),
    )
    .unwrap();
    let output = cli
        .command()
        .env_remove("CLAUDE_CONFIG_DIR")
        .args(["run", "2", "--require-session"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("default login"));
}

#[cfg(unix)]
#[test]
fn child_receives_the_profile_and_scrubbed_auth_and_its_exit_code_is_preserved() {
    let cli = mixed();
    std::fs::write(cli.bin_dir.join("claude"), "#!/bin/sh\nprintf 'profile=%s\\n' \"$CLAUDE_CONFIG_DIR\"\nprintf 'key=%s\\n' \"${ANTHROPIC_API_KEY-unset}\"\nprintf 'secure=%s\\n' \"${CLAUDE_SECURESTORAGE_CONFIG_DIR-unset}\"\nexit 23\n").unwrap();
    let output = cli
        .command()
        .args(["run", "2"])
        .env("ANTHROPIC_API_KEY", "test-only-key")
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", "")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(23),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(
        &format!("profile={}\n", cli.ccsw_home.join("sessions/2-session_example.com").display())
    ));
    assert!(stdout.contains("key=unset\n"));
    assert!(stdout.contains("secure=unset\n"));
}

#[test]
fn a_missing_provider_binary_reports_the_selected_tool() {
    let cli = mixed();
    std::fs::remove_file(cli.bin_dir.join("claude")).unwrap();
    #[cfg(windows)]
    std::fs::remove_file(cli.bin_dir.join("claude.cmd")).unwrap();
    let output = cli
        .command()
        .env("PATH", &cli.bin_dir)
        .args(["run", "2"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("'claude' was not found on PATH"));
    assert!(!cli.ccsw_home.join("sessions").exists());
}

#[test]
fn a_new_setup_token_cannot_reuse_a_running_old_generation() {
    let cli = Cli::new();
    assert_eq!(
        cli.run(&[
            "add-token",
            "sk-ant-oat-old",
            "--email",
            "session@example.com"
        ])
        .status,
        0
    );
    assert_eq!(cli.run(&["env", "1"]).status, 0);
    let profile = cli.ccsw_home.join("sessions/1-session_example.com");
    ccsw::fsutil::write_json_private(
        &profile.join("sessions/owner.json"),
        &serde_json::json!({"pid": std::process::id()}),
    )
    .unwrap();
    assert_eq!(
        cli.run(&[
            "add-token",
            "sk-ant-oat-new",
            "--email",
            "session@example.com"
        ])
        .status,
        0
    );
    let output = cli.run(&["env", "1"]);
    assert_ne!(output.status, 0, "{}", output.stdout);
    assert!(output.stderr.contains("close"));
    assert_eq!(
        read_json(&profile.join(".credentials.json"))
            .unwrap()
            .unwrap()["claudeAiOauth"]["accessToken"],
        "sk-ant-oat-old"
    );
    std::fs::remove_file(profile.join("sessions/owner.json")).unwrap();
    assert_eq!(cli.run(&["env", "1"]).status, 0);
    assert_eq!(
        read_json(&profile.join(".credentials.json"))
            .unwrap()
            .unwrap()["claudeAiOauth"]["accessToken"],
        "sk-ant-oat-new"
    );
}

#[test]
fn a_new_login_with_a_shorter_expiry_is_not_replaced_by_the_old_profile() {
    let cli = Cli::new();
    cli.write_claude_live_with(
        &support::claude_creds("rt-old", "cat-old"),
        &support::claude_config("session@example.com", "org", ""),
    );
    assert_eq!(cli.run(&["add", "claude"]).status, 0);
    assert_eq!(cli.run(&["env", "1"]).status, 0);
    let mut replacement = support::claude_creds("rt-new", "cat-new");
    replacement["claudeAiOauth"]["expiresAt"] = serde_json::json!(4_000_000_000_000i64);
    cli.write_claude_live_with(
        &replacement,
        &support::claude_config("session@example.com", "org", ""),
    );
    assert_eq!(cli.run(&["add", "claude"]).status, 0);
    assert_eq!(cli.run(&["env", "1"]).status, 0);
    let profile = cli.ccsw_home.join("sessions/1-session_example.com");
    assert_eq!(
        read_json(&profile.join(".credentials.json"))
            .unwrap()
            .unwrap()["claudeAiOauth"]["refreshToken"],
        "rt-new"
    );
    assert_eq!(cli.credential(1)["claudeAiOauth"]["refreshToken"], "rt-new");
}

#[test]
fn moving_an_existing_identity_with_add_checks_the_source_before_the_target() {
    let cli = Cli::new();
    cli.run(&[
        "add-token",
        "sk-ant-oat-source",
        "--email",
        "source@example.com",
    ]);
    cli.run(&[
        "add-token",
        "sk-ant-oat-target",
        "--email",
        "target@example.com",
    ]);
    assert_eq!(cli.run(&["env", "1"]).status, 0);
    let profile = cli.ccsw_home.join("sessions/1-source_example.com");
    ccsw::fsutil::write_json_private(
        &profile.join("sessions/owner.json"),
        &serde_json::json!({"pid": std::process::id()}),
    )
    .unwrap();
    let before = cli.credential(2);
    let result = cli.run_with_stdin(
        &[
            "add-token",
            "sk-ant-oat-source",
            "--email",
            "source@example.com",
            "--slot",
            "2",
        ],
        "y\n",
    );
    assert_ne!(result.status, 0);
    assert!(profile.join(".credentials.json").exists());
    assert_eq!(cli.credential(2), before);
    assert!(cli.roster()["accounts"]["1"].is_object());
}

#[test]
fn mutations_accept_quiet_slots_that_share_a_credential() {
    let cli = Cli::new();
    for email in ["first@example.com", "second@example.com"] {
        assert_eq!(
            cli.run(&["add-token", "sk-ant-oat-shared", "--email", email])
                .status,
            0
        );
    }
    assert_eq!(cli.run(&["swap", "1", "2"]).status, 0);
    assert_eq!(cli.run(&["move", "1", "2"]).status, 0);
    let output = cli.run_with_stdin(&["purge"], "y\n");
    assert_eq!(output.status, 0, "{}", output.stderr);
    assert!(!cli.ccsw_home.exists());
}

#[test]
fn purge_refuses_a_running_profile_before_removing_any_store_data() {
    let cli = Cli::new();
    cli.run(&[
        "add-token",
        "sk-ant-oat-live",
        "--email",
        "session@example.com",
    ]);
    cli.run(&["env", "1"]);
    let profile = cli.ccsw_home.join("sessions/1-session_example.com");
    ccsw::fsutil::write_json_private(
        &profile.join("sessions/owner.json"),
        &serde_json::json!({"pid": std::process::id()}),
    )
    .unwrap();
    let result = cli.run_with_stdin(&["purge"], "y\n");
    assert_ne!(result.status, 0);
    assert!(profile.join(".credentials.json").exists());
    assert!(cli.credential_path(1).exists());
}

#[cfg(unix)]
#[test]
fn launch_waits_for_an_inflight_refresh_before_seeding_the_profile() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (seen_tx, seen_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let body = if index == 0 {
                assert!(first.starts_with("POST "));
                seen_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                r#"{"access_token":"cat-new","refresh_token":"rt-new","expires_in":3600,"scope":"user:inference user:profile"}"#
            } else {
                r#"{"five_hour":{"utilization":5,"resets_at":"2099-01-01T00:00:00Z"},"seven_day":{"utilization":5,"resets_at":"2099-01-01T00:00:00Z"}}"#
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let mut cli = Cli::new();
    cli.claude_token_url = Some(format!("http://{address}/token"));
    cli.claude_usage_url = Some(format!("http://{address}/usage"));
    let mut old = support::claude_creds("rt-old", "cat-old");
    old["claudeAiOauth"]["expiresAt"] = serde_json::json!(1);
    cli.write_claude_live_with(
        &old,
        &support::claude_config("session@example.com", "org", ""),
    );
    assert_eq!(cli.run(&["add", "claude"]).status, 0);
    cli.remove_claude_live();
    std::fs::write(
        cli.bin_dir.join("claude"),
        "#!/bin/sh\ncat \"$CLAUDE_CONFIG_DIR/.credentials.json\" > \"$CS_STARTED\"\n",
    )
    .unwrap();
    let started = cli.root.path().join("started.json");
    let listing = cli
        .command()
        .args(["list", "claude", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    seen_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let launch = cli
        .command()
        .args(["run", "1"])
        .env("CS_STARTED", &started)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let started_early = started.exists();
    release_tx.send(()).unwrap();
    let listed = listing.wait_with_output().unwrap();
    let launched = launch.wait_with_output().unwrap();
    server.join().unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(
        !started_early,
        "the CLI started with a grant that was being consumed"
    );
    assert!(
        launched.status.success(),
        "{}",
        String::from_utf8_lossy(&launched.stderr)
    );
    assert_eq!(
        read_json(&started).unwrap().unwrap()["claudeAiOauth"]["refreshToken"],
        "rt-new"
    );
}
