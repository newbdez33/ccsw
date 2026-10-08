//! End to end through the built binary with real usage numbers: an axum mock
//! of the usage and token endpoints answers per bearer token, the fake `codex`
//! stands in for the CLI, and the store lives in a temp dir.

mod support;

use serde_json::{Value, json};
use support::usage_mock::{
    self, LIMIT_5H, LIMIT_7D, OK, POOL, POOL_NAME, REFRESHED, ROTATED_REFRESH, STALE, THROTTLED,
    UsageMock,
};
use support::{Cli, chatgpt_auth_with_tokens};

/// Make `email` (already added with `access`) the live login.
fn activate(cli: &Cli, email: &str, account_id: &str, access: &str) {
    cli.write_live(&chatgpt_auth_with_tokens(
        email,
        account_id,
        access,
        &usage_mock::live_refresh_token(email, account_id),
    ));
}

/// Alice (active), Bob, Carol with the given replies; the cache is warmed by
/// running `list` until every row has been fetched (one candidate per pass).
fn trio(mock: &UsageMock, alice: &str, bob: &str, carol: &str) -> Cli {
    let cli = Cli::new().with_mock(mock);
    cli.add_scripted("alice@example.com", "acct-alice", alice);
    cli.add_scripted("bob@example.com", "acct-bob", bob);
    cli.add_scripted("carol@example.com", "acct-carol", carol);
    activate(&cli, "alice@example.com", "acct-alice", alice);
    for _ in 0..2 {
        let run = cli.run(&["list"]);
        assert_eq!(run.status, 0, "{}", run.stderr);
    }
    cli.clear_codex_calls();
    cli
}

