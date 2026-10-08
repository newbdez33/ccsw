//! `switch`, `switch <id>`, `--force`, `--strategy`, the daemon restart and
//! the live-file backup.

mod support;

use serde_json::json;
use support::{Cli, chatgpt_auth, chatgpt_auth_at};

const NOT_RUNNING_FOLLOWUP: &str = "New account is active for the next Codex session — restart any running codex exec / --no-daemon session.";

fn two_accounts() -> Cli {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "acct-bob", "rt-b");
    cli.clear_codex_calls();
    cli
}

#[test]
fn direct_switch_replaces_the_live_file_and_backs_it_up() {
    let cli = two_accounts();
    let run = cli.run(&["switch", "1"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines[0], "Switched to Account-1 (alice@example.com)");
    assert_eq!(lines[1], "Accounts:");
    assert_eq!(lines[2], "  1: alice@example.com [Plus] (active)");
    assert_eq!(lines[5], "  2: bob@example.com [Plus]");
    assert_eq!(lines[lines.len() - 2], NOT_RUNNING_FOLLOWUP);
    assert_eq!(lines[lines.len() - 3], "");
    assert!(run.stdout.ends_with("\n\n"));

    assert_eq!(cli.live(), cli.credential(1));
    assert_eq!(cli.roster()["activeAccountNumber"], 1);
    let backups = cli.live_backups();
    assert_eq!(backups.len(), 1, "{backups:?}");
    let backup = support::read_json(&cli.codex_home.join(&backups[0]));
    assert_eq!(backup["tokens"]["refresh_token"], "rt-b");
    assert_eq!(
        cli.codex_calls(),
        ["app-server daemon version"],
        "a stopped daemon is never restarted"
    );
}

#[test]
fn running_daemon_is_restarted_and_unchanged_file_is_not() {
    let mut cli = two_accounts();
    cli.daemon_running = true;
    let run = cli.run(&["switch", "alice@example.com"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        cli.codex_calls(),
        ["app-server daemon version", "app-server daemon restart"]
    );
    assert!(run.stdout.contains(
        "\nRestarted the Codex app-server daemon — new and reconnecting Codex sessions use Account-1.\n"
    ));
    cli.clear_codex_calls();

    let run = cli.run(&["switch", "1"]);
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Already on Account-1 (alice@example.com)\nTo rewrite the live login from the stored backup (e.g. after --import), run: ccsw switch 1 --force\n"
    );
    assert!(cli.codex_calls().is_empty());

    let run = cli.run(&["switch", "1", "--force"]);
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Activated Account-1 (alice@example.com) from stored backup\n"
    );
    assert!(
        cli.codex_calls().is_empty(),
        "identical bytes need no restart"
    );

    let run = cli.run(&["switch", "1", "--force", "--json"]);
    let payload = run.json();
    assert_eq!(payload["switched"], false);
    assert_eq!(payload["reason"], "activated");
    assert_eq!(payload["strategy"], "direct");
    assert_eq!(payload["from"], payload["to"]);
}

