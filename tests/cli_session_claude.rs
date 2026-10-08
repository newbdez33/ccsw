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
