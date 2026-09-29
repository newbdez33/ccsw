//! `cswitch auto --once` end to end: the built binary, the usage mock, the
//! fake `codex`, and the files a switch leaves behind.

mod support;

use serde_json::Value;
use support::usage_mock::{self, COOL, HOT, LIMIT_5H, LIMIT_7D, UsageMock, WARM};
use support::{Cli, Run, chatgpt_auth_with_tokens};

/// Alice is the live login with `active`; the others follow in slot order.
fn world(mock: &UsageMock, active: &str, others: &[&str]) -> Cli {
    let cli = Cli::new().with_mock(mock);
    cli.add_scripted("alice@example.com", "acct-alice", active);
    for (i, bearer) in others.iter().enumerate() {
        let n = i + 2;
        cli.add_scripted(
            &format!("user{n}@example.com"),
            &format!("acct-{n}"),
            bearer,
        );
    }
    cli.write_live(&chatgpt_auth_with_tokens(
        "alice@example.com",
        "acct-alice",
        active,
        &usage_mock::live_refresh_token("alice@example.com", "acct-alice"),
    ));
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
    let path = cli.cswitch_home.join("autoswitch_state.json");
    path.exists().then(|| support::read_json(&path))
}

#[test]
fn once_switches_when_the_active_account_is_over_the_threshold() {
    let mock = UsageMock::start();
    let cli = world(&mock, HOT, &[COOL]);
    let run = cli.run(&["auto", "--once", "--json"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.stderr, "");
    let events = parse_events(&run);
    assert_eq!(events.len(), 2, "{events:?}");
    let poll = &events[0];
    assert_eq!(poll["event"], "poll");
    assert_eq!(poll["schemaVersion"], 1);
    assert_eq!(poll["active"]["number"], 1);
    assert_eq!(poll["headroomPct"]["1"], 5.0);
    assert_eq!(poll["headroomPct"]["2"], 80.0);
    assert_eq!(poll["threshold"], 90.0);
    assert_eq!(poll["windowsPct"]["1"]["5h"], 95.0);
    let switch = &events[1];
    assert_eq!(switch["event"], "switch");
    assert_eq!(switch["trigger"], "proactive");
    assert_eq!(switch["dryRun"], false);
    assert_eq!(switch["from"]["number"], 1);
    assert_eq!(switch["to"]["number"], 2);
    assert_eq!(switch["to"]["email"], "user2@example.com");

    assert_eq!(cli.live()["tokens"]["access_token"], COOL);
    assert_eq!(cli.live(), cli.credential(2));
    assert_eq!(cli.roster()["activeAccountNumber"], 2);
    let state = state(&cli).expect("autoswitch_state.json");
    assert_eq!(state["schemaVersion"], 1);
    assert_eq!(state["lastSwitchTo"], "2");
    assert_eq!(state["lastSwitchFrom"], 1);
    assert!(state["lastSwitchAt"].is_number());
    assert_eq!(cli.live_backups().len(), 1);
    assert_eq!(cli.codex_calls(), ["app-server daemon version"]);
    assert_eq!(mock.usage_calls(HOT), 1);
    assert_eq!(mock.usage_calls(COOL), 1);

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
    let cli = world(&mock, WARM, &[COOL]);
    let live = cli.live();
    let run = cli.run(&["auto", "--once", "--json"]);
    assert_eq!(run.status, 2, "{}{}", run.stdout, run.stderr);
    let events = parse_events(&run);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0]["headroomPct"]["1"], 38.0);
    assert_eq!(events[1]["event"], "no-switch");
    assert_eq!(events[1]["reason"], "below-threshold");
    assert_eq!(events[1]["detail"], "62% < 90%");
    assert_eq!(cli.live(), live);
    assert!(state(&cli).is_none());
    assert!(cli.codex_calls().is_empty());

    let run = cli.run(&["auto", "--once"]);
    assert_eq!(run.status, 2);
    assert!(
        run.stdout
            .contains("no switch: below-threshold (62% < 90%)"),
        "{}",
        run.stdout
    );
}

#[test]
fn threshold_override_and_dry_run() {
    let mock = UsageMock::start();
    let cli = world(&mock, WARM, &[COOL]);
    let live = cli.live();

    let run = cli.run(&["auto", "--once", "--json", "--threshold", "50", "--dry-run"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    let events = parse_events(&run);
    assert_eq!(events[0]["threshold"], 50.0);
    assert_eq!(events[1]["event"], "switch");
    assert_eq!(events[1]["dryRun"], true);
    assert_eq!(events[1]["to"]["number"], 2);
    assert_eq!(cli.live(), live, "dry-run never touches auth.json");
    assert!(state(&cli).is_none(), "dry-run never writes state");
    assert!(
        cli.codex_calls().is_empty(),
        "dry-run never probes the daemon"
    );

    let run = cli.run(&["auto", "--once", "--json", "--threshold", "50"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    let events = parse_events(&run);
    assert_eq!(events[1]["event"], "switch");
    assert_eq!(events[1]["dryRun"], false);
    assert_eq!(cli.live()["tokens"]["access_token"], COOL);
    assert_eq!(state(&cli).unwrap()["lastSwitchTo"], "2");
}

#[test]
fn once_with_every_candidate_at_limit_exits_3() {
    let mock = UsageMock::start();
    let cli = world(&mock, LIMIT_5H, &[LIMIT_5H, LIMIT_7D]);
    let live = cli.live();
    let run = cli.run(&["auto", "--once", "--json"]);
    assert_eq!(run.status, 3, "{}{}", run.stdout, run.stderr);
    let events = parse_events(&run);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0]["headroomPct"]["1"], 0.0);
    assert_eq!(events[0]["headroomPct"]["2"], 0.0);
    assert_eq!(events[0]["headroomPct"]["3"], 0.0);
    assert_eq!(events[1]["event"], "all-exhausted");
    let reset = events[1]["earliestResetAt"].as_str().unwrap();
    assert!(reset.ends_with('Z') && reset.len() == 20, "{reset}");
    assert_eq!(cli.live(), live);
    assert!(state(&cli).is_none());
    assert_eq!(
        mock.usage_calls(LIMIT_7D),
        1,
        "the tick escalated to every candidate"
    );
}