#[test]
fn switch_json_payload_and_rotation() {
    let cli = two_accounts();
    let run = cli.run(&["switch", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    // JSON mode: stdout is the one document and nothing reaches stderr.
    assert_eq!(run.stderr, "");
    let payload = run.json();
    assert_eq!(payload["schemaVersion"], 2);
    assert_eq!(payload["switched"], true);
    assert_eq!(
        payload["from"],
        json!({"number": 2, "email": "bob@example.com", "provider": "codex"})
    );
    assert_eq!(
        payload["to"],
        json!({"number": 1, "email": "alice@example.com", "provider": "codex"})
    );
    assert_eq!(payload["strategy"], "rotation");
    assert_eq!(payload["reason"], "switched");
    assert_eq!(
        payload["message"],
        "Switched to Account-1 (alice@example.com)"
    );
    assert_eq!(payload["warnings"], json!([]));
    assert!(payload.get("models").is_none());
    assert_eq!(cli.live()["tokens"]["refresh_token"], "rt-a");

    let run = cli.run(&["--switch"]);
    assert_eq!(run.status, 0);
    assert!(
        run.stdout
            .starts_with("Switched to Account-2 (bob@example.com)\nAccounts:\n")
    );

    cli.add_chatgpt("carol@example.com", "acct-carol", "rt-c");
    cli.run(&["disable", "1"]);
    // Live is carol (3); rotation wraps past the disabled slot 1 to 2.
    let run = cli.run(&["switch"]);
    assert_eq!(run.status, 0);
    assert!(
        run.stdout.starts_with(
            "Skipping Account-1 (disabled)\nSwitched to Account-2 (bob@example.com)\n"
        )
    );
    let run = cli.run(&["switch", "--json"]);
    let payload = run.json();
    assert_eq!(payload["to"]["number"], 3);
    assert_eq!(payload["warnings"], json!([]));
    let run = cli.run(&["switch", "--json"]);
    assert_eq!(
        run.json()["warnings"],
        json!(["Skipped Account-1 (disabled)"])
    );
}

#[test]
fn rotation_no_ops() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    let run = cli.run(&["switch"]);
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Only one account is managed. Add more accounts to switch between.\n"
    );
    let run = cli.run(&["switch", "--json"]);
    let payload = run.json();
    assert_eq!(payload["reason"], "only-one-account");
    assert_eq!(
        payload["from"],
        json!({"number": 1, "email": "alice@example.com", "provider": "codex"})
    );
    assert_eq!(payload["from"], payload["to"]);

    cli.add_chatgpt("bob@example.com", "acct-bob", "rt-b");
    cli.run(&["disable", "1"]);
    let run = cli.run(&["switch"]);
    assert_eq!(
        run.stdout,
        "Skipping Account-1 (disabled)\nNo other accounts have valid stored credentials.\nRe-add a skipped slot with: ccsw add --slot <number>\n"
    );
    let run = cli.run(&["switch", "--json"]);
    let payload = run.json();
    assert_eq!(payload["reason"], "no-valid-target");
    assert_eq!(
        payload["message"],
        "No other accounts have valid stored credentials."
    );
    assert_eq!(payload["warnings"], json!(["Skipped Account-1 (disabled)"]));

    cli.run(&["enable", "1"]);
    std::fs::remove_file(cli.credential_path(1)).unwrap();
    let run = cli.run(&["switch"]);
    assert!(run.stdout.starts_with(
        "Skipping Account-1 (no stored credentials, re-add with ccsw add --slot 1)\n"
    ));
    let run = cli.run(&["switch", "1"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "Error: Account-1 has no stored credentials. Re-add with: ccsw add --slot 1"
    );
}

#[test]
fn strategies_without_usage() {
    let cli = two_accounts();
    let run = cli.run(&["switch", "--strategy", "best"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "Current account usage is unavailable — staying on Account-2. Run ccsw switch to rotate.\n"
    );
    let run = cli.run(&[
        "switch",
        "--strategy",
        "best",
        "--model",
        "Spark,spark,all",
        "--json",
    ]);
    let payload = run.json();
    assert_eq!(payload["reason"], "usage-unavailable");
    assert_eq!(payload["strategy"], "best");
    assert_eq!(payload["models"], json!(["Spark", "all"]));
    assert_eq!(payload["modelSource"], "cli");
    assert_eq!(payload["from"], payload["to"]);

    let run = cli.run(&["switch", "--strategy", "next-available"]);
    assert_eq!(run.status, 0);
    assert!(
        run.stdout
            .starts_with("Switched to Account-1 (alice@example.com)\n")
    );
    let run = cli.run(&["switch", "--strategy", "next-available", "--json"]);
    let payload = run.json();
    assert_eq!(payload["strategy"], "next-available");
    assert_eq!(payload["to"]["number"], 2);
}

#[test]
fn unmanaged_and_missing_live_logins() {
    let cli = two_accounts();
    cli.write_live(&chatgpt_auth("carol@example.com", "acct-carol", "rt-c"));
    let run = cli.run(&["switch", "--json"]);
    let payload = run.json();
    assert_eq!(payload["switched"], false);
    assert_eq!(payload["reason"], "unmanaged-account");
    assert_eq!(
        payload["from"],
        json!({"number": null, "email": "carol@example.com", "provider": "codex"})
    );
    assert_eq!(
        payload["message"],
        "Active account is not managed; run ccsw add"
    );
    assert!(
        cli.roster()["accounts"].get("3").is_none(),
        "JSON mode never auto-adds"
    );

    let run = cli.run(&["switch", "1"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stdout.starts_with(
        "The live login does not match a managed account; it was left in place.\nActivated Account-1 (alice@example.com)\n"
    ));
    assert!(
        !run.stdout.contains("Accounts:"),
        "direct activation prints no list"
    );
    assert_eq!(cli.live()["tokens"]["refresh_token"], "rt-a");
    assert_eq!(
        cli.credential(2)["tokens"]["refresh_token"],
        "rt-b",
        "nothing was folded into a managed slot"
    );

    cli.write_live(&chatgpt_auth("carol@example.com", "acct-carol", "rt-c"));
    let run = cli.run(&["switch"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "Notice: Active account 'carol@example.com' was not managed.\nAdded Account 3: carol@example.com [Plus]\nIt has been automatically added as Account-3.\nPlease run the switch command again to switch to the next account.\n"
    );
    assert_eq!(cli.roster()["accounts"]["3"]["email"], "carol@example.com");

    cli.remove_live();
    let run = cli.run(&["switch", "--json"]);
    let payload = run.json();
    assert_eq!(payload["switched"], true);
    assert!(payload["from"].is_null());
    assert_eq!(
        payload["to"]["number"], 3,
        "the recorded active slot is activated"
    );
    assert_eq!(
        payload["message"],
        "Activated Account-3 (carol@example.com)"
    );
    assert_eq!(cli.live()["tokens"]["refresh_token"], "rt-c");
}

#[test]
fn newer_live_tokens_are_folded_back_before_switching() {
    let cli = two_accounts();
    // Codex rotated bob's tokens after the snapshot was taken.
    cli.write_live(&chatgpt_auth_at(
        "bob@example.com",
        "acct-bob",
        "rt-b-rotated",
        "2026-09-29T12:00:00Z",
    ));
    let run = cli.run(&["switch", "1"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(cli.credential(2)["tokens"]["refresh_token"], "rt-b-rotated");
    assert_eq!(cli.live()["tokens"]["refresh_token"], "rt-a");
    // An older live copy is not folded back.
    cli.write_live(&chatgpt_auth_at(
        "alice@example.com",
        "acct-alice",
        "rt-a-old",
        "2026-09-29T09:00:00Z",
    ));
    let run = cli.run(&["switch", "2"]);
    assert_eq!(run.status, 0);
    assert_eq!(cli.credential(1)["tokens"]["refresh_token"], "rt-a");
}

#[test]
fn identifiers_aliases_and_ambiguous_emails() {
    let cli = two_accounts();
    cli.run(&["alias", "1", "dev"]);
    let run = cli.run(&["switch", "DEV", "--json"]);
    assert_eq!(run.json()["to"]["number"], 1);
    let run = cli.run(&["switch", "nobody@example.com"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "Error: No account found with identifier: nobody@example.com"
    );
    let run = cli.run(&["switch", "not-valid"]);
    assert_eq!(
        run.stderr.trim(),
        "Error: Invalid account identifier: not-valid"
    );

    // The same email under a second workspace.
    cli.add_chatgpt("alice@example.com", "acct-team", "rt-a2");
    let run = cli.run_with_stdin(&["switch", "alice@example.com"], "x\n");
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Multiple accounts found for 'alice@example.com':\n  1: alice@example.com [Plus]\n  3: alice@example.com [Acme]\nEnter account number to switch to: Cancelled\n"
    );
    let run = cli.run_with_stdin(&["switch", "alice@example.com"], "1\n");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stdout.contains(
        "Enter account number to switch to: Switched to Account-1 (alice@example.com)\n"
    ));
    let run = cli.run(&["switch", "alice@example.com", "--json"]);
    assert_eq!(run.status, 1);
    let envelope = run.json();
    assert_eq!(envelope["error"]["type"], "ConfigError");
    assert_eq!(
        envelope["error"]["message"],
        "Email 'alice@example.com' is ambiguous — matches accounts: 1 [Plus], 3 [Acme]. Use account number instead (e.g., ccsw switch 1)."
    );
}
