//! `add`, `add-token`, `remove`, `disable`/`enable`, `alias`, `move`, `swap`,
//! `purge`, legacy flags, validation, help and version.

mod support;

use serde_json::json;
use support::{Cli, api_key_auth, chatgpt_auth};

#[test]
fn add_first_second_and_refresh_in_place() {
    let cli = Cli::new();
    let run = cli.run(&["add"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "Error: No active Codex or Claude login found. Log in first."
    );

    let run = cli.add_chatgpt("Alice@Example.com", "acct-alice", "rt-a1");
    assert_eq!(run.stdout, "Added Account 1: alice@example.com [Plus]\n");
    let roster = cli.roster();
    assert_eq!(roster["activeAccountNumber"], 1);
    assert_eq!(roster["sequence"], json!([1]));
    let record = &roster["accounts"]["1"];
    assert_eq!(record["email"], "alice@example.com");
    assert_eq!(record["organizationUuid"], "acct-alice");
    assert_eq!(record["uuid"], "user-alice@example.com");
    assert_eq!(record["planType"], "plus");
    assert_eq!(cli.credential(1)["tokens"]["refresh_token"], "rt-a1");

    let run = cli.add_chatgpt("bob@example.com", "acct-team", "rt-b1");
    assert_eq!(run.stdout, "Added Account 2: bob@example.com [Acme]\n");
    assert_eq!(cli.roster()["accounts"]["2"]["organizationName"], "Acme");
    assert_eq!(cli.roster()["activeAccountNumber"], 2);

    let run = cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a2");
    assert_eq!(
        run.stdout,
        "Updated credentials for Account 1 (alice@example.com [Plus]).\n"
    );
    assert_eq!(cli.credential(1)["tokens"]["refresh_token"], "rt-a2");
    assert_eq!(cli.roster()["activeAccountNumber"], 1);
    assert_eq!(cli.roster()["sequence"], json!([1, 2]));
}

#[test]
fn add_with_slot_prompts_before_overwriting() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    cli.write_live(&chatgpt_auth("carol@example.com", "acct-carol", "rt-c"));

    let run = cli.run_with_stdin(&["add", "--slot", "1"], "n\n");
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Slot 1 already occupied\nalice@example.com [Plus]\nOverwrite slot 1? [y/N] Cancelled\n"
    );
    assert_eq!(cli.roster()["accounts"]["1"]["email"], "alice@example.com");

    let run = cli.run_with_stdin(&["add", "--slot", "1", "--alias", "Work"], "y\n");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .ends_with("Overwrite slot 1? [y/N] Added Account 1: carol@example.com [Plus]\n"),
        "{}",
        run.stdout
    );
    let record = &cli.roster()["accounts"]["1"];
    assert_eq!(record["email"], "carol@example.com");
    assert_eq!(record["alias"], "work");
    assert_eq!(cli.credential(1)["tokens"]["refresh_token"], "rt-c");

    // The same identity moved to another slot keeps its alias.
    let run = cli.run(&["add", "--slot", "4"]);
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Moved from slot 1 → 4\nAdded Account 4: carol@example.com [Plus]\n"
    );
    let roster = cli.roster();
    assert!(roster["accounts"].get("1").is_none());
    assert_eq!(roster["accounts"]["4"]["alias"], "work");
    assert_eq!(roster["sequence"], json!([4]));
    assert!(!cli.credential_path(1).exists());

    let run = cli.run(&["add", "--slot", "0"]);
    assert_eq!(run.status, 1);
    assert_eq!(run.stderr.trim(), "Error: Slot number must be >= 1");
}