fn row(payload: &Value, number: u64) -> &Value {
    payload["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["number"] == number)
        .unwrap_or_else(|| panic!("no row {number} in {payload}"))
}

fn assert_usage_row(line: &str, prefix: &str) {
    assert!(line.starts_with(prefix), "{line:?} !~ {prefix:?}");
    assert!(
        line.contains("   resets ") && line.contains("  in "),
        "{line:?}"
    );
}

#[test]
fn list_renders_real_usage_rows_and_serves_the_cache() {
    let mock = UsageMock::start();
    let cli = Cli::new().with_mock(&mock);
    cli.add_scripted("alice@example.com", "acct-alice", OK);
    cli.add_scripted("bob@example.com", "acct-bob", LIMIT_5H);
    cli.add_scripted("carol@example.com", "acct-carol", POOL);
    cli.add_scripted("dave@example.com", "acct-dave", THROTTLED);
    activate(&cli, "alice@example.com", "acct-alice", OK);

    // One pass fetches the active account plus one due candidate, so the
    // fourth account is measured on the third pass.
    let mut last = None;
    for _ in 0..3 {
        let run = cli.run(&["list"]);
        assert_eq!(run.status, 0, "{}", run.stderr);
        last = Some(run);
    }
    let run = last.unwrap();
    let lines = run.lines();
    assert_eq!(lines[0], "Accounts:");
    assert_eq!(lines[1], "  1: alice@example.com [Plus] (active)");
    assert_usage_row(lines[2], "     ├ 5h:  35%");
    assert_usage_row(lines[3], "     ├ 7d:  60%");
    assert_eq!(lines[4], "     └ credits: $12.50");
    assert_eq!(lines[5], "");
    assert_eq!(lines[6], "  2: bob@example.com [Plus]");
    assert_usage_row(lines[7], "     ├ 5h: 100%");
    assert_usage_row(lines[8], "     └ 7d:  40%");
    assert_eq!(lines[10], "  3: carol@example.com [Plus]");
    assert_usage_row(lines[11], "     ├ 5h:                   10%");
    assert_usage_row(lines[12], "     ├ 7d:                   20%");
    assert_usage_row(lines[13], &format!("     └ {POOL_NAME}: 100%"));
    assert!(lines[13].ends_with("  (!)"), "{}", lines[13]);
    assert_eq!(lines[15], "  4: dave@example.com [Plus]");
    assert_eq!(lines[16], "     usage unavailable (http-429)");
    assert_eq!(lines.len(), 17, "{lines:?}");
    for bearer in [OK, LIMIT_5H, POOL, THROTTLED] {
        assert_eq!(mock.usage_calls(bearer), 1, "{bearer}");
    }

    // Within 180 s every row is served from cache/usage.json; the throttled
    // account sits out its Retry-After.
    let again = cli.run(&["list"]);
    assert_eq!(again.stdout, run.stdout);
    assert_eq!(mock.requests().len(), 4, "{:?}", mock.trail());

    let run = cli.run(&["list", "--json"]);
    assert_eq!(run.status, 0);
    assert_eq!(run.stderr, "");
    let payload = run.json();
    assert_eq!(payload["activeAccountNumber"], 1);
    let alice = row(&payload, 1);
    assert_eq!(alice["usageStatus"], "ok");
    assert_eq!(alice["usage"]["fiveHour"]["pct"], 35.0);
    assert_eq!(alice["usage"]["sevenDay"]["pct"], 60.0);
    assert!(alice["usage"]["sevenDay"]["resetsAt"].is_string());
    assert!(alice["usage"]["sevenDay"].get("expectedPct").is_some());
    assert_eq!(
        alice["usage"]["credits"],
        json!({"balance": 12.5, "unlimited": false})
    );
    assert!(alice["usage"].get("scoped").is_none());
    let fetched = alice["usageFetchedAt"].as_str().unwrap();
    assert!(fetched.ends_with('Z') && fetched.len() == 20, "{fetched}");
    assert!(alice["usageAgeSeconds"].as_f64().unwrap() < 60.0);
    let bob = row(&payload, 2);
    assert_eq!(bob["usage"]["fiveHour"]["pct"], 100.0);
    assert!(bob["usage"]["fiveHour"]["countdown"].is_string());
    let carol = row(&payload, 3);
    assert_eq!(carol["usage"]["scoped"][0]["name"], POOL_NAME);
    assert_eq!(carol["usage"]["scoped"][0]["pct"], 100.0);
    assert!(carol["usage"]["scoped"][0]["resetsAt"].is_string());
    let dave = row(&payload, 4);
    assert_eq!(dave["usageStatus"], "unavailable");
    assert!(dave["usage"].is_null());
    assert!(dave.get("usageFetchedAt").is_none());

    let run = cli.run(&["status", "--json"]);
    assert_eq!(run.status, 0);
    assert_eq!(run.stderr, "");
    let payload = run.json();
    assert_eq!(payload["active"]["codex"]["number"], 1);
    assert_eq!(payload["active"]["codex"]["managed"], true);
    assert_eq!(payload["active"]["codex"]["usageStatus"], "ok");
    assert_eq!(payload["active"]["codex"]["usage"]["fiveHour"]["pct"], 35.0);
    assert_eq!(payload["totalManagedAccounts"], 4);

    let run = cli.run(&["status"]);
    let lines = run.lines();
    assert_eq!(lines[0], "Status: Account-1 (alice@example.com [Plus])");
    assert_eq!(lines[1], "  Total managed accounts: 4");
    assert_usage_row(lines[2], "  ├ 5h:  35%");
    assert_eq!(lines[4], "  └ credits: $12.50");
    assert_eq!(mock.requests().len(), 4, "status served from the cache");
}

#[test]
fn best_and_next_available_use_real_headroom() {
    let mock = UsageMock::start();
    // Headroom: alice 40, bob 0 (at its 5h limit), carol 80 (pools do not count
    // unless named).
    let cli = trio(&mock, OK, LIMIT_5H, POOL);

    let run = cli.run(&["switch", "--strategy", "next-available"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stdout.starts_with(
            "Skipping Account-2 (at 5h limit)\nSwitched to Account-3 (carol@example.com)\nAccounts:\n"
        ),
        "{}",
        run.stdout
    );
    assert_eq!(cli.live()["tokens"]["access_token"], POOL);
    assert_eq!(cli.roster()["activeAccountNumber"], 3);

    let run = cli.run(&["switch", "1", "--json"]);
    assert_eq!(run.status, 0);
    assert_eq!(run.stderr, "", "JSON mode prints nothing to stderr");
    assert_eq!(run.json()["to"]["number"], 1);

    let run = cli.run(&["switch", "--strategy", "next-available", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "");
    let payload = run.json();
    assert_eq!(payload["switched"], true);
    assert_eq!(payload["strategy"], "next-available");
    assert_eq!(payload["reason"], "switched");
    assert_eq!(payload["to"]["number"], 3);
    assert_eq!(
        payload["warnings"],
        json!(["Skipped Account-2 (at 5h limit)"])
    );

    cli.run(&["switch", "1"]);
    let run = cli.run(&["switch", "--strategy", "best", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "");
    let payload = run.json();
    assert_eq!(payload["switched"], true);
    assert_eq!(payload["strategy"], "best");
    assert_eq!(
        payload["to"],
        json!({"number": 3, "email": "carol@example.com", "provider": "codex"})
    );
    assert_eq!(cli.live()["tokens"]["access_token"], POOL);

    let run = cli.run(&["switch", "--strategy", "best"]);
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Already on the account with the most remaining quota (Account-3).\n"
    );
    let run = cli.run(&["switch", "--strategy", "best", "--json"]);
    assert_eq!(run.json()["reason"], "already-best");

    // Naming the pool makes carol's exhausted model window count.
    let run = cli.run(&[
        "switch",
        "--strategy",
        "best",
        "--model",
        POOL_NAME,
        "--json",
    ]);
    let payload = run.json();
    assert_eq!(payload["switched"], true);
    assert_eq!(payload["to"]["number"], 1);
    assert_eq!(payload["models"], json!([POOL_NAME]));
    assert_eq!(payload["modelSource"], "cli");

    for bearer in [OK, LIMIT_5H, POOL] {
        assert_eq!(mock.usage_calls(bearer), 1, "every decision used the cache");
    }
}

#[test]
fn next_available_reports_exhaustion_when_everything_is_at_limit() {
    let mock = UsageMock::start();
    let cli = trio(&mock, OK, LIMIT_5H, LIMIT_7D);

    let run = cli.run(&["switch", "--strategy", "next-available"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "Skipping Account-2 (at 5h limit)\nSkipping Account-3 (at 7d limit)\nAll other accounts are at their 5h/7d limit — staying on Account-1.\n"
    );
    assert_eq!(cli.live()["tokens"]["access_token"], OK, "nothing changed");
    assert!(
        cli.codex_calls().is_empty(),
        "no daemon probe without a switch"
    );

    let run = cli.run(&["switch", "--strategy", "next-available", "--json"]);
    assert_eq!(run.status, 0);
    assert_eq!(run.stderr, "");
    let payload = run.json();
    assert_eq!(payload["switched"], false);
    assert_eq!(payload["reason"], "candidates-exhausted");
    assert_eq!(payload["from"], payload["to"]);
    assert_eq!(payload["from"]["number"], 1);
    assert_eq!(
        payload["warnings"],
        json!([
            "Skipped Account-2 (at 5h limit)",
            "Skipped Account-3 (at 7d limit)"
        ])
    );

    let run = cli.run(&["switch", "--strategy", "best", "--json"]);
    assert_eq!(run.json()["reason"], "already-best");
}

#[test]
fn unauthorized_then_refresh_rotates_the_slot_and_the_live_login() {
    let mock = UsageMock::start();
    let cli = Cli::new().with_mock(&mock);
    cli.add_scripted("erin@example.com", "acct-erin", STALE);
    cli.add_scripted("frank@example.com", "acct-frank", OK);
    activate(&cli, "erin@example.com", "acct-erin", STALE);
    let before = cli.credential(1);
    let presented = usage_mock::live_refresh_token("erin@example.com", "acct-erin");
    assert_eq!(before["tokens"]["refresh_token"], presented);
    assert_eq!(cli.live(), before);

    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines[1], "  1: erin@example.com [Plus] (active)");
    assert_usage_row(lines[2], "     ├ 5h:  50%");
    assert_usage_row(lines[6], "     ├ 5h:  35%");
    let erin_trail: Vec<String> = mock
        .trail()
        .into_iter()
        .filter(|step| step != &format!("usage:{OK}"))
        .collect();
    assert_eq!(
        erin_trail,
        [
            format!("usage:{STALE}"),
            format!("token:{presented}"),
            format!("usage:{REFRESHED}"),
        ]
    );

    let after = cli.credential(1);
    assert_eq!(after["tokens"]["access_token"], REFRESHED);
    assert_eq!(after["tokens"]["refresh_token"], ROTATED_REFRESH);
    assert_ne!(after["last_refresh"], before["last_refresh"]);
    assert_eq!(after["tokens"]["account_id"], "acct-erin");
    assert_eq!(
        cli.live()["tokens"],
        after["tokens"],
        "the active slot's rotation lands in auth.json too"
    );
    assert!(
        cli.ccsw_home
            .join("credentials")
            .join("1.json.prev")
            .exists(),
        "the previous generation is retained"
    );
    assert_eq!(cli.credential(2)["tokens"]["access_token"], OK);

    // The rotated bearer is what the next fetch presents.
    let run = cli.run(&["list", "--json"]);
    assert_eq!(row(&run.json(), 1)["usage"]["fiveHour"]["pct"], 50.0);
    assert_eq!(
        mock.usage_calls(STALE),
        1,
        "the dead bearer is never retried"
    );
}

#[test]
fn refresh_of_an_inactive_account_rotates_only_its_slot() {
    let mock = UsageMock::start();
    let cli = Cli::new().with_mock(&mock);
    cli.add_scripted("erin@example.com", "acct-erin", STALE);
    cli.add_scripted("frank@example.com", "acct-frank", OK);
    let live_before = cli.live();
    assert_eq!(live_before["tokens"]["access_token"], OK, "frank is live");

    let run = cli.run(&["list", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "");
    let payload = run.json();
    assert_eq!(row(&payload, 1)["usage"]["fiveHour"]["pct"], 50.0);
    assert_eq!(row(&payload, 2)["usage"]["fiveHour"]["pct"], 35.0);
    assert_eq!(
        cli.credential(1)["tokens"]["refresh_token"],
        ROTATED_REFRESH
    );
    assert_eq!(cli.live(), live_before, "the live login was not touched");
    assert_eq!(cli.credential(2), live_before);
}

#[test]
fn log_file_is_created_lazily_in_the_backup_root() {
    let cli = Cli::new();
    let run = cli.run(&["status"]);
    assert_eq!(run.stdout, "Status: No active Codex or Claude account\n");
    assert!(
        !cli.ccsw_home.exists(),
        "a no-op run creates neither the store nor the log"
    );

    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "acct-bob", "rt-b");
    let run = cli.run(&["switch", "1"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let log = std::fs::read_to_string(cli.log_path()).expect("ccsw.log");
    assert!(
        log.contains("INFO") && log.contains("Switched from account 2 to 1"),
        "{log}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(cli.log_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // --debug mirrors the records on stderr; the file keeps receiving them.
    let run = cli.run(&["switch", "2", "--debug"]);
    assert_eq!(run.status, 0);
    assert!(
        run.stderr.contains("Switched from account 1 to 2"),
        "{}",
        run.stderr
    );
    assert!(
        std::fs::read_to_string(cli.log_path())
            .unwrap()
            .contains("Switched from account 1 to 2")
    );

    let run = cli.run(&["switch", "1", "--json"]);
    assert_eq!(run.status, 0);
    assert_eq!(run.stderr, "", "records go to the file, not to stderr");
}

#[test]
fn env_verb_prints_the_session_export() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    let run = cli.run(&["env", "1"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    let expected = format!(
        "export CODEX_HOME='{}'\n",
        cli.ccsw_home
            .join("sessions")
            .join("1-alice_example.com")
            .display()
    );
    assert_eq!(run.stdout, expected);
}
