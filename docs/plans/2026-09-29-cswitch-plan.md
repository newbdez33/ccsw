# cswitch — implementation plan

Spec: `docs/specs/2026-09-29-cswitch-design.md`. Research: `docs/research/*.md`.

The crate skeleton (`src/errors.rs`, `src/model.rs`, `src/paths.rs`, `src/fsutil.rs`,
`src/lib.rs`, `src/main.rs`, placeholder modules) is committed first so every module can
be built in parallel against fixed types. Each task owns the files listed for it and
must not edit files owned by another task in the same wave. Every task: TDD (test first),
`cargo fmt`, `cargo clippy --all-targets -- -D warnings` clean for its files,
`cargo test` green, English comments only where the reason is not obvious.

Use `CARGO_TARGET_DIR=target/<task>` while several tasks build at once.

## Wave 1 — foundations (parallel)

### Task A — store layer and printer
Files: `src/store/{mod,roster,credentials,settings,mappings,state}.rs`, `src/printer.rs`.

- `store::Store { paths: Paths }` with `lock()` (10 s) and `lock_state()`; directories
  created lazily by writers (`ensure_dirs`).
- `roster`: `Roster` read/write (strict read per cswap: corrupt → `ConfigError` naming
  the file; absent → `None`), `init_if_absent`, `next_free_slot`, `find_slot(identity)`,
  `record(n)`, `set_active`, `add_record`, `remove_slot`, `set_alias/unset_alias`,
  `set_disabled`, `move_slot`/`swap_slots` (sequence renumber + sort), `switchable_slots`
  (has credentials, not disabled). Slot keys are strings, `sequence` sorted ints.
- `credentials`: `read(n) -> Option<AuthJson>`, `write(n, &AuthJson)` (keeps `.prev` when
  the value differs), `delete(n)`, `exists(n)`, `fingerprint(n)` (SHA-256 of the
  refresh token when present, else of the whole file).
- `settings`: `SETTING_SPECS` registry (9 keys, ranges, defaults, help), `Settings`
  (forgiving load with clamping), `AutoSwitchSettings`, `config_list/get/set/unset/path`
  helpers with the exact cswap messages, `parse_model_names`, `format_setting_value`,
  `merged_with_cli`.
- `mappings`: `MappingStore` (`normalize_path`, `set`, `remove`, `resolve` nearest
  ancestor, `prune(identity)`, list sorted).
- `state`: `AutoSwitchState` read/modify/write under `.autoswitch_state.lock`
  (`lastSwitchAt/To/From`, `quarantine`).
- `printer`: color precedence (`NO_COLOR`, `FORCE_COLOR`, tty, `TERM=dumb`), dark/light
  palettes (xterm 173 accent etc.), `accent/muted/dimmed/bolded/warning/error`,
  `format_age`, `format_duration`, `countdown`, `clock`, `yes_no_prompt`.

### Task B — Codex mechanics
Files: `src/codex/{mod,auth,jwt,usage,oauth,app_server}.rs`, `tests/codex_mock.rs`.

- `jwt`: `decode_payload`, `AccountInfo::from_auth(&AuthJson)` (claims per porting notes
  §2), `PlanKind` + labels, `token_expires_at`, `is_expiring(margin)`.
- `auth`: `AuthJson` (serde, preserves unknown keys via `serde_json::Value` inner or
  `#[serde(flatten)]`), `kind()` (`ChatGpt`/`ApiKey`/`Unknown`), `identity()`,
  `read(path)`, `write(path)` (atomic 0600 via fsutil), `backup_live(path)`
  (`auth.json.bak.<nanos>`, keep 3), `apply_tokens(id, access, refresh)` + `last_refresh`
  stamp, `refresh_token()`, `is_newer_than(&other)` (freshness rule), `sha256`.
