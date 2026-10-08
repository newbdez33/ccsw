//! `list` / `ls` / `status`, human and `--json`, including the first-run flow.

mod support;

use serde_json::json;
use support::{Cli, api_key_auth, chatgpt_auth};

#[test]
fn first_run_json_never_prompts() {
    let cli = Cli::new();
    let run = cli.run(&["list", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.json(),
        json!({"schemaVersion": 2, "activeAccountNumber": null, "active": {"codex": null, "claude": null}, "accounts": []})
    );
    assert!(
        !cli.ccsw_home.exists(),
        "a read-only command creates nothing"
    );
}

#[test]
fn first_run_human_offers_to_add_the_live_login() {
    let cli = Cli::new();
    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "No accounts are managed yet.\nNo active Codex or Claude login found. Log in first.\n"
    );

    cli.write_live(&chatgpt_auth("alice@example.com", "acct-alice", "rt-a"));
    let run = cli.run_with_stdin(&["list"], "n\n");
    assert_eq!(
        run.stdout,
        "No accounts are managed yet.\nNo managed accounts found. Add current account (alice@example.com) to managed list? [Y/n] Setup cancelled. You can run 'ccsw add' later.\n"
    );
    assert!(!cli.ccsw_home.join("sequence.json").exists());

    let run = cli.run_with_stdin(&["ls"], "\n");
    assert_eq!(run.status, 0);
    assert!(
        run.stdout
            .ends_with("[Y/n] Added Account 1: alice@example.com [Plus]\n")
    );
    assert_eq!(cli.roster()["activeAccountNumber"], 1);
}

#[test]
fn list_human_rows_and_usage_lines() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "acct-team", "rt-b");
    cli.run(&["add-token", "sk-key"]);
    cli.run(&["alias", "1", "dev"]);
    cli.run(&["disable", "3"]);
    // bob is the live login (added last); alice was re-selected via the store.
    cli.write_live(&chatgpt_auth("alice@example.com", "acct-alice", "rt-a"));

    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.lines(),
        [
            "Accounts:",
            "  1: dev (alice@example.com) [Plus] (active)",
            "     usage unavailable (network)",
            "",
            "  2: bob@example.com [Acme]",
            "     usage unavailable (network)",
            "",
            "  3: api-key-3@token.local [personal] (disabled)",
            "     API key (no quota)",
        ]
    );

    let run = cli.run(&["list", "--token-status"]);
    assert_eq!(run.status, 0);
    let lines = run.lines();
    assert!(
        lines[3].starts_with("     • active profile: fresh, refresh token yes, expires "),
        "{}",
        lines[3]
    );
    assert!(lines[4].starts_with("     • stored backup: fresh, refresh token yes, expires "));
    assert!(
        lines
            .iter()
            .all(|l| !l.contains("api-key-3") || !l.contains("•"))
    );
}

#[test]
fn list_json_rows() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "acct-team", "rt-b");
    cli.run(&["add-token", "sk-key", "--email", "key@example.com"]);
    cli.run(&["alias", "2", "work"]);
    cli.run(&["disable", "1"]);

    let run = cli.run(&["list", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "");
    let payload = run.json();
    assert_eq!(payload["schemaVersion"], 2);
    assert_eq!(payload["accounts"][0]["provider"], "codex");
    assert_eq!(payload["activeAccountNumber"], 2);
    let rows = payload["accounts"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let alice = &rows[0];
    assert_eq!(alice["number"], 1);
    assert_eq!(alice["email"], "alice@example.com");
    assert_eq!(alice["organizationUuid"], "acct-alice");
    assert_eq!(alice["accountId"], "acct-alice");
    assert_eq!(alice["isOrganization"], true);
    assert_eq!(alice["planType"], "plus");
    assert_eq!(alice["active"], false);
    assert_eq!(alice["disabled"], true);
    assert_eq!(alice["usageStatus"], "unavailable");
    assert!(alice["usage"].is_null());
    assert!(alice.get("alias").is_none());
    let bob = &rows[1];
    assert_eq!(bob["active"], true);
    assert_eq!(bob["alias"], "work");
    assert_eq!(bob["organizationName"], "Acme");
    assert!(bob.get("disabled").is_none());
    let key = &rows[2];
    assert_eq!(key["email"], "key@example.com");
    assert_eq!(key["usageStatus"], "api_key");
    assert_eq!(key["isOrganization"], false);
    assert!(key["planType"].is_null());
}

#[test]
fn status_human_and_json() {
    let cli = Cli::new();
    let run = cli.run(&["status"]);
    assert_eq!(run.stdout, "Status: No active Codex or Claude account\n");
    let run = cli.run(&["status", "--json"]);
    assert_eq!(
        run.json(),
        json!({"schemaVersion": 2, "active": {"codex": null, "claude": null}, "totalManagedAccounts": 0})
    );

    cli.write_live(&chatgpt_auth("carol@example.com", "acct-carol", "rt-c"));
    let run = cli.run(&["status"]);
    assert_eq!(run.stdout, "Status: carol@example.com (not managed)\n");
    let run = cli.run(&["--status", "--json"]);
    assert_eq!(
        run.json(),
        json!({"schemaVersion": 2, "active": {"codex": {"email": "carol@example.com", "managed": false}, "claude": null}, "totalManagedAccounts": 0})
    );

    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "acct-team", "rt-b");
    let run = cli.run(&["status"]);
    assert_eq!(
        run.stdout,
        "Status: Account-2 (bob@example.com [Acme])\n  Total managed accounts: 2\n  usage unavailable (network)\n"
    );
    let run = cli.run(&["status", "--json"]);
    let payload = run.json();
    assert_eq!(payload["active"]["codex"]["number"], 2);
    assert_eq!(payload["active"]["codex"]["managed"], true);
    assert!(payload["active"].get("active").is_none());
    assert_eq!(payload["active"]["codex"]["usageStatus"], "unavailable");
    assert_eq!(payload["totalManagedAccounts"], 2);

    cli.write_live(&api_key_auth("sk-unknown"));
    let run = cli.run(&["status"]);
    assert_eq!(run.stdout, "Status: API key (not managed)\n");
}