#[test]
fn add_captures_a_live_api_key() {
    let cli = Cli::new();
    cli.write_live(&api_key_auth("sk-live-key"));
    let run = cli.run(&["add"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "Added Account 1: api-key-1@token.local [personal] (from API key)\n"
    );
    assert_eq!(cli.roster()["accounts"]["1"]["kind"], "api_key");
    assert_eq!(cli.credential(1)["OPENAI_API_KEY"], "sk-live-key");
    let run = cli.run(&["add"]);
    assert_eq!(
        run.stdout,
        "Updated credentials for Account 1 (api-key-1@token.local [personal]).\n"
    );
}

#[test]
fn add_token_registers_api_keys() {
    let cli = Cli::new();
    let run = cli.run(&["add-token", "sk-one", "--email", "me@example.com"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "Added Account 1: me@example.com [personal] (from API key)\n"
    );
    assert_eq!(cli.credential(1), api_key_auth("sk-one"));
    assert_eq!(cli.roster()["accounts"]["1"]["kind"], "api_key");

    let run = cli.run_with_stdin(&["add-token", "-"], "sk-two\n");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "Added Account 2: api-key-2@token.local [personal] (from API key)\n"
    );
    assert_eq!(cli.credential(2)["OPENAI_API_KEY"], "sk-two");

    let run = cli.run_with_stdin(&["add-token"], "sk-three\n");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .contains("Added Account 3: api-key-3@token.local")
    );

    let run = cli.run(&["add-token", "sk-one-b", "--email", "me@example.com"]);
    assert_eq!(
        run.stdout,
        "Updated API key for Account 1 (me@example.com [personal]).\n"
    );
    assert_eq!(cli.credential(1)["OPENAI_API_KEY"], "sk-one-b");

    let run = cli.run(&["add-token", " ", "--slot", "9"]);
    assert_eq!(run.status, 1);
    assert_eq!(run.stderr.trim(), "Error: Token cannot be empty");
    let run = cli.run(&["add-token", "sk-x", "--email", "not-an-email"]);
    assert_eq!(
        run.stderr.trim(),
        "Error: Invalid email format: not-an-email"
    );
    let run = cli.run(&["--add-token", "sk-legacy", "--slot", "7"]);
    assert_eq!(run.status, 0);
    assert!(
        run.stdout
            .starts_with("Added Account 7: api-key-7@token.local")
    );
}

#[test]
fn alias_move_swap_disable_enable_remove() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "acct-bob", "rt-b");

    let run = cli.run(&["alias", "1", "Dev"]);
    assert_eq!(run.stdout, "Set alias 'dev' for Account 1\n");
    let run = cli.run(&["alias"]);
    assert_eq!(run.stdout, "Aliases:\n  1: dev (alice@example.com)\n");
    let run = cli.run(&["alias", "2", "dev"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "Error: Alias 'dev' is already used by account 1"
    );
    let run = cli.run(&["alias", "--unset"]);
    assert_eq!(run.status, 2);
    assert!(
        run.stderr
            .contains("cswitch alias: error: NUM|EMAIL is required with --unset")
    );
    let run = cli.run(&["alias", "1"]);
    assert_eq!(run.status, 2);
    assert!(
        run.stderr
            .contains("NAME is required (or pass --unset to remove the alias)")
    );
    let run = cli.run(&["alias", "dev", "--unset"]);
    assert_eq!(run.stdout, "Removed alias for Account 1\n");
    let run = cli.run(&["alias"]);
    assert_eq!(run.stdout, "No aliases set\n");

    let run = cli.run(&["move", "1", "5"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "Moved alice@example.com to slot 5\n");
    assert!(cli.credential_path(5).exists() && !cli.credential_path(1).exists());
    assert_eq!(cli.roster()["sequence"], json!([2, 5]));
    let run = cli.run(&["swap", "5", "2"]);
    assert_eq!(
        run.stdout,
        "Swapped Account 2 and Account 5:\n  2: alice@example.com\n  5: bob@example.com\n"
    );
    assert_eq!(cli.credential(2)["tokens"]["refresh_token"], "rt-a");
    assert_eq!(
        cli.roster()["activeAccountNumber"],
        5,
        "the active marker follows bob"
    );
    let run = cli.run(&["move", "2", "2"]);
    assert_eq!(run.stdout, "Already in slot 2: alice@example.com\n");
    let run = cli.run(&["move", "2"]);
    assert_eq!(run.status, 2);
    assert!(
        run.stderr
            .contains("the following arguments are required: SLOT")
    );
    let run = cli.run(&["swap", "2", "2"]);
    assert_eq!(
        run.stderr.trim(),
        "Error: Cannot swap an account with itself"
    );

    // bob (slot 5) is the live login.
    let run = cli.run(&["disable", "5"]);
    assert_eq!(
        run.stdout,
        "Disabled Account-5 (bob@example.com).\n  It is the active account — it stays live until you switch away; it just won't be an automatic switch target.\n"
    );
    let run = cli.run(&["disable", "bob@example.com"]);
    assert_eq!(
        run.stdout,
        "Account-5 (bob@example.com) is already disabled.\n"
    );
    let run = cli.run(&["disable", "2"]);
    assert!(run.stdout.ends_with(
        "  No accounts remain in rotation — auto-switch and bare switch have nothing to pick. Re-enable one with cswitch enable <num|email>.\n"
    ));
    let run = cli.run(&["enable", "2"]);
    assert_eq!(
        run.stdout,
        "Enabled Account-2 (alice@example.com).\n  It is back in the rotation.\n"
    );
    let run = cli.run(&["--enable-account", "5"]);
    assert_eq!(run.status, 0);
    assert!(cli.roster()["accounts"]["5"].get("disabled").is_none());
    let run = cli.run(&["disable", "9"]);
    assert_eq!(run.stderr.trim(), "Error: Account-9 does not exist");

    let run = cli.run_with_stdin(&["rm", "2"], "n\n");
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Are you sure you want to permanently remove Account-2 (alice@example.com)? [y/N] Cancelled\n"
    );
    let run = cli.run_with_stdin(&["remove", "bob@example.com"], "y\n");
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Warning: Account-5 (bob@example.com) is currently active\nAre you sure you want to permanently remove Account-5 (bob@example.com)? [y/N] Removed Account-5 (bob@example.com)\n"
    );
    assert!(cli.roster()["accounts"].get("5").is_none());
    assert!(!cli.credential_path(5).exists());
    let run = cli.run(&["remove", "bogus"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "Error: Invalid account identifier: bogus"
    );
    let run = cli.run(&["remove", "9"]);
    assert_eq!(run.stderr.trim(), "Error: Account-9 does not exist");
    let run = cli.run(&["remove", "nobody@example.com"]);
    assert_eq!(
        run.stderr.trim(),
        "Error: No account found with identifier: nobody@example.com"
    );
}