- `usage`: `UsageClient::new(proxy)`, `fetch(&AuthJson) -> Result<FetchOutcome, FetchError>`
  with the classification of spec §8.1 (`FetchError::{Http(u16, retry_after), Timeout,
  Network, BadResponse, Auth(TerminalAuth)}`), `parse_usage(&Value) -> NormalizedUsage`
  (spec §8.2, porting notes §4.5, free-plan remap, limited, credits), `headroom(&usage,
  models)`, `relevant_windows`, `binding_pct`, `limiting_reset`, `earliest_future_reset`,
  pace (`compute_pace`, `expected_pct`, `ahead`, `projected_exhaustion`, `will_last`).
  Endpoint override `CSWITCH_USAGE_URL`.
- `oauth`: `refresh(&client, refresh_token) -> Result<RefreshedTokens, RefreshError>`
  (terminal codes, memorable subset), endpoint override `CSWITCH_TOKEN_URL`.
- `app_server`: port of codex-switch `app_server.rs` (`LiveAuthSnapshot`,
  `restart_daemon_if_live_auth_changed`, `DaemonRestart`, messages per spec §4) and
  `codex_supports_no_daemon(command)`, `embedded_codex_argv`, `command_on_path`.
- Mock server test (axum): usage 200/429/401→refresh→200, refresh terminal verdicts.

### Task C — usage store and poll policy
Files: `src/store/usage_store.rs`, `src/store/poll_policy.rs`.

- `UsageStore` over `cache/usage.json` (schema 2; rows per spec §8.4; identity guard;
  `entries(now)` → `UsageEntry` with `age_s`, `fresh(ttl)`, `in_backoff`, `claimed`,
  `recent_429`, `token_dead(stored_fp)`, `trust_extended`, `decision_value()`),
  `reserve(candidates, now, mode)` (claim rows: sets `lastAttemptAt`, 90 s window),
  `record(n, FetchRecord, plan)`, `set_poll_plan`, `clear_dead_token`, `failure_backoff_s`.
- `poll_policy`: constants and `plan_after_fetch(...)`, `due_candidate(...)`, `replan_new_active`.
- Sentinel derivation (`no credentials`, `token expired`, `api key`, `re-login needed`)
  as `UsageSentinel` + `usage_status()` string mapping.

## Wave 2 — behavior (parallel, after wave 1)

### Task D — switcher, collector, CLI, JSON, transfer
Files: `src/switcher.rs`, `src/collect.rs`, `src/jsonout.rs`, `src/transfer.rs`,
`src/cli/{mod,legacy,accounts,switch,list,config,transfer,misc}.rs`, `src/main.rs`,
`tests/cli_*.rs`.

- `Switcher` façade: `resolve(identifier)`, `current_account_number()` (live identity),
  `add_account(slot, alias, yes)`, `add_token(token, email, slot, yes)`, `remove`,
  `disable/enable`, `alias set/unset/list`, `move`, `swap`, `switch(strategy, models)`,
  `switch_to(id, force)`, `SwitchResult` (JSON shape), `list_snapshot(fetch)`,
  `status()`, `purge()`; outgoing fold-back (spec §7.2a), backups, daemon restart.
- `collect`: the on-demand pass (which rows to fetch, refresh for inactive accounts with
  CAS persistence, active account re-read from the live file, record into the store).
- `jsonout`: account rows, usage projection (`fiveHour`, `sevenDay`+pace, `scoped`,
  `credits`), list/status/switch/config payloads, error envelope.
