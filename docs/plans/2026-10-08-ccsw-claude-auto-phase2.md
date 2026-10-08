# ccsw Claude auto-switch — phase 2 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Status:** Implemented for v0.5.0. All eight tasks are complete. The following
implementation decisions supersede the original examples below:

- Collection includes the tick-start and re-read live slots of the selected provider
  only. Fetching the other provider's active slot could refresh its credentials.
- A Keychain-unavailable login stays held until readable, including after 30 minutes.
  The expired-token hold keeps its existing 30-minute cap.
- Model validation uses `UsageEntry::decision_value()` so unreadable sentinels and
  untrusted historical measurements cannot produce a warning.
- Provider-filtered snapshots recompute their active slot. An OFF auto screen hides
  the unavailable engine controls.
- The Keychain injection setter is test-only. Tests use the provider-specific request
  counters and fixtures, and compare JSON keys and values without assuming key order.


**Goal:** `ccsw auto` watches and switches **Claude Code** accounts — the provider that picks a switched login up inside a running session — and refuses to pretend it can do the same for Codex.

**Architecture:** The v0.1 engine in `src/autoswitch.rs` (poll → classify → rank → freshen → switch, with cooldown, quarantine and the `token expired` idle-hold) is kept whole and given one `provider` field; the CLI and the TUI construct it for `Provider::Claude`. The active account comes from `collect::live_login_for(provider)`, the candidates are the switchable slots of that provider, refresh and switch go through the phase-1 Claude paths that already exist. Four Claude-specific additions ride on top: a `keychain unavailable` active account is held like `token expired`; `auto codex` and rosters without a Claude account are refused before any tick; events move to `schemaVersion: 2` with `provider`; the `autoswitch.model` typo guard that v0.1 omitted is implemented. The TUI auto view shows the Claude active card and Claude candidates only. Codex auto-switch is withdrawn (spec §9 says why).

**Tech Stack:** Rust 2024 (MSRV 1.88), clap (existing `AutoArgs`), serde_json, ratatui (existing), the in-module axum mock in `src/collect.rs` and the `tests/support` harness (existing). No new dependencies.

**Spec:** `docs/specs/2026-10-07-ccsw-claude-provider-design.md` §5, §6.1–6.3, §9, §12, §15, §16 as amended on 2026-10-08 (commit `5512e86`); the engine body is `docs/specs/2026-09-29-ccsw-design.md` §9 and `docs/research/cswap-model-autoswitch.md` §6.

## Global Constraints

- Rust edition 2024, `rust-version = "1.88"`; the quality gate is `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` (every task ends green). Run the suite as `env -u CODEX_HOME cargo test --all` on this machine (the host exports `CODEX_HOME`; one pre-existing test asserts it is unset).
- The engine body (settings, flags, clamps, exit codes `0` switched / `1` error / `2` no action / `3` blocked / `130` Ctrl-C, event kinds, `no-switch` reasons, human lines, banner, signal handling) is v0.1 §9 unchanged.
- The active Claude account is never refreshed by ccsw (spec §8); the collector's `actives` protects both the tick-start and re-read live slot of the selected provider, and `claude_may_refresh` fails safe.
- Refusal text, verbatim (spec §9): `Auto-switch covers Claude Code accounts only: Codex sessions do not pick up a switched account without a restart. Add a Claude Code account with 'ccsw add claude' first.` — printed to stderr, exit `1`, before any tick.
- `active-idle` detail, verbatim: `token expired while Claude Code is idle; resumes on next use` (token) and `keychain unavailable; holding until Claude Code's login is readable` (Keychain). Keychain failures never count toward failover. Expired tokens hold up to `IDLE_HOLD_MAX_S` (1800 s) before unhealthy counting resumes.
- Events: `{"schemaVersion": 2, "event": …, "ts": …, "provider": "claude", …}`; human lines unchanged. `autoswitch_state.json` keeps `schemaVersion: 1`.
- Model-name warning, verbatim: `autoswitch.model: <names> matches no account's usage windows — only the 5h/7d limits are being watched for it (typo?)` as a `config-warning` event, at most once per engine run, only on a tick where every relevant slot's usage is readable; `all` never warns.
- Unit tests never touch the real macOS Keychain, `~/.claude`, `~/.codex` or the network: `temp_store()` paths, the `FakeSecurity` seam, the in-module mock, `CCSW_KEYCHAIN=off` in the e2e harness.
- Commit after every task with a conventional message (`feat(auto): …`, `fix(...)`, `test(...)`, `docs(...)`); no `Co-Authored-By` lines, no AI attribution.

## Review Focus

1. **A mixed roster where the Claude slot is the one over the threshold** — `auto --once` must switch the Claude login (live `.credentials.json` + `.claude.json` change, `auth.json` byte-identical, no `codex` daemon probe) — Task 2 (`auto_once_switches_the_claude_login_and_leaves_codex_alone`).
2. **The live Claude token expiring during a tick** — the engine must never rotate it (`claude_token_calls` stays 0 for the active slot) even when the slot is also an inactive-looking candidate under another name — Task 2 keeps `auto_once_never_refreshes_or_targets_the_live_claude_login` (tests/cli_claude.rs) green and asserts the token-call count.
3. **A locked Keychain while the engine runs** — the active account shows `keychain unavailable`; the engine holds (`active-idle`), does not fail over, does not switch a cool candidate in — Task 3 (`keychain_unavailable_active_holds_like_token_expired`).
4. **A Codex-only user who runs `ccsw auto` after upgrading** — the command must explain itself and exit `1`, not loop printing `no-active-account` — Task 4 (`auto_refuses_without_a_claude_account` and the e2e `auto_is_refused_on_a_codex_only_store`).
5. **An `autoswitch.model` typo (`Fabel`)** — one `config-warning` naming it, the engine keeps watching 5h/7d; the correct name and `all` never warn — Task 6 (`model_typo_warns_once_and_known_names_never_do`).

---

## File structure

No new source files. Modified files (owner task in parentheses):

| path | responsibility / change |
|---|---|
| `src/autoswitch.rs` | engine: `provider` field (1), Claude wiring in `run_cli_to` (2), `keychain unavailable` hold + `FakeSecurity` seam (3), selector + refusal (4), events v2 (5), model warning (6) |
| `src/switcher.rs` | `AutoFacade` impl takes the provider (1) |
| `src/tui/worker.rs` | the engine thread constructs a Claude engine (2) |
| `src/collect.rs` | `run_pass_with` becomes `pub(crate)` (3) |
| `src/claude/keychain.rs` | `#[cfg(test)] FakeSecurity` (3) |
| `src/tui/auto.rs`, `src/tui/app.rs`, `src/tui/modals.rs` | Claude-only candidates and active card, no-account notice, modal copy (7) |
| `tests/auto_once.rs` | Claude world (2) |
| `tests/e2e_auto.rs` | Claude world through the binary (2) |
| `tests/support/usage_mock.rs` | `CLAUDE_HOT` bearer (2) |
| `tests/cli_claude.rs` | refusal e2e tests (4), comment fix (2) |
| `tests/tui_render.rs` | modal copy, no-account notice (7) |
| `README.md`, `CHANGELOG.md` | Claude-only auto-switch and why (8) |

---

### Task 1: The engine takes a provider

**Files:**
- Modify: `src/autoswitch.rs` (`AutoFacade`, `Engine`, `tick_inner`, `run_cli_to`, tests), `src/switcher.rs` (the `AutoFacade` impl, ~line 2344), `src/tui/worker.rs` (`start_engine`, ~line 410), `tests/auto_once.rs` (the `Fake` impl)
- Test: `src/autoswitch.rs`

**Interfaces:**
- Produces: `AutoFacade::current_account(&mut self, provider: Provider) -> Result<CurrentAccount>` (replaces the no-argument method); `Engine::new(facade, provider: Provider, settings, dry_run, sink)`; `Engine::provider(&self) -> Provider`; the engine's candidate filter `record.provider == self.provider`; the `active-idle` detail `token expired while {provider.tool_name()} is idle; resumes on next use`.
- Consumes: `Switcher::current_account_for(&self, Provider)` (`src/switcher.rs:616`), `Provider::tool_name()` (`Codex` / `Claude Code`).

This task keeps the product on Codex (the CLI and the TUI pass `Provider::Codex`), so every existing test stays green; Task 2 flips the two call sites to Claude.

- [x] **Step 1: Write the failing tests**

In `src/autoswitch.rs`'s `mod tests`, change the `Fake` facade to carry a provider and implement the new trait method. Replace the struct and its `AutoFacade` impl (lines ~1333-1395) with:

