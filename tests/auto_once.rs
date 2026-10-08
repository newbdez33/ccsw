//! `ccsw auto --once` through `run_cli_to` with a fake facade: exit codes,
//! the JSONL event stream, dry-run, and the state a real switch records.
//!
//! The usage store is seeded with fresh, planned rows so no tick ever fetches.

use std::fs;

use serde_json::{Value, json};

use ccsw::autoswitch::{AutoFacade, run_cli_to};
use ccsw::claude::credentials::{ClaudeCredential, OauthAccount, SlotFile};
use ccsw::errors::{CcswError, Result};
use ccsw::model::{
    AccountRecord, AccountRef, CurrentAccount, NormalizedUsage, Roster, SwitchOutcome, WindowUsage,
    now_unix,
};
use ccsw::paths::Paths;
use ccsw::provider::Provider;
use ccsw::store::usage_store::{FetchRecord, UsageStore};
use ccsw::store::{Store, credentials, state};

const FAR: i64 = 4_102_444_800;

/// A Claude slot file for `u{slot}@x.com` with a far-future token.
fn claude_slot(slot: u32) -> Value {
    let credential = ClaudeCredential::from_value(json!({"claudeAiOauth": {
        "accessToken": format!("cat-{slot}"), "refreshToken": format!("crt-{slot}"),
        "expiresAt": FAR * 1000, "scopes": ["user:inference"]
    }}));
    SlotFile::new(
        &credential,
        OauthAccount::synthesized(&format!("u{slot}@x.com")),
    )
    .to_value()
}

struct Fake {
    store: Store,
    roster: Roster,
    current: CurrentAccount,
    switches: Vec<u32>,
    fail_switch: bool,
}

impl AutoFacade for Fake {
    fn store(&self) -> &Store {
        &self.store
    }
    fn roster(&mut self) -> Result<Roster> {
        Ok(self.roster.clone())
    }
    fn current_account(&mut self, _provider: Provider) -> Result<CurrentAccount> {
        Ok(self.current.clone())
    }
    fn switch_to(&mut self, slot: u32) -> Result<SwitchOutcome> {
        if self.fail_switch {
            return Err(CcswError::switch("switch exploded"));
        }
        self.switches.push(slot);
        let from = self.current.slot().map(|n| AccountRef {
            number: Some(n),
            email: format!("u{n}@x.com"),
        });
        let email = format!("u{slot}@x.com");
        self.current = CurrentAccount::Managed {
            slot,
            email: email.clone(),
            api_key: false,
        };
        Ok(SwitchOutcome {
            switched: true,
            provider: Provider::Claude,
            from,
            to: Some(AccountRef {
                number: Some(slot),
                email,
            }),
            strategy: "direct".into(),
            reason: "switched".into(),
            message: "Switched".into(),
            warnings: Vec::new(),
        })
    }
}

struct World {
    _dir: tempfile::TempDir,
    fake: Fake,
    now: f64,
}

impl World {
    fn new(slots: &[u32]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_values(
            Some(dir.path().join("store")),
            Some(dir.path().join("codex")),
            Some(dir.path().join("claude")),
            dir.path(),
        )
        .unwrap();
        let store = Store::open(paths);
        let mut roster = Roster::empty();
        for &slot in slots {
            let mut record = AccountRecord::new(format!("u{slot}@x.com"));
            record.provider = Provider::Claude;
            record.organization_uuid = format!("org-{slot}");
            roster.add_record(slot, record);
            credentials::write(&store, slot, &claude_slot(slot)).unwrap();
        }
        let current = CurrentAccount::Managed {
            slot: slots[0],
            email: format!("u{}@x.com", slots[0]),
            api_key: false,
        };
        Self {
            _dir: dir,
            fake: Fake {
                store,
                roster,
                current,
                switches: Vec::new(),
                fail_switch: false,
            },
            now: now_unix() as f64,
        }
    }

    fn seed(&self, slot: u32, five_hour: f64, seven_day: f64, reset: Option<i64>) {
        let usage = NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: five_hour,
                resets_at: reset.map(ccsw::model::format_iso),
            }),
            seven_day: Some(WindowUsage {
                pct: seven_day,
                resets_at: None,
            }),
            ..NormalizedUsage::default()
        };
        UsageStore::new(&self.fake.store.paths)
            .record(
                slot,
                &self.fake.roster.record(slot).unwrap().identity(),
                FetchRecord::Success {
                    usage,
                    plan: Some((self.now + 600.0, 300.0)),
                },
                self.now,
            )
            .unwrap();
    }

    fn run(&mut self, args: &[&str]) -> (i32, Vec<String>) {
        let argv = args.iter().map(|s| s.to_string()).collect();
        let mut out = Vec::new();
        let code = run_cli_to(argv, &mut self.fake, &mut out);
        let text = String::from_utf8(out).unwrap();
        (code, text.lines().map(str::to_string).collect())
    }

    fn run_json(&mut self, args: &[&str]) -> (i32, Vec<Value>) {
        let (code, lines) = self.run(args);
        let events = lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}")))
            .collect();
        (code, events)
    }
}

