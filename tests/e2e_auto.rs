//! `ccsw auto --once` end to end: the built binary, the usage mock, and
//! the Claude Code files a switch leaves behind. Auto-switch covers Claude
//! Code accounts only (spec §9); `auth.json` is never touched.

mod support;

use serde_json::Value;
use support::usage_mock::{
    self, CLAUDE_HOT, CLAUDE_LIMIT_7D, CLAUDE_OK, CLAUDE_REFRESHED, UsageMock,
};
use support::{Cli, Run, claude_config, claude_creds};

/// Claude accounts in slot order; the LAST one written is the live login, so
/// `active` is written last and the others before it.
fn world(mock: &UsageMock, active: &str, others: &[&str]) -> Cli {
    let cli = Cli::new().with_mock(mock);
    for (i, bearer) in others.iter().enumerate() {
        let n = i + 1;
        let email = format!("user{n}@example.com");
        cli.write_claude_live_with(
            &claude_creds(&usage_mock::claude_live_refresh_token(&email), bearer),
            &claude_config(&email, &format!("org-{n}"), ""),
        );
        let run = cli.run(&["add", "claude"]);
        assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    }
    cli.write_claude_live_with(
        &claude_creds(
            &usage_mock::claude_live_refresh_token("alice@example.com"),
            active,
        ),
        &claude_config("alice@example.com", "org-alice", ""),
    );
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    cli.clear_codex_calls();
    cli
}

fn parse_events(run: &Run) -> Vec<Value> {
    run.stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}")))
        .collect()
}

fn state(cli: &Cli) -> Option<Value> {
    let path = cli.ccsw_home.join("autoswitch_state.json");
    path.exists().then(|| support::read_json(&path))
}

/// The slot `world()` gave alice: the others take 1..=n, alice n+1.
fn alice(others: usize) -> u32 {
    others as u32 + 1
}

#[test]
fn auto_once_switches_the_claude_login_and_leaves_codex_alone() {
    for bearer in [usage_mock::COOL, usage_mock::STALE] {
        let mock = UsageMock::start();
        let cli = world(&mock, CLAUDE_HOT, &[CLAUDE_OK]);
        cli.add_scripted("codex@example.com", "acct-codex", bearer);
        cli.clear_codex_calls();
        let auth = std::fs::read(cli.codex_home.join("auth.json")).unwrap();

        let run = cli.run(&["auto", "--once", "--json"]);
        assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
        assert_eq!(
            cli.claude_credentials()["claudeAiOauth"]["accessToken"],
            CLAUDE_OK
        );
        assert_eq!(
            cli.claude_config()["oauthAccount"]["emailAddress"],
            "user1@example.com"
        );
        assert_eq!(
            std::fs::read(cli.codex_home.join("auth.json")).unwrap(),
            auth
        );
        assert!(cli.codex_calls().is_empty());
        assert_eq!(mock.claude_token_calls(), 0);
        let events = parse_events(&run);
        assert!(events[0]["headroomPct"].get("3").is_none());
        assert!(
            mock.requests()
                .iter()
                .all(|r| !r.path.starts_with("/wham/") && r.path != "/oauth/token"),
            "{:?}",
            mock.trail()
        );
    }
}