```rust
    struct Fake {
        store: Store,
        roster: Roster,
        provider: Provider,
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
        fn current_account(&mut self, provider: Provider) -> Result<CurrentAccount> {
            assert_eq!(provider, self.provider, "the engine asks for its own provider");
            Ok(self.current.clone())
        }
        fn switch_to(&mut self, slot: u32) -> Result<SwitchOutcome> {
            if self.fail_switch {
                return Err(crate::errors::CcswError::switch("boom"));
            }
            self.switches.push(slot);
            let from = match &self.current {
                CurrentAccount::Managed { slot, email, .. } => Some(AccountRef {
                    number: Some(*slot),
                    email: email.clone(),
                }),
                _ => None,
            };
            let email = self.roster.record(slot).unwrap().email.clone();
            let switched = from.as_ref().and_then(|f| f.number) != Some(slot);
            if switched {
                self.current = CurrentAccount::Managed {
                    slot,
                    email: email.clone(),
                    api_key: false,
                };
            }
            Ok(SwitchOutcome {
                switched,
                provider: self.provider,
                from,
                to: Some(AccountRef {
                    number: Some(slot),
                    email,
                }),
                strategy: "direct".into(),
                reason: if switched {
                    "switched"
                } else {
                    "already-active"
                }
                .into(),
                message: String::new(),
                warnings: vec!["w1".into()],
            })
        }
    }
```

Add a Claude slot-file builder next to `auth(...)`:

```rust
    /// A Claude slot file for `a{slot}@example.com` whose token is far from expiry.
    fn claude_slot(slot: u32) -> Value {
        use crate::claude::credentials::{ClaudeCredential, OauthAccount, SlotFile};
        let credential = ClaudeCredential::from_value(json!({"claudeAiOauth": {
            "accessToken": format!("cat-{slot}"), "refreshToken": format!("crt-{slot}"),
            "expiresAt": FAR * 1000, "scopes": ["user:inference"]
        }}));
        SlotFile::new(
            &credential,
            OauthAccount::synthesized(&format!("a{slot}@example.com")),
        )
        .to_value()
    }
```

Change `Fixture::new` into a provider-aware constructor and keep `new` as the Codex form:

```rust
    impl Fixture {
        fn new(slots: &[u32]) -> Self {
            Self::new_for(Provider::Codex, slots)
        }

        /// Claude records with Claude slot files; the first slot is the live login.
        fn claude(slots: &[u32]) -> Self {
            Self::new_for(Provider::Claude, slots)
        }

        fn new_for(provider: Provider, slots: &[u32]) -> Self {
            let (dir, store) = temp_store();
            let mut roster = Roster::empty();
            for &slot in slots {
                let id = format!("a{slot}");
                let mut record = AccountRecord::new(format!("{id}@example.com"));
                record.organization_uuid = id.clone();
                record.provider = provider;
                roster.add_record(slot, record);
                match provider {
                    Provider::Codex => {
                        credentials::write(&store, slot, &auth(&id, FAR, Some("rt")).0).unwrap()
                    }
                    Provider::Claude => credentials::write(&store, slot, &claude_slot(slot)).unwrap(),
                }
            }
            let current = CurrentAccount::Managed {
                slot: slots[0],
                email: format!("a{}@example.com", slots[0]),
                api_key: false,
            };
            Self {
                _dir: dir,
                fake: Fake {
                    store,
                    roster,
                    provider,
                    current,
                    switches: Vec::new(),
                    fail_switch: false,
                },
                now: now_unix() as f64,
            }
        }
```

(`identity`, `seed` and `seed_failure` are unchanged.) Make the `tick` helper pass the fixture's provider:

```rust
            let mut engine = Engine::new(
                &mut fixture.fake,
                fixture.fake.provider,
                settings,
                dry_run,
                move |event| sink_log.borrow_mut().push(event.clone()),
            );
```

In `expired_active_token_idles_instead_of_counting_unhealthy`, the second `Engine::new(&mut fixture.fake, settings, false, …)` call gains the provider argument the same way (`&mut fixture.fake, Provider::Codex, settings, false, …`).

Rename `a_claude_record_is_never_a_candidate` to `records_of_the_other_provider_are_never_candidates` and extend it with the Claude-engine half:

```rust
    #[test]
    fn records_of_the_other_provider_are_never_candidates() {
        // A Codex engine ignores a Claude record …
        let mut fixture = Fixture::new(&[1, 2, 3]);
        let record = fixture.fake.roster.record_mut(3).unwrap();
        record.provider = Provider::Claude;
        credentials::write(&fixture.fake.store, 3, &claude_slot(3)).unwrap();
        fixture.seed(1, usage(95.0, 10.0, None));
        fixture.seed(2, usage(40.0, 10.0, None));
        fixture.seed(3, usage(5.0, 5.0, None));
        let (outcome, events) = tick(&mut fixture, defaults(), true);
        assert_eq!(outcome, TickOutcome::Switched);
        let Event::Poll { headroom, .. } = &events[0] else {
            panic!("poll event");
        };
        assert!(!headroom.contains_key(&3), "the Claude slot is not ranked");
        assert!(matches!(
            &events[1],
            Event::Switch { to: Some(to), .. } if to.number == Some(2)
        ));

        // … and a Claude engine ignores a Codex record, even the best one.
        let mut fixture = Fixture::claude(&[1, 2, 3]);
        let record = fixture.fake.roster.record_mut(3).unwrap();
        record.provider = Provider::Codex;
        credentials::write(&fixture.fake.store, 3, &auth("a3", FAR, Some("rt")).0).unwrap();
        fixture.seed(1, usage(95.0, 10.0, None));
        fixture.seed(2, usage(40.0, 10.0, None));
        fixture.seed(3, usage(5.0, 5.0, None));
        let (outcome, events) = tick(&mut fixture, defaults(), true);
        assert_eq!(outcome, TickOutcome::Switched);
        let Event::Poll { headroom, .. } = &events[0] else {
            panic!("poll event");
        };
        assert!(!headroom.contains_key(&3), "the Codex slot is not ranked");
        assert!(matches!(
            &events[1],
            Event::Switch { to: Some(to), .. } if to.number == Some(2)
        ));
        assert!(fixture.fake.switches.is_empty(), "dry-run");
    }
```

Add the idle-detail test for a Claude engine right after `expired_active_token_idles_instead_of_counting_unhealthy`:

```rust
    #[test]
    fn idle_detail_names_the_engine_provider() {
        let mut fixture = Fixture::claude(&[1, 2]);
        fixture.seed_failure(1, "http-401");
        fixture.seed(2, usage(20.0, 10.0, None));
        // An expired live token: the collector derives `token expired` for the
        // active slot when its access token is past expiry and nothing fetched it.
        let expired = {
            use crate::claude::credentials::{ClaudeCredential, OauthAccount, SlotFile};
            let credential = ClaudeCredential::from_value(json!({"claudeAiOauth": {
                "accessToken": "cat-1", "refreshToken": "crt-1", "expiresAt": 1000,
                "scopes": ["user:inference"]
            }}));
            SlotFile::new(&credential, OauthAccount::synthesized("a1@example.com")).to_value()
        };
        credentials::write(&fixture.fake.store, 1, &expired).unwrap();
        let mut settings = defaults();
        settings.unhealthy_ticks = 1;
        let (outcome, events) = tick(&mut fixture, settings, false);
        assert_eq!(outcome, TickOutcome::NoAction);
        assert_eq!(
            no_switch_reason(&events),
            Some((
                "active-idle".into(),
                "token expired while Claude Code is idle; resumes on next use".into()
            ))
        );
        assert!(fixture.fake.switches.is_empty());
    }
```

Update `tests/auto_once.rs`'s `Fake` impl (it is a separate crate-level test and must compile):

```rust
    fn current_account(&mut self, _provider: Provider) -> Result<CurrentAccount> {
        Ok(self.current.clone())
    }
```

- [x] **Step 2: Run the tests to see them fail**

Run: `env -u CODEX_HOME cargo test --lib autoswitch::`
Expected: compile errors — `method current_account has 1 parameter but the declaration in trait AutoFacade has 0`, `this function takes 4 arguments but 5 arguments were supplied` for `Engine::new`.

- [x] **Step 3: Implement the provider on the trait and the engine**

`src/autoswitch.rs`, the trait (lines 38-43):

```rust
/// What the engine needs from the switcher.
pub trait AutoFacade {
    fn store(&self) -> &Store;
    fn roster(&mut self) -> Result<Roster>;
    /// The live login of `provider` — the engine only ever asks for its own.
    fn current_account(&mut self, provider: Provider) -> Result<CurrentAccount>;
    fn switch_to(&mut self, slot: u32) -> Result<crate::model::SwitchOutcome>;
}
```

The engine struct and constructor (lines 359-406): add `provider: Provider,` as the second field, and

```rust
    pub fn new(
        facade: &'a mut dyn AutoFacade,
        provider: Provider,
        settings: AutoSwitchSettings,
        dry_run: bool,
        sink: impl FnMut(&Event) + 'a,
    ) -> Self {
        let models = settings.model_names();
        Self {
            facade,
            provider,
            settings,
            models,
            dry_run,
            sink: Box::new(sink),
            clock: Box::new(|| now_unix() as f64),
            unhealthy_ticks: 0,
            idle_hold_since: None,
            sleep_until: None,
            blocked_wait_long: false,
            idle_hold_slow: false,
            active_next_poll_at: None,
        }
    }

    /// The provider this engine rotates.
    pub fn provider(&self) -> Provider {
        self.provider
    }
```