- `cli`: clap grammar (verbs + legacy flags via pre-translation), cross-flag validation
  with the exact messages, `--json` single-document rule, exit codes, root guard,
  Ctrl-C notes, help text per spec §6; `auto`/`run`/`env`/`map`/`unmap`/`tui`/`watch`
  dispatch to functions in `cli::auto`, `cli::session`, `cli::tui` (owned by E/F; call
  `cswitch::autoswitch::cli_entry(argv)` etc. through the signatures fixed in those
  modules' skeletons).
- `transfer`: export/import per spec §11.

### Task E — auto-switch engine and session mode
Files: `src/autoswitch.rs`, `src/session.rs`, `src/cli/auto.rs`, `src/cli/session.rs`.

- `autoswitch`: `AutoFacade` trait (what the engine needs from the switcher: live slot,
  roster reads, usage collection, `switch_to`, freshen), `Engine::new(facade, settings,
  dry_run)`, `tick() -> TickOutcome`, `run_loop(stop)`, events (`Event::to_json`,
  `human()`), ranking per spec §9, state persistence, `cli_entry(args) -> i32`.
- `session`: profile bootstrap/reuse, sharing manifest, `run`, `env` lines, fold-back of
  rotated tokens after exit, `map`/`unmap` command bodies.

## Wave 3 — surface (parallel, after wave 2)

### Task F — TUI
Files: `src/tui/{mod,app,dashboard,switch,watch,auto,modals,widgets,theme,data}.rs`.
Per spec §12 and `docs/research/cswap-tui.md`.

### Task G — integration, wiring, docs
Files: `tests/integration_*.rs`, `README.md`, `docs/`, glue (`impl AutoFacade for
Switcher`, `cli` dispatch of auto/session/tui), release workflow (`.github/workflows/ci.yml`).

## Done criteria
`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
green on macOS; every command in spec §2 exercised by an integration test with a fake
`codex` and the mock usage server; README covers install, first account, switch, list,
auto, run, config, export/import, TUI.

## Status (2026-09-29, v0.1.0 released)

Landed, in commit order: skeleton and docs; wave 1 (Task A store + printer, Task B Codex
mechanics + mock server test, Task C usage store + poll policy); wave 2 (Task D switcher,
collector, CLI, JSON, transfer; Task T transfer + `config`; Task E auto-switch engine and
session mode); wave 3 Task G (end-to-end tests, JSON-mode silence, the rotating log file,
release workflow, CHANGELOG) and Task F (TUI: dashboard, switch, watch and auto screens,
modals, toasts, dark and light themes; the README screenshots are rendered from a
snapshot by `examples/tui_screenshot.rs`).

Released as `v0.1.0` on 2026-09-29 from commit `6cf4c44`: the `Release` workflow built
the four archives (macOS arm64 / x86_64, Linux x86_64, Windows x86_64) plus
`SHA256SUMS`, and the repository is public. `ci.yml` is green on ubuntu, macos and
windows for the tagged commit.

Tests: 327 (`cargo test --all`): 232 unit tests in the library plus integration binaries
`auto_once` (4), `cli_accounts` (10), `cli_list` (5), `cli_switch` (8), `codex_mock` (7),
`config_cmd` (12), `e2e_auto` (4), `e2e_usage` (7), `session_run` (11),
`transfer_roundtrip` (19), `tui_render` (8). The two `e2e_*` binaries drive the built
binary against an axum mock of the usage and token endpoints
(`tests/support/usage_mock.rs`), the fake `codex`, and temp `CSWITCH_HOME` /
`CODEX_HOME`; they cover real usage rows, the model pool row, credits, `(!)`,
`http-429`, the 180 s cache, `--strategy best` / `next-available` (skips and
`candidates-exhausted`), the 401 → refresh → 200 rotation into the slot file and the
live `auth.json`, `auto --once` exit codes 0/2/3, `--dry-run`, `--threshold`, and the
state file. `tui_render` renders every screen into a `TestBackend` buffer.

Behavior changes made by Task G: `--json` mode uses `SilentUi` (nothing on stderr for a
successful command, as in cswap §4.1); `src/logging.rs` installs the `cswitch.log`
file layer (INFO, 1 MiB × 3, lazy) for every entry point and the stderr layer with
`--debug`; the `env` verb is now dispatched (it was listed in `help` but never routed).

Known gaps: the v0.1 omissions of spec §2, §9 and §12 stand (`menubar`, `unclaimed`,
no-return anti-flap bar, recovery-horizon escape, identity-conflict quarantine,
`config-warning`, `soonest-reset`; the TUI theme `auto` follows `COLORFGBG` only).
Note for test authors: the API's `limit_reached` flag makes every window binding
(`usage.limited`), so a skip then reads `at 5h/7d limit`; the e2e mock leaves the flag
unset and lets the window percentages decide.