#[test]
fn purge_removes_the_store_after_confirmation() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    let run = cli.run_with_stdin(&["purge"], "\n");
    assert_eq!(run.status, 0);
    assert!(
        run.stdout
            .starts_with("This will remove ALL cswitch data from your system:\n")
    );
    assert!(
        run.stdout
            .contains("Note: This does NOT affect your current Codex login.\n")
    );
    assert!(
        run.stdout
            .ends_with("Are you sure you want to purge all data? [y/N] Cancelled\n")
    );
    assert!(cli.cswitch_home.exists());
    let run = cli.run_with_stdin(&["--purge"], "y\n");
    assert_eq!(run.status, 0);
    assert!(run.stdout.ends_with("Purge complete.\n"));
    assert!(!cli.cswitch_home.exists());
    assert!(cli.live_path().exists(), "the Codex login is untouched");
}

#[test]
fn usage_errors_exit_2_with_the_usage_line() {
    let cli = Cli::new();
    let cases = [
        (&[][..], "no command given — try 'cswitch help'"),
        (&["--json"], "no command given — try 'cswitch help'"),
        (&["bogus"], "unrecognized arguments: bogus"),
        (
            &["list", "--strategy", "best"],
            "--strategy can only be used with bare 'switch'",
        ),
        (
            &["switch", "2", "--strategy", "best"],
            "--strategy can only be used with bare 'switch'",
        ),
        (
            &["switch", "--model", "x"],
            "--model can only be used with 'switch --strategy best' or 'switch --strategy next-available'",
        ),
        (
            &["status", "--token-status"],
            "--token-status can only be used with 'list'",
        ),
        (
            &["purge", "--json"],
            "--json can only be used with 'list', 'status', or 'switch'",
        ),
        (
            &["list", "--json", "--token-status"],
            "--token-status cannot be combined with --json",
        ),
        (
            &["list", "--slot", "2"],
            "--slot can only be used with 'add' or 'add-token'",
        ),
        (
            &["add", "--email", "a@b.co"],
            "--email can only be used with 'add-token'",
        ),
        (
            &["list", "--account", "1"],
            "--account can only be used with 'export'",
        ),
        (
            &["add-token", "x", "--alias", "a"],
            "--alias can only be used with 'add'",
        ),
        (
            &["switch", "--force"],
            "--force can only be used with 'import' or 'switch <num|email>'",
        ),
        (
            &["import", "f", "--full"],
            "--full can only be used with 'export'",
        ),
        (
            &["switch", "--strategy", "bogus"],
            "argument --strategy: invalid choice: 'bogus' (choose from 'best', 'next-available')",
        ),
        (
            &["--list", "--switch"],
            "argument --switch: not allowed with argument --list",
        ),
        (
            &["add", "--slot", "x"],
            "argument --slot: invalid int value: 'x'",
        ),
    ];
    for (args, message) in cases {
        let run = cli.run(args);
        assert_eq!(run.status, 2, "{args:?}: {}", run.stderr);
        assert_eq!(
            run.stderr,
            format!("usage: cswitch <command> [args] [options]\ncswitch: error: {message}\n"),
            "{args:?}"
        );
        assert!(run.stdout.is_empty());
    }
}