In `tick_inner`:
- line 463: `let (current, email, api_key) = match self.facade.current_account(self.provider)? {`
- lines 493-505, the candidate filter:

```rust
        let store = self.facade.store();
        let provider = self.provider;
        // Only this engine's provider rotates; the other provider's slots are
        // never candidates, whatever their headroom.
        let candidates: Vec<u32> = roster
            .switchable_slots(|slot| credentials::exists(store, slot))
            .into_iter()
            .filter(|slot| {
                roster
                    .record(*slot)
                    .is_some_and(|r| r.provider == provider)
            })
            .filter(|slot| *slot != current && !quarantined.contains(slot))
            .collect();
```

- lines 576-579, the idle detail:

```rust
                        self.no_switch(
                            "active-idle",
                            format!(
                                "token expired while {} is idle; resumes on next use",
                                self.provider.tool_name()
                            ),
                        );
```

`run_cli_to` (line ~1296): `let mut engine = Engine::new(facade, Provider::Codex, settings, args.dry_run, sink);` (Task 2 changes this to Claude).

`src/switcher.rs`, the `AutoFacade` impl (~line 2344):

```rust
    fn current_account(&mut self, provider: Provider) -> Result<CurrentAccount> {
        Switcher::current_account_for(self, provider)
    }
```

(`Provider` is already imported in switcher.rs.)

`src/tui/worker.rs` (~line 410): `let mut engine = Engine::new(&mut switcher, Provider::Codex, settings, dry_run, move |event| { … });` and add `use crate::provider::Provider;` to the imports if it is not there (`grep -n "provider::Provider" src/tui/worker.rs`).

- [x] **Step 4: Run the tests to see them pass**

Run: `env -u CODEX_HOME cargo test --lib autoswitch::` then `env -u CODEX_HOME cargo test --test auto_once`
Expected: all pass, including `records_of_the_other_provider_are_never_candidates` and `idle_detail_names_the_engine_provider`.

- [x] **Step 5: Run the gate and commit**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && env -u CODEX_HOME cargo test --all`
Expected: green (the product still runs the Codex engine; nothing observable changed).

```bash
git add src/autoswitch.rs src/switcher.rs src/tui/worker.rs tests/auto_once.rs
git commit -m "refactor(auto): give the engine a provider and scope the active login and the candidates to it"
```

---

### Task 2: The product runs the Claude engine

**Files:**
- Modify: `src/autoswitch.rs` (`run_cli_to`, `cli_parses_flags_and_runs_once`), `src/tui/worker.rs` (`start_engine`), `tests/auto_once.rs` (Claude world), `tests/support/usage_mock.rs` (`CLAUDE_HOT`), `tests/e2e_auto.rs` (Claude world), `tests/cli_claude.rs` (one comment, one assertion)
- Test: `tests/auto_once.rs`, `tests/e2e_auto.rs`, `tests/cli_claude.rs`

**Interfaces:**
- Produces: the CLI (`run_cli_to`) and the TUI worker construct `Engine::new(…, Provider::Claude, …)`; `usage_mock::CLAUDE_HOT` (Claude bearer: 5h 95 %, 7d 20 %); a Claude `world()` in `tests/e2e_auto.rs`.
- Consumes: Task 1; the phase-1 harness (`Cli::write_claude_live_with`, `claude_creds(refresh, access)`, `claude_config(email, org_uuid, org_name)`, `Cli::claude_credentials()`, `Cli::claude_config()`, `Cli::live()`, `Cli::codex_calls()`, `UsageMock::usage_calls(bearer)`, `UsageMock::claude_token_calls()`).

- [x] **Step 1: Add the `CLAUDE_HOT` bearer to the mock**

`tests/support/usage_mock.rs`, after `CLAUDE_OK` (line ~50):

```rust
/// Claude: 5h 95 %, 7d 20 % — over the default threshold, not at the limit.
pub const CLAUDE_HOT: &str = "cat-hot";
```

and a match arm in `claude_usage` next to `CLAUDE_LIMIT_7D` (line ~286):

```rust
        CLAUDE_HOT => Json(claude_body(95.0, 20.0, "2099-01-03T10:00:00Z")).into_response(),
```

- [x] **Step 2: Rewrite `tests/e2e_auto.rs` as a Claude world (the failing tests)**

Replace the file's header, `world()` and the four tests with the Claude versions below. The engine rules are the same; only the accounts, bearers and the files a switch leaves behind change.

```rust
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
    assert_eq!(cli.claude_config()["oauthAccount"]["emailAddress"], "user1@example.com");
    assert_eq!(cli.roster()["activeByProvider"]["claude"], 1);
    assert!(cli.codex_calls().is_empty(), "no Codex daemon probe: {:?}", cli.codex_calls());
    assert_eq!(cli.claude_backups().len(), 1);
    let state = state(&cli).expect("autoswitch_state.json");
    assert_eq!(state["schemaVersion"], 1);
    assert_eq!(state["lastSwitchTo"], "1");
    assert_eq!(state["lastSwitchFrom"], alice(1));
    assert!(state["lastSwitchAt"].is_number());
    assert_eq!(mock.usage_calls(CLAUDE_HOT), 1);
    assert_eq!(mock.usage_calls(CLAUDE_OK), 1);
    assert_eq!(mock.claude_token_calls(), 0, "nothing was refreshed: {:?}", mock.trail());

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
        run.stdout.contains("no switch: below-threshold (55% < 90%)"),
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
    assert_eq!(cli.claude_credentials(), live, "dry-run never touches the live login");
    assert!(state(&cli).is_none(), "dry-run never writes state");
    assert!(cli.claude_backups().is_empty(), "dry-run never backs up");

    let run = cli.run(&["auto", "--once", "--json", "--threshold", "50"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    let events = parse_events(&run);
    assert_eq!(events[1]["event"], "switch");
    assert_eq!(events[1]["dryRun"], false);
    assert_eq!(cli.claude_credentials()["claudeAiOauth"]["accessToken"], CLAUDE_REFRESHED);
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
        mock.usage_calls(CLAUDE_LIMIT_7D),
        3,
        "the tick escalated to every candidate: {:?}",
        mock.trail()
    );
}
```

Notes for the implementer: `CLAUDE_OK`'s `Fable` scoped window is at 62 % and the default settings have no `autoswitch.model`, so it does not affect headroom (`45.0` = 100 − max(40, 55)). `CLAUDE_REFRESHED` is 30/35 → headroom 65. `claude_backups()` lists `backups/claude/`. If `usage_calls(CLAUDE_LIMIT_7D)` counts differently because the same bearer serves three slots, assert `>= 3` and say so in the report.

- [x] **Step 3: Rewrite `tests/auto_once.rs` as a Claude world**

Replace the `chatgpt(slot)` builder and the world's record loop. Imports: drop `ccsw::codex::auth::AuthJson`, `base64::Engine`, `URL_SAFE_NO_PAD`, `jwt` (if nothing else uses them); add `use ccsw::claude::credentials::{ClaudeCredential, OauthAccount, SlotFile};`.

```rust
/// A Claude slot file for `u{slot}@x.com` with a far-future token.
fn claude_slot(slot: u32) -> Value {
    let credential = ClaudeCredential::from_value(json!({"claudeAiOauth": {
        "accessToken": format!("cat-{slot}"), "refreshToken": format!("crt-{slot}"),
        "expiresAt": FAR * 1000, "scopes": ["user:inference"]
    }}));
    SlotFile::new(&credential, OauthAccount::synthesized(&format!("u{slot}@x.com"))).to_value()
}
```

In `World::new`:

```rust
        for &slot in slots {
            let mut record = AccountRecord::new(format!("u{slot}@x.com"));
            record.provider = Provider::Claude;
            record.organization_uuid = format!("org-{slot}");
            roster.add_record(slot, record);
            credentials::write(&store, slot, &claude_slot(slot)).unwrap();
        }
```

In the `Fake::switch_to` impl: `provider: Provider::Claude,`. Everything else (seeded rows, `run`, `run_json`, the four tests) is unchanged — the engine rules are provider-independent.

- [x] **Step 4: Point the CLI and the TUI at Claude**

`src/autoswitch.rs` `run_cli_to`: `let mut engine = Engine::new(facade, Provider::Claude, settings, args.dry_run, sink);`

`src/tui/worker.rs` `start_engine`: `Engine::new(&mut switcher, Provider::Claude, settings, dry_run, move |event| { … })`.

`src/autoswitch.rs` test `cli_parses_flags_and_runs_once`: build the fixture with `Fixture::claude(&[1, 2])` (the CLI path now asks the facade for the Claude login, and the `Fake` asserts the provider matches).

`tests/cli_claude.rs`, in `auto_once_never_refreshes_or_targets_the_live_claude_login`: the comment `// Blocked: the only other account is a Claude one, which auto never targets.` becomes `// Blocked: the live Claude slot is the only Claude account, so there is no candidate; the Codex account is never one.`; the assertions are unchanged (exit 3, `no-candidates`, zero token calls, credentials byte-identical) — they now prove the active Claude login is never refreshed by the engine that owns it.