#[test]
fn once_switches_when_the_active_account_is_over_the_threshold() {
    let mock = UsageMock::start();
    let cli = world(&mock, CLAUDE_HOT, &[CLAUDE_OK]);
    let live_before = cli.claude_credentials();
    let run = cli.run(&["auto", "--once", "--json"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.stderr, "");
    let events = parse_events(&run);
    assert_eq!(events.len(), 2, "{events:?}");
    let poll = &events[0];
    assert_eq!(poll["event"], "poll");
    assert_eq!(poll["active"]["number"], alice(1));
    assert_eq!(poll["headroomPct"][alice(1).to_string()], 5.0);
    assert_eq!(poll["headroomPct"]["1"], 45.0);
    assert_eq!(poll["threshold"], 90.0);
    assert_eq!(poll["windowsPct"][alice(1).to_string()]["5h"], 95.0);
    let switch = &events[1];
    assert_eq!(switch["event"], "switch");
    assert_eq!(switch["trigger"], "proactive");
    assert_eq!(switch["dryRun"], false);
    assert_eq!(switch["from"]["number"], alice(1));
    assert_eq!(switch["to"]["number"], 1);
    assert_eq!(switch["to"]["email"], "user1@example.com");

    // The live Claude login is now slot 1's; Codex was never touched.
    let live = cli.claude_credentials();
    assert_eq!(live["claudeAiOauth"]["accessToken"], CLAUDE_OK);
    assert_ne!(live, live_before);
    assert_eq!(
        cli.claude_config()["oauthAccount"]["emailAddress"],
        "user1@example.com"
    );
    assert_eq!(cli.roster()["activeByProvider"]["claude"], 1);
    assert!(
        cli.codex_calls().is_empty(),
        "no Codex daemon probe: {:?}",
        cli.codex_calls()
    );
    assert_eq!(cli.claude_backups().len(), 1);
    let state = state(&cli).expect("autoswitch_state.json");
    assert_eq!(state["schemaVersion"], 1);
    assert_eq!(state["lastSwitchTo"], "1");
    assert_eq!(state["lastSwitchFrom"], alice(1));
    assert!(state["lastSwitchAt"].is_number());
    assert_eq!(mock.claude_usage_calls(CLAUDE_HOT), 1);
    assert_eq!(mock.claude_usage_calls(CLAUDE_OK), 1);
    assert_eq!(
        mock.claude_token_calls(),
        0,
        "nothing was refreshed: {:?}",
        mock.trail()
    );

    // The new active account is far from the threshold: the next tick idles.
    let run = cli.run(&["auto", "--once", "--json"]);
    assert_eq!(run.status, 2);
    let events = parse_events(&run);
    assert_eq!(events[1]["event"], "no-switch");
    assert_eq!(events[1]["reason"], "below-threshold");
}

#[test]
fn once_below_threshold_exits_2_and_writes_nothing() {
    let mock = UsageMock::start();
    let cli = world(&mock, CLAUDE_OK, &[CLAUDE_REFRESHED]);
    let live = cli.claude_credentials();
    let run = cli.run(&["auto", "--once", "--json"]);
    assert_eq!(run.status, 2, "{}{}", run.stdout, run.stderr);
    let events = parse_events(&run);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0]["headroomPct"][alice(1).to_string()], 45.0);
    assert_eq!(events[1]["event"], "no-switch");
    assert_eq!(events[1]["reason"], "below-threshold");
    assert_eq!(events[1]["detail"], "55% < 90%");
    assert_eq!(cli.claude_credentials(), live);
    assert!(state(&cli).is_none());
    assert!(cli.codex_calls().is_empty());

    let run = cli.run(&["auto", "--once"]);
    assert_eq!(run.status, 2);
    assert!(
        run.stdout
            .contains("no switch: below-threshold (55% < 90%)"),
        "{}",
        run.stdout
    );
}

#[test]
fn threshold_override_and_dry_run() {
    let mock = UsageMock::start();
    let cli = world(&mock, CLAUDE_OK, &[CLAUDE_REFRESHED]);
    let live = cli.claude_credentials();

    let run = cli.run(&["auto", "--once", "--json", "--threshold", "50", "--dry-run"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    let events = parse_events(&run);
    assert_eq!(events[0]["threshold"], 50.0);
    assert_eq!(events[1]["event"], "switch");
    assert_eq!(events[1]["dryRun"], true);
    assert_eq!(events[1]["to"]["number"], 1);
    assert_eq!(
        cli.claude_credentials(),
        live,
        "dry-run never touches the live login"
    );
    assert!(state(&cli).is_none(), "dry-run never writes state");
    assert!(cli.claude_backups().is_empty(), "dry-run never backs up");

    let run = cli.run(&["auto", "--once", "--json", "--threshold", "50"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    let events = parse_events(&run);
    assert_eq!(events[1]["event"], "switch");
    assert_eq!(events[1]["dryRun"], false);
    assert_eq!(
        cli.claude_credentials()["claudeAiOauth"]["accessToken"],
        CLAUDE_REFRESHED
    );
    assert_eq!(state(&cli).unwrap()["lastSwitchTo"], "1");
}

#[test]
fn once_with_every_candidate_at_limit_exits_3() {
    let mock = UsageMock::start();
    let cli = world(&mock, CLAUDE_LIMIT_7D, &[CLAUDE_LIMIT_7D, CLAUDE_LIMIT_7D]);
    let live = cli.claude_credentials();
    let run = cli.run(&["auto", "--once", "--json"]);
    assert_eq!(run.status, 3, "{}{}", run.stdout, run.stderr);
    let events = parse_events(&run);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0]["headroomPct"]["1"], 0.0);
    assert_eq!(events[0]["headroomPct"]["2"], 0.0);
    assert_eq!(events[0]["headroomPct"][alice(2).to_string()], 0.0);
    assert_eq!(events[1]["event"], "all-exhausted");
    let reset = events[1]["earliestResetAt"].as_str().unwrap();
    assert!(reset.ends_with('Z') && reset.len() == 20, "{reset}");
    assert_eq!(cli.claude_credentials(), live);
    assert!(state(&cli).is_none());
    assert_eq!(
        mock.claude_usage_calls(CLAUDE_LIMIT_7D),
        3,
        "the tick escalated to every candidate: {:?}",
        mock.trail()
    );
}
