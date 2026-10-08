//! The mixed roster through the built binary: both logins captured by `add`,
//! Claude switches under the lock protocol, two-block `list`, `status`,
//! JSON v2, and the provider selector.

mod support;

use serde_json::json;
use support::usage_mock::{
    self, CLAUDE_LIMIT_7D, CLAUDE_OK, CLAUDE_POOL_NAME, CLAUDE_REFRESHED, CLAUDE_ROTATED_REFRESH,
    CLAUDE_STALE, UsageMock,
};
use support::{Cli, api_key_auth, chatgpt_auth, claude_config, claude_creds};

fn mixed() -> Cli {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    cli.add_claude("One@example.com", "org-1", "", "crt-1");
    cli.add_claude("two@example.com", "org-2", "Acme", "crt-2");
    cli
}

#[test]
fn add_captures_both_logins_even_with_the_same_email() {
    let cli = Cli::new();
    cli.write_live(&chatgpt_auth("Me@Example.com", "acct-me", "rt-me"));
    cli.write_claude_live("Me@Example.com", "org-me", "Acme", "crt-me");
    let run = cli.run(&["add"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "Added Account 1: me@example.com [Plus]\nAdded Account 2: me@example.com [Acme]\n"
    );
    let roster = cli.roster();
    assert_eq!(roster["accounts"]["1"]["provider"], "codex");
    assert_eq!(roster["accounts"]["2"]["provider"], "claude");
    assert_eq!(roster["accounts"]["2"]["organizationUuid"], "org-me");
    assert_eq!(roster["accounts"]["2"]["uuid"], "uuid-me@example.com");
    assert_eq!(roster["activeByProvider"], json!({"claude": 2, "codex": 1}));
    assert_eq!(roster["activeAccountNumber"], 1);
    let stored = cli.credential(2);
    assert_eq!(stored["claudeAiOauth"]["refreshToken"], "crt-me");
    assert_eq!(stored["oauthAccount"]["billingType"], "stripe");
    assert!(stored.get("mcpOAuth").is_none());

    let run = cli.run(&["add"]);
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Updated credentials for Account 1 (me@example.com [Plus]).\n\
         Updated credentials for Account 2 (me@example.com [Acme]).\n\
         Both current logins were already managed: Account-1 (codex), Account-2 (claude) — nothing new was added.\n"
    );
}