- [x] **Step 5: Run the tests to see them pass**

Run: `env -u CODEX_HOME cargo test --test e2e_auto` and `env -u CODEX_HOME cargo test --test auto_once` and `env -u CODEX_HOME cargo test --test cli_claude auto_once`
Expected: all pass. If `once_switches_when_the_active_account_is_over_the_threshold` fails on `claude_config()["oauthAccount"]["emailAddress"]`, check what `claude_config()` returns for the synthesized vs. live object — the switch splices the slot's stored `oauthAccount`, which `world()` wrote with `claude_config(email, …)`, so the email is `user1@example.com`.

- [x] **Step 6: Run the gate and commit**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && env -u CODEX_HOME cargo test --all`
Expected: green.

```bash
git add src/autoswitch.rs src/tui/worker.rs tests/auto_once.rs tests/e2e_auto.rs tests/support/usage_mock.rs tests/cli_claude.rs
git commit -m "feat(auto): run the engine for Claude Code accounts"
```

---
### Task 3: A `keychain unavailable` active account holds like `token expired`

**Files:**
- Modify: `src/autoswitch.rs` (`Engine` gets a `security` seam; the two idle checks; tests), `src/collect.rs` (`run_pass_with`, `refresh_slot_with` and a new `live_login_for_with` become `pub(crate)`), `src/claude/keychain.rs` (`#[cfg(test)] pub(crate) mod test_support` with `FakeSecurity`)
- Test: `src/autoswitch.rs`

**Interfaces:**
- Produces: `Engine::with_security(self, security: &'a dyn SecurityCli) -> Self` (default `&SystemSecurity`); `collect::live_login_for_with(store, roster, provider, cli: &dyn SecurityCli) -> CurrentAccount`; `collect::run_pass_with` / `collect::refresh_slot_with` as `pub(crate)`; `claude::keychain::test_support::FakeSecurity { failing: bool }`; the engine helper `held_sentinel(entry: Option<&UsageEntry>) -> Option<UsageSentinel>`; the `active-idle` detail `keychain unavailable; holding until Claude Code's login is readable`.
- Consumes: Task 1 (`Fixture::claude`, `Provider` on the engine); phase 1's `run_pass_with(store, roster, opts, cli)` (`src/collect.rs:476`), `refresh_slot_with` (the `refresh_slot` delegate), `claude_live_login_with(store, roster, cli)` (private, `src/collect.rs:~157`), `Paths.keychain_enabled` (pub field), `Paths::claude_global_config_file()`, `Paths.claude_home`, `fsutil::write_json_private`.

Why the seam: the engine's unit test must make the collector see a Keychain failure for the live Claude slot, and the only Keychain the collector can be pointed at without touching the real `/usr/bin/security` is a `SecurityCli` fake. The engine therefore holds `security: &'a dyn SecurityCli` and passes it to every collector call (`run_pass_with`, `refresh_slot_with`, `live_login_for_with`); the product passes `&SystemSecurity` by default. Without this, a unit test with `keychain_enabled = true` would read the developer's real Claude Code item.

- [x] **Step 1: Write the failing test**

In `src/autoswitch.rs`'s `mod tests`, after `idle_detail_names_the_engine_provider`:

```rust
    #[test]
    fn keychain_unavailable_active_holds_like_token_expired() {
        use crate::claude::keychain::test_support::FakeSecurity;
        let mut fixture = Fixture::claude(&[1, 2]);
        // The Keychain is the live backend and cannot be read; nothing on disk
        // covers the login, so the active slot reports `keychain unavailable`.
        fixture.fake.store.paths.keychain_enabled = true;
        {
            let paths = &fixture.fake.store.paths;
            std::fs::create_dir_all(&paths.claude_home).unwrap();
            crate::fsutil::write_json_private(
                &paths.claude_global_config_file(),
                &json!({"oauthAccount": {
                    "emailAddress": "a1@example.com", "accountUuid": "u1",
                    "organizationUuid": "a1", "organizationName": null
                }}),
            )
            .unwrap();
        }
        fixture.seed(2, usage(20.0, 10.0, None));
        let mut settings = defaults();
        settings.unhealthy_ticks = 1;
        let security = FakeSecurity { failing: true };
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        let outcome = {
            let mut engine = Engine::new(
                &mut fixture.fake,
                Provider::Claude,
                settings,
                false,
                move |event| sink_log.borrow_mut().push(event.clone()),
            )
            .with_security(&security);
            engine.tick()
        };
        let events = log.borrow().clone();
        assert_eq!(outcome, TickOutcome::NoAction);
        assert!(
            matches!(&events[0], Event::Poll { headroom, .. } if headroom.get(&1) == Some(&None)),
            "the active account's usage is unknown: {events:?}"
        );
        assert_eq!(
            no_switch_reason(&events),
            Some((
                "active-idle".into(),
                "keychain unavailable; holding until Claude Code's login is readable".into()
            ))
        );
        assert!(
            fixture.fake.switches.is_empty(),
            "a cool candidate is not switched in while the login is unreadable"
        );
    }
```

- [x] **Step 2: Run the test to see it fail**

Run: `env -u CODEX_HOME cargo test --lib autoswitch::keychain_unavailable`
Expected: compile errors — `could not find test_support in keychain`, `no method named with_security`.

- [x] **Step 3: Add the fake and the collector seams**

`src/claude/keychain.rs`, at the end of the file (before or after the existing `#[cfg(test)] mod tests`):

```rust
/// Fakes shared by the engine tests: no process is spawned.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io;

    use super::{CliOutput, SecurityCli};

    /// A `security` that is either empty (every item "not found", rc 44) or
    /// broken (every call fails, as a locked or missing Keychain does).
    pub(crate) struct FakeSecurity {
        pub failing: bool,
    }

    impl SecurityCli for FakeSecurity {
        fn run(&self, _args: &[String], _stdin_line: Option<&str>) -> io::Result<CliOutput> {
            if self.failing {
                Err(io::Error::other("keychain locked"))
            } else {
                Ok(CliOutput {
                    status: 44,
                    stdout: String::new(),
                })
            }
        }
    }
}
```

`src/collect.rs`:
- `fn run_pass_with(` (line ~476) → `pub(crate) fn run_pass_with(`.
- the `refresh_slot_with` delegate (the function `refresh_slot` calls with `&SystemSecurity`, ~line 943) → `pub(crate) fn refresh_slot_with(`.
- next to `live_login_for` (line ~146), add:

```rust
/// [`live_login_for`] with an injected `security` (tests).
pub(crate) fn live_login_for_with(
    store: &Store,
    roster: &Roster,
    provider: Provider,
    cli: &dyn SecurityCli,
) -> CurrentAccount {
    match provider {
        Provider::Codex => live_login(store, roster),
        Provider::Claude => claude_live_login_with(store, roster, cli),
    }
}
```

and make `live_login_for` delegate to it with `&SystemSecurity` (if it does not already: `grep -n "fn live_login_for" src/collect.rs`).

- [x] **Step 4: Implement the seam and the hold in the engine**

`src/autoswitch.rs` imports: add `use crate::claude::keychain::{SecurityCli, SystemSecurity};`.

`Engine`: add the field `security: &'a dyn SecurityCli,` and in `new()` initialise `security: &SystemSecurity,`. Add:

```rust
    /// Replace the Keychain client (tests).
    pub fn with_security(mut self, security: &'a dyn SecurityCli) -> Self {
        self.security = security;
        self
    }
```

A free helper near `entry_headroom`:

```rust
/// The sentinels under which the active account is held rather than counted
/// unhealthy: an expired token Claude Code refreshes on its next use, or a
/// Keychain that cannot be read right now.
fn held_sentinel(entry: Option<&UsageEntry>) -> Option<UsageSentinel> {
    match entry.and_then(|e| e.sentinel) {
        Some(s @ (UsageSentinel::TokenExpired | UsageSentinel::KeychainUnavailable)) => Some(s),
        _ => None,
    }
}
```

In `tick_inner`, the `None =>` arm of the classification (lines ~570-590) becomes:

```rust
            None => {
                if let Some(held) = held_sentinel(entries.get(&current)) {
                    let since = *self.idle_hold_since.get_or_insert(now);
                    if now - since <= IDLE_HOLD_MAX_S {
                        self.unhealthy_ticks = 0;
                        self.idle_hold_slow = true;
                        let detail = match held {
                            UsageSentinel::KeychainUnavailable => {
                                "keychain unavailable; holding until Claude Code's login is readable"
                                    .to_string()
                            }
                            _ => format!(
                                "token expired while {} is idle; resumes on next use",
                                self.provider.tool_name()
                            ),
                        };
                        self.no_switch("active-idle", detail);
                        return Ok(TickOutcome::NoAction);
                    }
                    tracing::warn!(
                        "active account has idled past {IDLE_HOLD_MAX_S}s with an unreadable or expired token; counting it as unhealthy"
                    );
                } else {
                    self.idle_hold_since = None;
                }
                self.unhealthy_ticks += 1;
                // … unchanged from here (active-usage-unknown / Failover)
```