#[test]
fn dry_run_reports_the_switch_and_writes_nothing() {
    let mut world = World::new(&[1, 2, 3]);
    world.seed(1, 95.0, 10.0, None);
    world.seed(2, 40.0, 10.0, None);
    world.seed(3, 20.0, 10.0, None);
    let (code, events) = world.run_json(&["--once", "--json", "--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(events.len(), 2, "{events:?}");
    let poll = &events[0];
    assert_eq!(poll["schemaVersion"], 1);
    assert_eq!(poll["event"], "poll");
    assert_eq!(poll["active"], json!({"number": 1, "email": "u1@x.com"}));
    assert_eq!(poll["headroomPct"], json!({"1": 5.0, "2": 60.0, "3": 80.0}));
    assert_eq!(poll["threshold"], 90.0);
    assert_eq!(poll["windowsPct"]["1"], json!({"5h": 95.0, "7d": 10.0}));
    assert!(poll.get("fetchErrors").is_none());
    let ts = poll["ts"].as_str().unwrap();
    assert!(ts.ends_with('Z') && ts.len() == 20, "{ts}");
    let switch = &events[1];
    assert_eq!(switch["event"], "switch");
    assert_eq!(switch["trigger"], "proactive");
    assert_eq!(switch["dryRun"], true);
    assert_eq!(switch["from"]["number"], 1);
    assert_eq!(switch["to"], json!({"number": 3, "email": "u3@x.com"}));
    assert_eq!(switch["warnings"], json!([]));
    assert!(world.fake.switches.is_empty());
    assert!(!world.fake.store.paths.state_file().exists());
}

#[test]
fn exit_codes_follow_the_outcome() {
    let mut world = World::new(&[1, 2]);
    world.seed(1, 62.0, 10.0, None);
    world.seed(2, 20.0, 10.0, None);
    let (code, events) = world.run_json(&["--once", "--json"]);
    assert_eq!(code, 2);
    assert_eq!(events[1]["event"], "no-switch");
    assert_eq!(events[1]["reason"], "below-threshold");
    assert_eq!(events[1]["detail"], "62% < 90%");

    // Flags override the file: a lower threshold makes it a switch.
    let (code, events) = world.run_json(&["--once", "--json", "--threshold", "50", "--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(events[0]["threshold"], 50.0);
    assert_eq!(events[1]["event"], "switch");

    let now = world.now as i64;
    world.seed(1, 100.0, 10.0, Some(now + 1200));
    world.seed(2, 100.0, 10.0, Some(now + 900));
    let (code, events) = world.run_json(&["--once", "--json"]);
    assert_eq!(code, 3);
    assert_eq!(events[1]["event"], "all-exhausted");
    assert_eq!(
        events[1]["earliestResetAt"],
        ccsw::model::format_iso(now + 900)
    );

    world.seed(2, 10.0, 10.0, None);
    world.fake.fail_switch = true;
    let (code, events) = world.run_json(&["--once", "--json"]);
    assert_eq!(code, 1);
    let error = events.last().unwrap();
    assert_eq!(error["event"], "error");
    assert_eq!(error["message"], "switch exploded");
    assert_eq!(error["transient"], true);

    assert_eq!(world.run(&["--once", "--bogus"]).0, 2);
    assert_eq!(world.run(&["--once", "--interval", "abc"]).0, 2);
}

#[test]
fn a_real_switch_records_state_and_starts_the_cooldown() {
    let mut world = World::new(&[1, 2]);
    world.seed(1, 95.0, 10.0, None);
    world.seed(2, 20.0, 10.0, None);
    let (code, events) = world.run_json(&["--once", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(events[1]["event"], "switch");
    assert_eq!(events[1]["dryRun"], false);
    assert_eq!(events[1]["to"]["number"], 2);
    assert_eq!(world.fake.switches, vec![2]);
    assert_eq!(world.fake.current.slot(), Some(2));
    let state = state::read(&world.fake.store.paths);
    assert!(state.last_switch_at.unwrap() >= world.now);
    assert_eq!(state.last_switch_to.as_deref(), Some("2"));
    assert_eq!(state.last_switch_from, Some(1));
    let raw: Value =
        serde_json::from_slice(&fs::read(world.fake.store.paths.state_file()).unwrap()).unwrap();
    assert_eq!(raw["schemaVersion"], 1);

    // The new active account is over the threshold too, but the cooldown holds.
    world.seed(2, 95.0, 10.0, None);
    world.seed(1, 20.0, 10.0, None);
    let (code, events) = world.run_json(&["--once", "--json"]);
    assert_eq!(code, 2);
    assert_eq!(events[1]["reason"], "cooldown");
    assert_eq!(world.fake.switches, vec![2]);

    // `--cooldown 0` lifts it.
    let (code, _) = world.run_json(&["--once", "--json", "--cooldown", "0"]);
    assert_eq!(code, 0);
    assert_eq!(world.fake.switches, vec![2, 1]);
}

#[test]
fn human_mode_prints_timestamped_lines() {
    let mut world = World::new(&[1, 2]);
    world.seed(1, 62.0, 10.0, None);
    world.seed(2, 20.0, 10.0, None);
    let (code, lines) = world.run(&["--once"]);
    assert_eq!(code, 2);
    assert_eq!(lines.len(), 2, "{lines:?}");
    for line in &lines {
        let (clock, rest) = line.split_once("  ").expect("two-space separator");
        assert_eq!(clock.len(), 8, "{clock}");
        assert!(
            clock.chars().all(|c| c.is_ascii_digit() || c == ':'),
            "{clock}"
        );
        assert!(!rest.is_empty());
    }
    assert!(
        lines[0].contains(
            "Account-1 (u1@x.com): 62% used (switch at 90%) | others: #2: 5h 20% · 7d 10%"
        ),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains("no switch: below-threshold (62% < 90%)"),
        "{}",
        lines[1]
    );
    assert!(
        !lines[0].contains("Auto-switch running"),
        "no banner with --once"
    );
}