#[test]
fn help_version_menubar_and_upgrade() {
    let cli = Cli::new();
    for args in [&["help"][..], &["--help"], &["-h"]] {
        let run = cli.run(args);
        assert_eq!(run.status, 0);
        assert!(
            run.stdout
                .starts_with("usage: cswitch <command> [args] [options]\n")
        );
        assert!(
            run.stdout
                .contains("Multi-Account Switcher for OpenAI Codex and Claude Code")
        );
        assert!(
            run.stdout
                .contains("Aliases: ls=list  rm=remove  update=upgrade")
        );
        assert!(run.stdout.contains(
            "The original flag spellings (cswitch --switch, cswitch --list, ...) keep working."
        ));
    }
    let run = cli.run(&["--version"]);
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        format!("cswitch {}\n", env!("CARGO_PKG_VERSION"))
    );
    let run = cli.run(&["menubar"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "The menu bar is not available in cswitch."
    );
    for args in [&["upgrade"][..], &["update"], &["--upgrade"]] {
        let run = cli.run(args);
        assert_eq!(run.status, 1);
        assert!(
            run.stderr
                .contains("cargo install --git https://github.com/newbdez33/cswitch --locked")
        );
    }
    let run = cli.run(&["alias", "-h"]);
    assert_eq!(run.status, 0);
    assert!(run.stdout.starts_with("usage: cswitch alias"));
}

#[test]
fn errors_use_the_json_envelope_in_json_mode() {
    let cli = Cli::new();
    let run = cli.run(&["switch", "2", "--json"]);
    assert_eq!(run.status, 1);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.json(),
        json!({"schemaVersion": 2, "error": {"type": "ConfigError", "message": "No accounts are managed yet"}})
    );
    let run = cli.run(&["switch", "2"]);
    assert_eq!(run.status, 1);
    assert_eq!(run.stderr, "Error: No accounts are managed yet\n");
    assert_eq!(run.stdout, "");

    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    let run = cli.run(&["--switch-to", "nobody", "--json"]);
    assert_eq!(run.status, 1);
    let envelope = run.json();
    assert_eq!(envelope["error"]["type"], "ValidationError");
    assert_eq!(
        envelope["error"]["message"],
        "Invalid account identifier: nobody"
    );
}

#[test]
fn credential_store_gate_refuses_keyring_mode() {
    let cli = Cli::new();
    // The gate applies once Codex is in use (a Codex record or a live auth.json).
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    std::fs::write(
        cli.codex_home.join("config.toml"),
        "cli_auth_credentials_store = \"keyring\"\n",
    )
    .unwrap();
    let run = cli.run(&["list"]);
    assert_eq!(run.status, 1);
    assert!(
        run.stderr
            .contains("cswitch requires file-based Codex credentials")
    );
}