In `collect()` (lines ~736-762): use the seam for the live logins and the passes, and the helper for the escalation guard:

```rust
            if let Some(slot) =
                collect::live_login_for_with(store, roster, provider, self.security).slot()
                && !actives.contains(&slot)
            {
                actives.push(slot);
            }
        …
        let mut collected = collect::run_pass_with(store, roster, CollectOptions { … }, self.security)?;
        let active_entry = collected.entries.get(&active);
        let active_headroom = entry_headroom(&collected.entries, active, models);
        let idle = held_sentinel(active_entry).is_some();
        …
            let more = collect::run_pass_with(store, roster, CollectOptions { … }, self.security)?;
```

In the switch loop (line ~698): `collect::refresh_slot_with(self.facade.store(), &roster, target, false, self.security)`.

Borrow note: `self.security` is a `&'a dyn SecurityCli` copied out of `self` — bind it to a local (`let security = self.security;`) before the calls that also borrow `self.facade`, as the existing code does for `store`.

- [x] **Step 5: Run the tests to see them pass**

Run: `env -u CODEX_HOME cargo test --lib autoswitch::` and `env -u CODEX_HOME cargo test --lib collect::`
Expected: all pass; `expired_active_token_idles_instead_of_counting_unhealthy` still reads `token expired while Codex is idle; resumes on next use` (Codex fixture).

- [x] **Step 6: Run the gate and commit**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && env -u CODEX_HOME cargo test --all`
Expected: green.

```bash
git add src/autoswitch.rs src/collect.rs src/claude/keychain.rs
git commit -m "feat(auto): hold a keychain-unavailable active account like an expired token"
```

---

### Task 4: `auto codex` and rosters without a Claude account are refused

**Files:**
- Modify: `src/autoswitch.rs` (`CLAUDE_ONLY_NOTICE`, `run_cli_to`, tests), `tests/cli_claude.rs` (one e2e test)
- Test: `src/autoswitch.rs`, `tests/cli_claude.rs`

**Interfaces:**
- Produces: `pub const CLAUDE_ONLY_NOTICE: &str`; `run_cli_to` consumes a leading `claude` word, refuses a leading `codex` word and a roster with no Claude slots (stderr `Error: <notice>`, exit `1`, before parsing flags / before any tick).
- Consumes: `Provider::parse_selector`, `Roster::slots_of(Provider::Claude)`.

- [x] **Step 1: Write the failing unit test**

In `src/autoswitch.rs`'s `mod tests`, after `cli_parses_flags_and_runs_once`:

```rust
    #[test]
    fn auto_refuses_codex_and_rosters_without_a_claude_account() {
        let argv = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A Codex-only roster: refused before any tick (no poll event, exit 1).
        let mut codex_only = Fixture::new(&[1, 2]);
        codex_only.seed(1, usage(95.0, 10.0, None));
        codex_only.seed(2, usage(10.0, 10.0, None));
        let mut out = Vec::new();
        assert_eq!(
            run_cli_to(argv(&["--once", "--json"]), &mut codex_only.fake, &mut out),
            1
        );
        assert!(out.is_empty(), "no event is written: {}", String::from_utf8_lossy(&out));
        assert!(codex_only.fake.switches.is_empty());

        // `auto codex` is refused even with Claude accounts around.
        let mut claude = Fixture::claude(&[1, 2]);
        claude.seed(1, usage(95.0, 10.0, None));
        claude.seed(2, usage(10.0, 10.0, None));
        let mut out = Vec::new();
        assert_eq!(
            run_cli_to(argv(&["codex", "--once", "--json"]), &mut claude.fake, &mut out),
            1
        );
        assert!(out.is_empty());

        // `auto claude` is the bare form.
        let mut out = Vec::new();
        assert_eq!(
            run_cli_to(
                argv(&["claude", "--once", "--json", "--dry-run"]),
                &mut claude.fake,
                &mut out
            ),
            TickOutcome::Switched.code()
        );
        assert!(String::from_utf8_lossy(&out).contains("\"event\":\"switch\""));
        assert!(claude.fake.switches.is_empty(), "dry-run");
    }