#[test]
fn add_selector_missing_login_and_flags() {
    let cli = Cli::new();
    let run = cli.run(&["add"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "Error: No active Codex or Claude login found. Log in first."
    );
    cli.write_live(&chatgpt_auth("alice@example.com", "acct-alice", "rt-a"));
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "Error: No active Claude account found. Please log in first."
    );
    let run = cli.run(&["add", "codex", "--alias", "work"]);
    assert_eq!(run.stdout, "Added Account 1: alice@example.com [Plus]\n");
    cli.write_claude_live("c@example.com", "org", "", "crt-c");
    let run = cli.run(&["add", "--slot", "5"]);
    assert_eq!(run.status, 1);
    assert!(
        run.stderr
            .starts_with("Error: --slot/--alias need a single login"),
        "{}",
        run.stderr
    );
    let run = cli.run(&["add", "claude", "--slot", "5"]);
    assert_eq!(run.stdout, "Added Account 5: c@example.com [personal]\n");
    let run = cli.run(&["alias", "5", "claude"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "Error: alias 'claude' is reserved for the provider selector"
    );
}

#[test]
fn add_token_routes_by_prefix() {
    let cli = Cli::new();
    let run = cli.run(&["add-token", "sk-ant-api03-key"]);
    assert_eq!(
        run.stdout,
        "Added Account 1: api-key-1@token.local [personal] (from API key)\n"
    );
    assert_eq!(cli.roster()["accounts"]["1"]["provider"], "claude");
    assert_eq!(cli.roster()["accounts"]["1"]["kind"], "api_key");
    assert_eq!(cli.credential(1)["primaryApiKey"], "sk-ant-api03-key");
    let run = cli.run(&["add-token", "sk-ant-oat01-tok", "--email", "me@example.com"]);
    assert_eq!(
        run.stdout,
        "Added Account 2: me@example.com [personal] (from token)\n"
    );
    assert_eq!(cli.roster()["accounts"]["2"]["provider"], "claude");
    assert!(cli.roster()["accounts"]["2"].get("kind").is_none());
    assert_eq!(
        cli.credential(2)["claudeAiOauth"]["accessToken"],
        "sk-ant-oat01-tok"
    );
    let run = cli.run(&["add-token", "sk-openai"]);
    assert_eq!(
        run.stdout,
        "Added Account 3: api-key-3@token.local [personal] (from API key)\n"
    );
    assert_eq!(cli.credential(3), api_key_auth("sk-openai"));
}

#[test]
fn switching_a_claude_slot_leaves_codex_alone() {
    let cli = mixed();
    let codex_before = cli.live();
    let run = cli.run(&["switch", "2"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines[0], "Switched to Account-2 (one@example.com)");
    assert_eq!(lines[1], "Codex accounts:");
    assert!(lines.contains(&"Claude accounts:"), "{lines:?}");
    assert!(
        run.stdout
            .trim_end()
            .ends_with("New account is active on your next message — no restart needed."),
        "{}",
        run.stdout
    );
    assert_eq!(
        cli.claude_credentials()["claudeAiOauth"]["refreshToken"],
        "crt-1"
    );
    assert_eq!(
        cli.claude_config()["oauthAccount"]["emailAddress"],
        "One@example.com"
    );
    assert_eq!(
        cli.claude_config()["oauthAccount"]["organizationUuid"],
        "org-1"
    );
    assert_eq!(cli.live(), codex_before);
    assert!(
        cli.live_backups().is_empty(),
        "no Codex backup for a Claude switch"
    );
    assert_eq!(cli.claude_backups().len(), 1);
    assert!(
        cli.codex_calls().is_empty(),
        "the Codex daemon is never probed"
    );
    assert_eq!(
        cli.roster()["activeByProvider"],
        json!({"claude": 2, "codex": 1})
    );
    assert!(!cli.claude_home.join(".oauth_refresh.lock").exists());
    assert!(!cli.root.path().join("claude.lock").exists());
    assert!(!cli.claude_home.join(".claude.json.lock").exists());

    let run = cli.run(&["switch", "2", "--json"]);
    let payload = run.json();
    assert_eq!(payload["schemaVersion"], 2);
    assert_eq!(payload["switched"], false);
    assert_eq!(payload["reason"], "already-active");
    assert_eq!(payload["provider"], "claude");
    assert_eq!(payload["to"]["provider"], "claude");
}

#[test]
fn switch_claude_keeps_sibling_keys() {
    let cli = mixed();
    let mut creds = claude_creds("crt-2", "cat-crt-2");
    creds["mcpOAuth"]["other"] = json!({"accessToken": "keep-me"});
    cli.write_claude_live_with(&creds, &claude_config("two@example.com", "org-2", "Acme"));
    let run = cli.run(&["switch", "2"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let after = cli.claude_credentials();
    assert_eq!(after["claudeAiOauth"]["refreshToken"], "crt-1");
    assert_eq!(after["mcpOAuth"]["other"]["accessToken"], "keep-me");
    assert_eq!(after["mcpOAuth"]["srv"]["accessToken"], "mcp-token");
    assert_eq!(
        cli.claude_config()["projects"]["/tmp/p"]["allowedTools"],
        json!([])
    );
    assert_eq!(cli.claude_config()["numStartups"], 7);
    let backup = support::read_json(
        &cli.ccsw_home
            .join("backups")
            .join("claude")
            .join(&cli.claude_backups()[0]),
    );
    assert_eq!(
        backup["credentials"]["claudeAiOauth"]["refreshToken"],
        "crt-2"
    );
    assert_eq!(backup["oauthAccount"]["emailAddress"], "two@example.com");
}

#[test]
fn bare_switch_needs_a_selector_when_both_providers_exist() {
    let cli = mixed();
    let run = cli.run(&["switch"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "Error: Both Codex and Claude accounts are managed — say which: ccsw switch codex | ccsw switch claude"
    );
    let run = cli.run(&["switch", "claude", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let payload = run.json();
    assert_eq!(payload["switched"], true);
    assert_eq!(payload["from"]["number"], 3);
    assert_eq!(payload["to"]["number"], 2);
    assert_eq!(payload["provider"], "claude");
    let run = cli.run(&["switch", "codex"]);
    assert_eq!(run.status, 0);
    assert!(
        run.stdout.contains("Only one account is managed"),
        "{}",
        run.stdout
    );
    assert_eq!(cli.live()["tokens"]["refresh_token"], "rt-a");
    let run = cli.run(&["--switch"]);
    assert_eq!(run.status, 1, "the legacy flag follows the same rule");
}

#[test]
fn list_prints_two_blocks_and_json_v2() {
    let cli = mixed();
    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines[0], "Codex accounts:");
    assert_eq!(lines[1], "  1: alice@example.com [Plus] (active)");
    let claude_at = lines.iter().position(|l| *l == "Claude accounts:").unwrap();
    assert_eq!(lines[claude_at - 1], "");
    assert_eq!(lines[claude_at + 1], "  2: one@example.com [personal]");
    assert_eq!(lines[claude_at + 3], "", "a blank line between accounts");
    assert!(
        lines[claude_at + 4].starts_with("  3: two@example.com [Acme] (active)"),
        "{}",
        lines[claude_at + 4]
    );
    let run = cli.run(&["list", "claude"]);
    assert_eq!(run.lines()[0], "Claude accounts:");
    assert!(!run.stdout.contains("alice@example.com"));

    let run = cli.run(&["list", "--json"]);
    let payload = run.json();
    assert_eq!(payload["schemaVersion"], 2);
    assert_eq!(payload["activeAccountNumber"], 1);
    assert_eq!(payload["active"], json!({"codex": 1, "claude": 3}));
    let rows = payload["accounts"].as_array().unwrap();
    assert_eq!(rows[0]["provider"], "codex");
    assert_eq!(rows[2]["provider"], "claude");
    assert_eq!(rows[2]["active"], true);
    assert_eq!(rows[1]["active"], false);
    assert_eq!(run.stderr, "");
}

#[test]
fn status_shows_both_providers() {
    let cli = mixed();
    let run = cli.run(&["status"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(
        lines[0],
        "Codex status: Account-1 (alice@example.com [Plus])"
    );
    assert_eq!(lines[1], "  Total managed accounts: 3");
    let claude_at = lines
        .iter()
        .position(|l| l.starts_with("Claude status:"))
        .unwrap();
    assert_eq!(
        lines[claude_at],
        "Claude status: Account-3 (two@example.com [Acme])"
    );
    let run = cli.run(&["status", "codex"]);
    assert_eq!(
        run.lines()[0],
        "Status: Account-1 (alice@example.com [Plus])"
    );
    let run = cli.run(&["status", "--json"]);
    let payload = run.json();
    assert_eq!(payload["active"]["codex"]["number"], 1);
    assert_eq!(payload["active"]["claude"]["number"], 3);
    assert_eq!(payload["active"]["claude"]["provider"], "claude");
    assert_eq!(payload["active"]["claude"]["managed"], true);
    cli.remove_claude_live();
    let run = cli.run(&["status", "--json"]);
    assert!(run.json()["active"]["claude"].is_null());
}

#[test]
fn v1_roster_is_read_as_codex() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    let mut roster = cli.roster();
    roster["accounts"]["1"]
        .as_object_mut()
        .unwrap()
        .remove("provider");
    roster.as_object_mut().unwrap().remove("activeByProvider");
    std::fs::write(cli.ccsw_home.join("sequence.json"), roster.to_string()).unwrap();
    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.lines()[0], "Accounts:");
    cli.remove_live();
    let run = cli.run(&["switch", "1", "--force"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(cli.live()["tokens"]["refresh_token"], "rt-a");
    assert_eq!(cli.roster()["accounts"]["1"]["provider"], "codex");
    assert_eq!(cli.roster()["activeByProvider"], json!({"codex": 1}));
}

#[test]
fn claude_usage_rows_refresh_and_the_active_login_is_never_refreshed() {
    let mock = UsageMock::start();
    let cli = Cli::new().with_mock(&mock);
    cli.write_claude_live_with(
        &claude_creds(
            &usage_mock::claude_live_refresh_token("one@example.com"),
            CLAUDE_OK,
        ),
        &claude_config("one@example.com", "org-1", ""),
    );
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    cli.write_claude_live_with(
        &claude_creds(
            &usage_mock::claude_live_refresh_token("two@example.com"),
            CLAUDE_STALE,
        ),
        &claude_config("two@example.com", "org-2", ""),
    );
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    // Two is live and stale: a 401 is reported, never refreshed. One is inactive and fine.
    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines[0], "Accounts:");
    assert!(lines[1].starts_with("  1: one@example.com"));
    // Labels are padded to the widest one (`Fable:`), so match the parts, not the columns.
    assert!(
        lines[2].starts_with("     ├ $$:") && lines[2].contains(" 15%"),
        "{}",
        lines[2]
    );
    assert!(lines[2].ends_with("  $7.29 / $50.00"), "{}", lines[2]);
    assert!(
        lines[3].starts_with("     ├ 5h:") && lines[3].contains(" 40%"),
        "{}",
        lines[3]
    );
    assert!(
        lines[4].starts_with("     ├ 7d:") && lines[4].contains(" 55%"),
        "{}",
        lines[4]
    );
    assert!(
        lines[5].starts_with(&format!("     └ {CLAUDE_POOL_NAME}:")),
        "{}",
        lines[5]
    );
    assert!(lines[7].starts_with("  2: two@example.com [personal] (active)"));
    assert_eq!(lines[8], "     usage unavailable (http-401)");
    assert_eq!(
        mock.claude_token_calls(),
        0,
        "the active login belongs to Claude Code"
    );

    // Make one the live login (fresh) and leave two inactive with a dead bearer: 401 → refresh → retry.
    cli.write_claude_live_with(
        &claude_creds(
            &usage_mock::claude_live_refresh_token("one@example.com"),
            CLAUDE_OK,
        ),
        &claude_config("one@example.com", "org-1", ""),
    );
    // The poll policy keeps the first pass's results for 180 s, so drop the usage cache to make
    // both rows due again.
    std::fs::remove_file(cli.ccsw_home.join("cache").join("usage.json")).unwrap();
    let run = cli.run(&["list", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let payload = run.json();
    let two = payload["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["number"] == 2)
        .unwrap();
    assert_eq!(two["usageStatus"], "ok");
    assert_eq!(two["usage"]["fiveHour"]["pct"], 30.0);
    assert_eq!(
        cli.credential(2)["claudeAiOauth"]["refreshToken"],
        CLAUDE_ROTATED_REFRESH
    );
    assert_eq!(
        cli.credential(2)["claudeAiOauth"]["accessToken"],
        CLAUDE_REFRESHED
    );
    assert_eq!(mock.claude_token_calls(), 1);
    let one = payload["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["number"] == 1)
        .unwrap();
    assert_eq!(
        one["usage"]["spend"],
        json!({"used": 7.29, "limit": 50.0, "pct": 14.58, "currency": "USD"})
    );
    assert_eq!(one["usage"]["scoped"][0]["name"], CLAUDE_POOL_NAME);
    assert_eq!(one["usage"]["scoped"][0]["pct"], 62.0);
    let _ = CLAUDE_LIMIT_7D;
}

#[test]
fn stale_claude_lock_is_taken_over_and_a_fresh_one_fails() {
    let cli = mixed();
    let stale = cli.claude_home.join(".oauth_refresh.lock");
    std::fs::create_dir(&stale).unwrap();
    let old = filetime::FileTime::from_unix_time(filetime::FileTime::now().unix_seconds() - 120, 0);
    filetime::set_file_mtime(&stale, old).unwrap();
    let run = cli.run(&["switch", "2"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!stale.exists());

    std::fs::create_dir(&stale).unwrap();
    let run = cli.run(&["switch", "3"]);
    assert_eq!(run.status, 1);
    assert!(
        run.stderr.starts_with("Error: Claude Code is holding "),
        "{}",
        run.stderr
    );
    assert!(run.stderr.trim().ends_with("; retry in a moment"));
    assert_eq!(
        cli.claude_config()["oauthAccount"]["emailAddress"],
        "One@example.com",
        "nothing changed"
    );
    let run = cli.run(&["switch", "3", "--json"]);
    assert_eq!(run.json()["error"]["type"], "LockError");
    std::fs::remove_dir(&stale).unwrap();
}

#[test]
fn auto_once_never_refreshes_or_targets_the_live_claude_login() {
    let mock = UsageMock::start();
    let cli = Cli::new().with_mock(&mock);
    // The healthy Codex account must never become a Claude candidate.
    cli.add_scripted("alice@example.com", "acct-alice", usage_mock::COOL);
    // The live Claude login expires within the 5-minute refresh buffer.
    let refresh = usage_mock::claude_live_refresh_token("two@example.com");
    let mut creds = claude_creds(&refresh, usage_mock::CLAUDE_HOT);
    creds["claudeAiOauth"]["expiresAt"] = json!(ccsw::model::now_unix() * 1000 + 60_000);
    cli.write_claude_live_with(&creds, &claude_config("two@example.com", "org-2", ""));
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let claude_slot = 2;
    let slot_before = cli.credential(claude_slot);
    let live_before = cli.claude_credentials();
    assert_eq!(cli.roster()["activeByProvider"]["claude"], claude_slot);

    let run = cli.run(&["auto", "--once", "--json"]);
    // The hot Claude login has no other Claude account to switch to.
    assert_eq!(run.status, 3, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("\"reason\":\"no-candidates\""),
        "{}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("\"event\":\"switch\""),
        "no switch to another provider: {}",
        run.stdout
    );
    assert_eq!(mock.claude_token_calls(), 0, "{:?}", mock.trail());
    assert_eq!(
        cli.credential(claude_slot)["claudeAiOauth"]["refreshToken"],
        slot_before["claudeAiOauth"]["refreshToken"]
    );
    assert_eq!(cli.claude_credentials(), live_before);
    assert_eq!(cli.roster()["activeByProvider"]["claude"], claude_slot);
}

#[test]
fn the_codex_credential_store_gate_applies_only_once_codex_is_in_use() {
    let cli = Cli::new();
    cli.add_claude("one@example.com", "org-1", "", "crt-1");
    let config = cli.codex_home.join("config.toml");
    std::fs::write(&config, "cli_auth_credentials_store = \"keyring\"\n").unwrap();
    let gate = "ccsw requires file-based Codex credentials";

    // Claude only, no auth.json: every command runs.
    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stdout.contains("one@example.com"), "{}", run.stdout);
    std::fs::write(&config, "not = [valid toml\n").unwrap();
    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0, "{}", run.stderr);

    // Adding a Codex account still meets the gate.
    std::fs::write(&config, "cli_auth_credentials_store = \"keyring\"\n").unwrap();
    let run = cli.run(&["add", "codex"]);
    assert_ne!(run.status, 0);
    assert!(run.stderr.contains(gate), "{}", run.stderr);
    let run = cli.run(&["add-token", "sk-openai"]);
    assert_ne!(run.status, 0);
    assert!(run.stderr.contains(gate), "{}", run.stderr);

    // With a Codex record in the roster the gate is back for every command.
    std::fs::remove_file(&config).unwrap();
    let run = cli.run(&["add-token", "sk-openai"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    std::fs::write(&config, "cli_auth_credentials_store = \"keyring\"\n").unwrap();
    let run = cli.run(&["list"]);
    assert_ne!(run.status, 0);
    assert!(run.stderr.contains(gate), "{}", run.stderr);
}

#[test]
fn auto_is_refused_on_a_codex_only_store_and_for_codex() {
    let mock = UsageMock::start();
    let cli = Cli::new().with_mock(&mock);
    let notice = "Auto-switch covers Claude Code accounts only: Codex sessions do not pick up a switched account without a restart. Add a Claude Code account with 'ccsw add claude' first.";
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");

    let run = cli.run(&["auto", "--once"]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains(notice), "{}", run.stderr);

    // With a Claude account, `auto codex` is still refused and `auto claude` runs.
    cli.write_claude_live_with(
        &claude_creds(
            &usage_mock::claude_live_refresh_token("one@example.com"),
            CLAUDE_OK,
        ),
        &claude_config("one@example.com", "org-1", ""),
    );
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let run = cli.run(&["auto", "codex", "--once"]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stderr.contains(notice), "{}", run.stderr);
    let run = cli.run(&["auto", "claude", "--once", "--json"]);
    assert_eq!(run.status, 2, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("\"reason\":\"below-threshold\""),
        "{}",
        run.stdout
    );
}

#[test]
fn auto_on_a_fresh_store_explains_which_account_to_add() {
    let cli = Cli::new();
    for args in [
        vec!["auto", "--once"],
        vec!["auto", "claude", "--once", "--json"],
    ] {
        let run = cli.run(&args);
        assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
        assert!(run.stdout.is_empty(), "{}", run.stdout);
        assert_eq!(
            run.stderr.trim(),
            format!("Error: {}", ccsw::autoswitch::CLAUDE_ONLY_NOTICE)
        );
    }
}