```

- [x] **Step 2: Run the test to see it fail**

Run: `env -u CODEX_HOME cargo test --lib autoswitch::auto_refuses`
Expected: FAIL — the Codex-only run returns `2`/`3` and writes a poll event; the `codex` word makes clap exit `2` (`unexpected argument 'codex'`).

- [x] **Step 3: Implement the notice and the checks**

`src/autoswitch.rs`, next to `IDLE_HOLD_MAX_S`:

```rust
/// Why `auto` is Claude-only (spec §9): Codex sessions keep the account they
/// started with until they restart, so switching under them achieves nothing.
pub const CLAUDE_ONLY_NOTICE: &str = "Auto-switch covers Claude Code accounts only: Codex sessions do not pick up a switched account without a restart. Add a Claude Code account with 'ccsw add claude' first.";
```

`run_cli_to` (line ~1264): accept the selector word before clap sees the argv, and check the roster after `root_guard`:

```rust
pub fn run_cli_to(argv: Vec<String>, facade: &mut dyn AutoFacade, out: &mut dyn Write) -> i32 {
    let mut argv = argv;
    match argv.first().and_then(|word| Provider::parse_selector(word)) {
        Some(Provider::Claude) => {
            argv.remove(0);
        }
        Some(Provider::Codex) => {
            eprintln!("Error: {CLAUDE_ONLY_NOTICE}");
            return 1;
        }
        None => {}
    }
    let args =
        match AutoArgs::try_parse_from(std::iter::once("ccsw auto".to_string()).chain(argv)) {
            Ok(args) => args,
            Err(err) => {
                let _ = err.print();
                return err.exit_code();
            }
        };
    if let Some(code) = root_guard() {
        return code;
    }
    match facade.roster() {
        Ok(roster) if roster.slots_of(Provider::Claude).is_empty() => {
            eprintln!("Error: {CLAUDE_ONLY_NOTICE}");
            return 1;
        }
        Ok(_) => {}
        Err(err) => {
            eprintln!("Error: {err}");
            return 1;
        }
    }
    // … unchanged: --debug, settings, banner, engine
```

- [x] **Step 4: Write the e2e test**

`tests/cli_claude.rs`, after `auto_once_never_refreshes_or_targets_the_live_claude_login` (imports already include `CLAUDE_OK`, `claude_config`, `claude_creds`, `usage_mock`):

```rust
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
        &claude_creds(&usage_mock::claude_live_refresh_token("one@example.com"), CLAUDE_OK),
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
```

- [x] **Step 5: Run the tests to see them pass**

Run: `env -u CODEX_HOME cargo test --lib autoswitch::` and `env -u CODEX_HOME cargo test --test cli_claude auto_`
Expected: all pass.

- [x] **Step 6: Run the gate and commit**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && env -u CODEX_HOME cargo test --all`
Expected: green.

```bash
git add src/autoswitch.rs tests/cli_claude.rs
git commit -m "feat(auto): accept the claude selector and refuse codex and rosters without a Claude account"
```

---
### Task 5: Events move to `schemaVersion: 2` and carry `provider`

**Files:**
- Modify: `src/autoswitch.rs` (`Event::to_json`, `write_event`, `run_cli_to`, the events test), `tests/auto_once.rs` (two assertions), `tests/e2e_auto.rs` (assertions)
- Test: `src/autoswitch.rs`, `tests/auto_once.rs`, `tests/e2e_auto.rs`

**Interfaces:**
- Produces: `Event::to_json(&self, provider: Provider) -> Value` emitting `{"schemaVersion": 2, "event": …, "ts": …, "provider": "<provider>", …}`; `write_event(out, event, provider, json)`.
- Consumes: Task 2 (the CLI engine is Claude). The TUI only uses `Event::human()` and is untouched. `autoswitch_state.json` keeps `schemaVersion: 1` (`src/store/state.rs` untouched; its pins in `tests/auto_once.rs:270`, `tests/e2e_auto.rs` and `src/store/state.rs` stay at `1`).

- [x] **Step 1: Update the failing assertions**

`src/autoswitch.rs`, test `events_serialize_per_contract`: every `.to_json()` call in the test becomes `.to_json(Provider::Claude)`, and the header assertions become:

```rust
        let json = poll.to_json(Provider::Claude);
        assert_eq!(json["schemaVersion"], 2);
        assert_eq!(json["provider"], "claude");
        assert_eq!(json["event"], "poll");
```

Add, at the end of that test, a check that the key order is header-first and that a Codex engine would label itself (the engine is generic even though the product is Claude):

```rust
        let sleep = Event::Sleep {
            seconds: 12.34,
            until: "2026-10-08T00:00:00Z".into(),
        };
        let json = sleep.to_json(Provider::Codex);
        assert_eq!(json["provider"], "codex");
        let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert_eq!(keys[..4], [&"schemaVersion".to_string(), &"event".to_string(), &"ts".to_string(), &"provider".to_string()][..]);
```

(serde_json without `preserve_order` sorts keys alphabetically — if that assertion fails for that reason, replace it with `assert!(json.get("provider").is_some())` and note it in the report; the contract is the key set, not the order.)

`tests/auto_once.rs`, in `dry_run_reports_the_switch_and_writes_nothing` (line ~191): `assert_eq!(poll["schemaVersion"], 2); assert_eq!(poll["provider"], "claude");` and on the switch event `assert_eq!(switch["provider"], "claude");`. Leave `raw["schemaVersion"] == 1` for the state file (line ~270) as is.

`tests/e2e_auto.rs`, in `once_switches_when_the_active_account_is_over_the_threshold`, after `assert_eq!(poll["event"], "poll");`: `assert_eq!(poll["schemaVersion"], 2); assert_eq!(poll["provider"], "claude");` and after `assert_eq!(switch["event"], "switch");`: `assert_eq!(switch["provider"], "claude");`. The state-file `schemaVersion == 1` assertion stays.

- [x] **Step 2: Run the tests to see them fail**

Run: `env -u CODEX_HOME cargo test --lib autoswitch::events_serialize`
Expected: compile error `this method takes 0 arguments but 1 argument was supplied`.

- [x] **Step 3: Implement**

`src/autoswitch.rs`, `Event::to_json` (line ~148):

```rust
    /// `{"schemaVersion": 2, "event": kind, "ts": now, "provider": provider, …}`.
    pub fn to_json(&self, provider: Provider) -> Value {
        let mut map = Map::new();
        map.insert("schemaVersion".into(), json!(2));
        map.insert("event".into(), json!(self.kind()));
        map.insert("ts".into(), json!(now_iso()));
        map.insert("provider".into(), json!(provider.as_str()));
        match self {
            // … unchanged
```

`write_event` (line ~1195): `fn write_event(out: &mut dyn Write, event: &Event, provider: Provider, json: bool)` with `writeln!(out, "{}", event.to_json(provider))`.

`run_cli_to`: `let sink = |event: &Event| write_event(out, event, Provider::Claude, json);`.

- [x] **Step 4: Run the tests to see them pass**

Run: `env -u CODEX_HOME cargo test --lib autoswitch::` and `env -u CODEX_HOME cargo test --test auto_once` and `env -u CODEX_HOME cargo test --test e2e_auto`
Expected: all pass.

- [x] **Step 5: Run the gate and commit**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && env -u CODEX_HOME cargo test --all`
Expected: green.

```bash
git add src/autoswitch.rs tests/auto_once.rs tests/e2e_auto.rs
git commit -m "feat(auto): events carry the provider and move to schemaVersion 2"
```

---

### Task 6: The `autoswitch.model` typo guard

**Files:**
- Modify: `src/autoswitch.rs` (`Engine` field, `warn_unknown_models`, call site in `tick_inner`, tests)
- Test: `src/autoswitch.rs`

**Interfaces:**
- Produces: `Engine.model_warning_pending: bool`; private `Engine::warn_unknown_models(&mut self, entries: &BTreeMap<u32, UsageEntry>, slots: &[u32])`; the `config-warning` event text `autoswitch.model: <names> matches no account's usage windows — only the 5h/7d limits are being watched for it (typo?)` (names joined with `,` in configured order, original spelling).
- Consumes: `Event::ConfigWarning { message }` (exists, never emitted until now; `kind()` = `config-warning`, `human()` = `warning: {message}`), `NormalizedUsage.scoped: Vec<ScopedWindow { name, pct, resets_at }>`, `UsageEntry.last_good: Option<NormalizedUsage>`, `UsageEntry.sentinel`, `AutoSwitchSettings::model_names()` (comma-split, trimmed, case-insensitively deduped).

Rules (cswap §6.12): `wanted` = the configured names minus `all` (case-insensitive); a bare `all` never warns. The check runs at most once per engine run, on the first tick where every slot in the pass that is not an API-key account has a readable usage dict (`last_good`); if some slot is unreadable the check waits for a later tick. `seen` = the lowercased `scoped[].name` across those slots; every wanted name not in `seen` goes into one warning. The check never forces a fetch.

- [x] **Step 1: Write the failing test**

In `src/autoswitch.rs`'s `mod tests`:

```rust
    fn usage_with_pool(five_hour: f64, pool: &str) -> NormalizedUsage {
        let mut u = usage(five_hour, 10.0, None);
        u.scoped = vec![crate::model::ScopedWindow {
            name: pool.to_string(),
            pct: 10.0,
            resets_at: None,
        }];
        u
    }

    fn config_warnings(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::ConfigWarning { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn model_typo_warns_once_and_known_names_never_do() {
        let mut fixture = Fixture::claude(&[1, 2]);
        fixture.seed(1, usage_with_pool(50.0, "Fable"));
        fixture.seed(2, usage_with_pool(10.0, "Fable"));

        // A typo warns exactly once per run, and the engine keeps watching 5h/7d.
        let mut settings = defaults();
        settings.model = Some("Fabel".into());
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let sink_log = log.clone();
        {
            let mut engine = Engine::new(
                &mut fixture.fake,
                Provider::Claude,
                settings,
                true,
                move |event| sink_log.borrow_mut().push(event.clone()),
            );
            assert_eq!(engine.tick(), TickOutcome::NoAction, "50% < 90%");
            assert_eq!(engine.tick(), TickOutcome::NoAction);
        }
        let events = log.borrow().clone();
        assert_eq!(
            config_warnings(&events),
            vec![
                "autoswitch.model: Fabel matches no account's usage windows — only the 5h/7d limits are being watched for it (typo?)"
                    .to_string()
            ],
            "{events:?}"
        );
        assert_eq!(
            kinds(&events)[..2],
            ["poll", "config-warning"],
            "the warning follows the first poll"
        );

        // A known name, `all`, and a mixed list warn only for the unknown part.
        for (model, expected) in [
            ("Fable", Vec::<String>::new()),
            ("all", Vec::new()),
            ("FABLE,all", Vec::new()),
            ("Fable,Opus", vec!["autoswitch.model: Opus matches no account's usage windows — only the 5h/7d limits are being watched for it (typo?)".to_string()]),
        ] {
            let mut settings = defaults();
            settings.model = Some(model.into());
            let (_, events) = tick(&mut fixture, settings, true);
            assert_eq!(config_warnings(&events), expected, "model = {model}");
        }

        // An unreadable slot defers the check instead of guessing.
        let mut fixture = Fixture::claude(&[1, 2]);
        fixture.seed(1, usage_with_pool(50.0, "Fable"));
        fixture.seed_failure(2, "http-500");
        let mut settings = defaults();
        settings.model = Some("Fabel".into());
        let (_, events) = tick(&mut fixture, settings, true);
        assert!(config_warnings(&events).is_empty(), "{events:?}");
    }
```

- [x] **Step 2: Run the test to see it fail**

Run: `env -u CODEX_HOME cargo test --lib autoswitch::model_typo`
Expected: FAIL — `config_warnings` is empty for `Fabel`.

- [x] **Step 3: Implement**

`src/autoswitch.rs`: `use std::collections::{BTreeMap, BTreeSet};`. Add the field `model_warning_pending: bool,` to `Engine` and initialise it in `new()` as

```rust
            model_warning_pending: models.iter().any(|m| !m.eq_ignore_ascii_case("all")),
```

(place the line after `models` is computed and before `models` is moved into the struct). Add the method:

```rust
    /// cswap's typo guard: once per run, on the first tick where every slot in
    /// the pass that is not an API-key account has a readable usage dict, warn
    /// about configured model names that no scoped window reports. `all` never
    /// warns, and the check never fetches.
    fn warn_unknown_models(&mut self, entries: &BTreeMap<u32, UsageEntry>, slots: &[u32]) {
        if !self.model_warning_pending {
            return;
        }
        let wanted: Vec<String> = self
            .models
            .iter()
            .filter(|m| !m.eq_ignore_ascii_case("all"))
            .cloned()
            .collect();
        if wanted.is_empty() {
            self.model_warning_pending = false;
            return;
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for slot in slots {
            let entry = entries.get(slot);
            if entry.and_then(|e| e.sentinel) == Some(UsageSentinel::ApiKey) {
                continue;
            }
            let Some(usage) = entry.and_then(|e| e.last_good.as_ref()) else {
                return; // not every slot is readable yet: try again next tick
            };
            seen.extend(usage.scoped.iter().map(|w| w.name.to_ascii_lowercase()));
        }
        self.model_warning_pending = false;
        let missing: Vec<String> = wanted
            .into_iter()
            .filter(|m| !seen.contains(&m.to_ascii_lowercase()))
            .collect();
        if !missing.is_empty() {
            self.emit(Event::ConfigWarning {
                message: format!(
                    "autoswitch.model: {} matches no account's usage windows — only the 5h/7d limits are being watched for it (typo?)",
                    missing.join(",")
                ),
            });
        }
    }
```

Call it in `tick_inner` right after the `Event::Poll` is emitted (line ~535) and before the API-key gate:

```rust
        let in_pass: Vec<u32> = std::iter::once(current)
            .chain(candidates.iter().copied())
            .collect();
        self.warn_unknown_models(&entries, &in_pass);
```

- [x] **Step 4: Run the tests to see them pass**

Run: `env -u CODEX_HOME cargo test --lib autoswitch::`
Expected: all pass.

- [x] **Step 5: Run the gate and commit**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && env -u CODEX_HOME cargo test --all`
Expected: green.

```bash
git add src/autoswitch.rs
git commit -m "feat(auto): warn once about autoswitch.model names no account reports"
```

---
### Task 7: The TUI auto view shows Claude only

**Files:**
- Modify: `src/tui/snapshot.rs` (`AccountsSnapshot::only`), `src/tui/auto.rs` (`engine_available`, `without_claude`, badge, key handling, tests), `src/tui/app.rs` (`open_auto`, `draw_auto`), `src/tui/modals.rs` (`go_live` copy), `tests/tui_render.rs` (the auto test on the mixed fixture, the no-account test, the modal copy)
- Test: `src/tui/auto.rs`, `src/tui/app.rs`, `tests/tui_render.rs`

**Interfaces:**
- Produces: `AccountsSnapshot::only(&self, provider: Provider) -> AccountsSnapshot` (same `active_number` and `taken_at`, only that provider's accounts); `AutoScreen::without_claude(settings, stamp) -> AutoScreen` (no engine; first log line `— no Claude Code account: auto-switch covers Claude Code only (ccsw add claude) —`; badge ` OFF `); `AutoScreen::engine_available(&self) -> bool`; `App::open_auto` starts no engine when the current snapshot has no Claude account.
- Consumes: Task 2 (the worker starts a Claude engine); `AccountSnapshot.provider`; `tests/tui_render.rs` `mixed_fixture()` (phase 1: Claude accounts `6 bob@gmail.com` active with `$$`/7d 100/`Fable`, `7 bob@work.com`) and `fixture()` (Codex only); `App::apply_snapshot(snapshot, generation, now)`.

- [x] **Step 1: Write the failing unit tests**

`src/tui/auto.rs` `mod tests`, after `opens_in_dry_run_and_needs_confirm_to_go_live`:

```rust
    #[test]
    fn without_a_claude_account_there_is_no_engine_to_start() {
        let mut auto = AutoScreen::without_claude(settings(), "20:39:01");
        assert!(!auto.engine_available());
        assert_eq!(
            auto.log()[0].text,
            "— no Claude Code account: auto-switch covers Claude Code only (ccsw add claude) —"
        );
        assert_eq!(auto.badge(&DARK).content, " OFF ");
        assert!(
            auto.handle_key(key(KeyCode::Char('l')), "20:39:02").is_empty(),
            "Go live does nothing"
        );
        assert!(
            auto.handle_key(key(KeyCode::Char('t')), "20:39:02").is_empty(),
            "the threshold is not adjustable without an engine"
        );
        assert!(!auto.adjusting());
        assert_eq!(
            auto.handle_key(key(KeyCode::Esc), "20:39:03"),
            vec![Effect::Pop]
        );
    }
```

`src/tui/snapshot.rs` `mod tests` (next to the `grouped` test):

```rust
    #[test]
    fn only_keeps_one_providers_accounts() {
        let mut snapshot = AccountsSnapshot::empty(0.0);
        snapshot.active_number = Some(1);
        snapshot.accounts = vec![
            crate::tui::test_support::account(1, "a@x", true, None),
            crate::tui::test_support::claude_account(2, "b@x", false, None),
        ];
        let claude = snapshot.only(Provider::Claude);
        assert_eq!(claude.active_number, Some(1));
        assert_eq!(claude.accounts.len(), 1);
        assert_eq!(claude.accounts[0].number, 2);
        assert!(snapshot.only(Provider::Codex).accounts.iter().all(|a| a.provider == Provider::Codex));
    }
```

(Use the `test_support` builders with the argument shapes they have — `grep -n "pub fn" src/tui/test_support.rs` — the point is one account per provider.)

`tests/tui_render.rs`: change `auto_screen_badge_summary_candidates_log_and_threshold` to run on the mixed fixture and expect the Claude rows. Replace its opening with

```rust
    let mut app = App::new(TuiStart::Dashboard, ThemeName::Dark, 90.0, None);
    app.apply_snapshot(mixed_fixture(), 1, NOW);
    assert_eq!(
        app.handle_key(key(KeyCode::Char('g')), NOW),
        vec![Command::OpenAuto]
    );
```

and the account expectations with:

```rust
    let (_, header) = find_row(&rows, "bob@gmail.com");
    assert!(header.starts_with("    6  bob@gmail.com"), "{header}");
    assert!(header.contains("● active"), "{header}");
    assert!(
        rows.iter().all(|r| !r.contains("john.doe@gmail.com") && !r.contains("alice@corp.io")),
        "no Codex account on the auto view"
    );
    let (next_y, _) = find_row(&rows, "Next best");
    assert!(rows[next_y + 1].starts_with("     7  bob@work.com"), "{}", rows[next_y + 1]);
    assert!(rows[next_y + 1].ends_with("% used"), "{}", rows[next_y + 1]);
    assert!(
        rows.get(next_y + 2).is_none_or(|r| !r.contains("@")),
        "bob@work.com is the only candidate: {:?}",
        rows.get(next_y + 2)
    );
```

(Keep the badge, summary, threshold-adjust, modal and log assertions; the `5h` tick-mark assertion stays if the Claude card has a 5h bar — it does.) Change the modal assertion `rows[title_y + 2].contains("Go live? ccsw will switch your active account")` to `contains("Go live? ccsw will switch your active Claude Code account")`.

Add a new render test:

```rust
#[test]
fn auto_view_without_a_claude_account_shows_the_notice_and_starts_no_engine() {
    let mut app = app_with(TuiStart::Dashboard);
    let commands = app.open_auto(AutoSwitchSettings::default(), NOW);
    assert_eq!(commands, vec![Command::Refresh { full: false }], "no engine: {commands:?}");
    let rows = screen_rows(&render(&mut app, 100, 30, NOW));
    assert!(rows.iter().any(|r| r.contains(" OFF ")), "{rows:?}");
    assert!(
        rows.iter().any(|r| r.contains(
            "— no Claude Code account: auto-switch covers Claude Code only (ccsw add claude) —"
        )),
        "{rows:?}"
    );
    let commands = app.handle_key(key(KeyCode::Char('l')), NOW);
    assert!(commands.is_empty(), "Go live is inert: {commands:?}");
}
```

- [x] **Step 2: Run the tests to see them fail**

Run: `env -u CODEX_HOME cargo test --lib tui::` and `env -u CODEX_HOME cargo test --test tui_render auto_`
Expected: compile errors (`without_claude`, `engine_available`, `only` missing); the render test expects Claude rows the Codex-only view does not show.

- [x] **Step 3: Implement**

`src/tui/snapshot.rs`:

```rust
    /// The same snapshot restricted to one provider's accounts.
    pub fn only(&self, provider: Provider) -> AccountsSnapshot {
        AccountsSnapshot {
            active_number: self.active_number,
            accounts: self
                .accounts
                .iter()
                .filter(|a| a.provider == provider)
                .cloned()
                .collect(),
            taken_at: self.taken_at,
        }
    }
```

`src/tui/auto.rs`: add the field `engine_available: bool` to `AutoScreen` (set `true` in `new`), and

```rust
    /// The view for a roster without a Claude Code account: a notice, no engine.
    pub fn without_claude(settings: AutoSwitchSettings, stamp: &str) -> Self {
        let mut screen = Self {
            configured_threshold: settings.threshold,
            settings,
            dry_run: true,
            adjusting: false,
            adjust_start: 0.0,
            log: Vec::new(),
            engine_available: false,
        };
        screen.push_system(
            "— no Claude Code account: auto-switch covers Claude Code only (ccsw add claude) —",
            stamp,
        );
        screen
    }

    pub fn engine_available(&self) -> bool {
        self.engine_available
    }
```

In `handle_key`, first arm: when `!self.engine_available`, only `Esc` / `q` pop (`vec![Effect::Pop]`), every other key returns `Vec::new()`. In `badge`, when `!self.engine_available` return `Span::styled(" OFF ", p.muted_style())` before the dry-run/live branches.

`src/tui/app.rs` `open_auto`:

```rust
    pub fn open_auto(&mut self, settings: AutoSwitchSettings, now: f64) -> Vec<Command> {
        if self.screen_kind() == ScreenKind::Auto {
            return Vec::new();
        }
        self.threshold_pct = Some(settings.threshold);
        let stamp = clock_stamp(now);
        let has_claude = self
            .snapshot
            .as_ref()
            .is_some_and(|s| s.accounts.iter().any(|a| a.provider == Provider::Claude));
        if !has_claude {
            self.screens
                .push(Screen::Auto(AutoScreen::without_claude(settings, &stamp)));
            return vec![Command::Refresh { full: false }];
        }
        let screen = AutoScreen::new(settings.clone(), &stamp);
        self.screens.push(Screen::Auto(screen));
        vec![
            Command::StartEngine {
                settings,
                dry_run: true,
            },
            Command::Refresh { full: false },
        ]
    }
```

`draw_auto`: restrict the snapshot once at the top and pass the restricted one to both `accounts_panel` and `candidate_lines`:

```rust
    let claude = snapshot.map(|s| s.only(Provider::Claude));
    let snapshot = claude.as_ref();
```

(then the existing `accounts_panel(snapshot, …)` and `auto.candidate_lines(snapshot, p)` calls are unchanged). Import `crate::provider::Provider` in `app.rs` if missing.

`src/tui/modals.rs` `go_live()` body: `"Go live? ccsw will switch your active Claude Code account automatically when the threshold is reached.\n\n(Same behavior as running `ccsw auto` in a terminal.)"`.

- [x] **Step 4: Run the tests to see them pass**

Run: `env -u CODEX_HOME cargo test --lib tui::` and `env -u CODEX_HOME cargo test --test tui_render`
Expected: all pass. If the mixed-fixture auto test's exact row prefixes differ by a column, print the rows (`{rows:?}`) and adjust the `starts_with` strings to the rendered output, keeping the assertions on account numbers and emails.

- [x] **Step 5: Run the gate and commit**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && env -u CODEX_HOME cargo test --all`
Expected: green.

```bash
git add src/tui/snapshot.rs src/tui/auto.rs src/tui/app.rs src/tui/modals.rs tests/tui_render.rs
git commit -m "feat(tui): the auto view shows the Claude Code account and candidates only"
```

---

### Task 8: Help text, README and CHANGELOG

**Files:**
- Modify: `src/autoswitch.rs` (`AUTO_EPILOG`, `AutoArgs` `about` and `--model` help), `src/cli/legacy.rs` (lines ~373, ~430), `README.md` (intro lines 5-8, "Automatic switching"), `CHANGELOG.md` (`## Unreleased`)
- Test: `src/autoswitch.rs` (`cli_parses_flags_and_runs_once` still parses `--help` → 0)

**Interfaces:**
- Consumes: Tasks 2–7.

- [x] **Step 1: Help text**

`src/autoswitch.rs`:

```rust
const AUTO_EPILOG: &str = "Exit codes with --once:
  0  switched to another account
  1  error (network trouble, lock contention, no Claude Code account, ...)
  2  no action needed
  3  blocked: wanted to switch but no viable target / all exhausted

Auto-switch covers Claude Code accounts: a running Claude Code session picks the
new login up by itself (next message, or ~30 s with the macOS Keychain). Codex
sessions keep the account they started with until they restart, so there is no
Codex auto-switch; use `ccsw switch` and restart the session instead.

Examples:
  ccsw auto                       # foreground loop, switch at 90% used
  ccsw auto claude                # the same (claude is the only provider)
  ccsw auto --threshold 80        # switch earlier
  ccsw auto --model Fable         # also switch when that model's weekly limit is hit
  ccsw auto --json                # one JSON event per line (for scripts)
  ccsw auto --once; echo $?       # single tick, outcome in exit code
  ccsw auto --dry-run             # log decisions, never actually switch

Defaults live in settings.json in the backup root; flags override them.";
```

`AutoArgs`: `about = "Automatically switch Claude Code accounts when the active one nears its 5h/7d rate limit. Runs a foreground polling loop; use --once for a single tick (cron-friendly)."`, and the `--model` doc comment: `/// Also switch when a per-model weekly limit is hit, not just the account-wide 5h/7d windows. One pool name or a comma-separated list of the model pools an account reports (e.g. Fable), or 'all' for every per-model window`.

`src/cli/legacy.rs`: line ~373 `  ccsw auto [claude]              auto-switch Claude Code accounts near their rate limits` (keep the column alignment of the surrounding lines); line ~430 `  ccsw auto --once                       # single auto-switch tick for Claude Code (cron-friendly)`.

- [x] **Step 2: README**

Intro (lines 5-8): replace `let it\nswitch Codex accounts for you before you hit a rate limit, and run two Codex accounts side\nby side in different terminals. Auto-switch, session mode and export/import cover Codex\naccounts only for now; Claude support for them comes in later phases.` with:

```
let it
switch Claude Code accounts for you before you hit a rate limit, and run two Codex accounts
side by side in different terminals. Session mode and export/import cover Codex accounts
only for now; Claude support for them comes in later phases.
```

"Automatic switching" section (lines 139-149):

````markdown
### Automatic switching (Claude Code)

```bash
ccsw auto                    # foreground loop, switch Claude Code accounts at 90% used
ccsw auto --threshold 80     # switch earlier
ccsw auto --once             # one tick, outcome in the exit code (0 switched, 1 error, 2 nothing to do, 3 blocked)
ccsw auto --dry-run          # log what it would do, never switch
ccsw auto --json             # one JSON event per line (schemaVersion 2, provider "claude")
```

Auto-switch covers Claude Code accounts only. A running Claude Code session picks the new
login up by itself (on the next message, or within about 30 seconds with the macOS Keychain),
so switching early keeps you working. Codex sessions keep the account they started with
until they restart — Codex CLI's app-server daemon loads `auth.json` once and re-reads it
only for the account it already holds — so there is no Codex auto-switch; use `ccsw
switch` and restart the session. `ccsw auto` on a roster without a Claude Code account
exits 1 and says so. While the Keychain is locked, the engine holds (`active-idle`) rather
than failing over.

Defaults live in `settings.json`; change them with `ccsw config set autoswitch.threshold 80`.
`autoswitch.model` names that no account reports produce one `config-warning` event.
````

- [x] **Step 3: CHANGELOG**

Above `## v0.3.0 — 2026-10-08`:

```markdown
## Unreleased

### Changed

- `ccsw auto` now switches Claude Code accounts. Codex auto-switch is withdrawn: a
  Codex session keeps the account it started with until it restarts (the app-server daemon
  loads `auth.json` once and re-reads it only for the account it already holds), so an
  automatic switch could never reach the session that hit the limit. `ccsw auto` on a
  roster without a Claude Code account, and `ccsw auto codex`, exit 1 with an explanation.
- `auto --json` events are `schemaVersion: 2` and carry `provider: "claude"`.
- The TUI auto view shows the Claude Code active card and candidates; the `Go live`
  confirmation names Claude Code. Without a Claude Code account the view shows a notice
  instead of starting an engine.

### Added

- A `keychain unavailable` active account is held like an expired token (`no-switch
  active-idle`) instead of counting toward failover.
- `autoswitch.model` names that no account's usage windows report produce one
  `config-warning` event per run.
```

- [x] **Step 4: Run the gate and commit**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && env -u CODEX_HOME cargo test --all`
Expected: green (`--help` still exits 0 in `cli_parses_flags_and_runs_once`).

```bash
git add src/autoswitch.rs src/cli/legacy.rs README.md CHANGELOG.md
git commit -m "docs: auto-switch covers Claude Code only — help text, README and changelog"
```

---

## Deviations from the spec recorded by this plan

- The engine keeps a `provider` field (spec §9 last bullet) and the unit tests keep a Codex fixture for the provider-independent engine rules; nothing in the product constructs a Codex engine.
- `Engine::with_security` and the `pub(crate)` collector seams (`run_pass_with`, `refresh_slot_with`, `live_login_for_with`) exist only so the Keychain-unavailable hold can be unit-tested without touching the real Keychain; the product always passes `&SystemSecurity`.
- The TUI's "no Claude Code account" decision reads the snapshot the dashboard already holds; if the auto view is opened before the first snapshot arrives (loading state), it shows the notice. Reopening after the snapshot arrives starts the engine.
- The model-name warning skips API-key slots and waits for every other slot in the pass to be readable, as cswap does; it is evaluated against the slots of the current pass, not every roster slot.

## Self-review notes

- Spec coverage: §9 bullets 1–6 → Tasks 2, 4, 3, 5, 6, (state file untouched — §5); §6.1 `auto claude` → Task 4; §6.2 `auto` row → Task 4, `config` row → Task 6; §6.3 → Task 5; §12 auto view → Task 7; §15 `auto --once` on a mixed roster → Task 2 (`tests/e2e_auto.rs` and the kept `tests/cli_claude.rs` test); §16 → all.
- Type consistency: `Engine::new(facade, provider, settings, dry_run, sink)` (Task 1) is used by Tasks 2–7; `AutoFacade::current_account(provider)` (Task 1) by the `Fake`s in Tasks 1–6 and `tests/auto_once.rs`; `Fixture::claude` (Task 1) by Tasks 2–6; `with_security` / `FakeSecurity` (Task 3) by Task 3 only; `to_json(provider)` / `write_event(out, event, provider, json)` (Task 5) by Task 5; `AccountsSnapshot::only`, `AutoScreen::without_claude`, `engine_available` (Task 7) by Task 7.
- Placeholder scan: no TBD/TODO; every code step shows the code; the two "if this assertion fails for a layout reason" notes name the exact fallback.
- Review Focus: items 1–5 each name their task and test; all five are new tests in this plan.
