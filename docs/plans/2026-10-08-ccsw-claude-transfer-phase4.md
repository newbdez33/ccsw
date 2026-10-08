# Provider export / import (phase 4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ccsw export` writes a version-2 `.ccsw` envelope that carries Codex and Claude accounts, and `ccsw import` reads v2 envelopes, v1 `.ccsw` envelopes (all Codex) and cswap `.cswap` exports (all Claude); release v0.7.0.

**Architecture:** Everything stays in `src/transfer.rs`. The envelope gains `provider` per account and `activeByProvider` beside `activeAccountNumber`; `credentials` is the provider's own slot-file object (§5). Import detects the envelope flavor from the root (`version` 2; `version` 1 with a ccsw marker; `version` 1 with `swapVersion` only), validates every entry for its provider in pass 1, then writes in pass 2 under the store lock, matching on `(provider, email, organizationUuid)`. Claude slot writes reuse the phase-3 ownership helpers (`claude::session::mutation_lock`, `mark_backup_replacement`, `is_quiescent`) exactly as `Switcher::refresh_in_place` does; export reconciles session-profile tokens through `claude::session::reconcile` before reading a Claude snapshot and prefers the live login for the active slot, as the Codex side already does.

**Tech Stack:** Rust 1.88 / edition 2024, serde_json, the existing `ccsw::claude` credential model (`ClaudeCredential`, `OauthAccount`, `SlotFile`), temp-store library tests and the `tests/support` binary harness (`CCSW_KEYCHAIN=off`).

**Spec:** `docs/specs/2026-10-07-ccsw-claude-provider-design.md` §5 (slot files), §6.2 (`export` / `import` row), §11 (Export / import), §15 (testing), §16 phase 4; messages and flow from `docs/specs/2026-09-29-ccsw-design.md` §11 and `docs/research/cswap-model-autoswitch.md` §8.

## Global Constraints

- Work in the current checkout and branch; do not create a worktree, do not rewrite history, never use bare `git stash` / `git stash pop`.
- Quality gate before every commit claim: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `env -u CODEX_HOME cargo test --all` (the host sets `CODEX_HOME`).
- Tests use temporary stores, the file credential backend (`Paths::from_values` disables the Keychain; the binary harness sets `CCSW_KEYCHAIN=off`) and no network. Never read or write real credentials.
- Identity is `(provider, email, organizationUuid)`; slots are one global space and gaps are never reused by import (`Roster::next_free_slot`).
- Claude slot files are `{"claudeAiOauth": …, "oauthAccount": …}` or `{"primaryApiKey": …, "oauthAccount": …}`; only the login part is stored (`SlotFile::new` strips siblings such as `mcpOAuth`).
- Unreadable stored credentials or session records mean uncertain ownership: propagate the error, never overwrite or refresh past it (phase-3 rule).
- Messages are cswap's texts with `ccsw` substituted; Python-style `repr` / type names stay in validation errors.
- No first-launch migration, no `.cswap` writing, no encryption.
- Commit messages and the PR carry no AI attribution; code, comments, commits and docs are concise English.

## Review Focus

1. A cswap `--full` export (credentials with `mcpOAuth` siblings, `config` holding the whole `~/.claude.json`) must import as a slim slot file: login part only, `config.oauthAccount` lifted verbatim, every other config key dropped. — Task 2 test `import_reads_a_cswap_export`.
2. A mixed-case email in a cswap export must become the lowercase identity Claude code uses everywhere (`OauthAccount::identity`), so a later `add claude` of the same login refreshes the slot instead of creating a duplicate. — Task 2 test `import_reads_a_cswap_export`, Task 3 test `imports_a_cswap_export_from_stdin`.
3. The same email and organization under both providers in one envelope are two accounts, not a duplicate. — Task 2 test `import_round_trips_a_mixed_export`.
4. Overwriting a Claude slot that has a session profile must write the stale marker (so a running profile is invalidated after it closes) and warn when the profile is live; a Claude slot whose stored snapshot is unparseable is refused, not overwritten. — Task 2 test `import_force_on_a_claude_slot_marks_its_profile_stale`.
5. Export must never fail or stall because of the live Claude login: an unreadable `.credentials.json` or a live login of another identity falls back to the stored snapshot; a Codex-only store never reads Claude files. — Task 1 test `export_claude_active_prefers_the_live_login_only_when_it_matches`.

---

### Task 1: Export v2 with both providers

**Files:**
- Modify: `src/transfer.rs` (`FORMAT_VERSION`, `Envelope`, `ExportedAccount`, `export_accounts`, `export_credentials`, unit tests at the bottom)
- Test: `tests/transfer_roundtrip.rs`

**Interfaces:**
- Consumes: `ccsw::claude::live::{ClaudeLive, LiveLogin}`, `ccsw::claude::keychain::SystemSecurity`, `ccsw::claude::credentials::SlotFile`, `crate::claude::session::reconcile(store, slot, record, cli)` (pub(crate)), `Roster::active_for(Provider)`.
- Produces: envelope `version: 2`; per-account `provider: "codex"|"claude"`; root `activeByProvider: {"codex": n, "claude": m}` (only slots present in the file; omitted when empty); `activeAccountNumber` still the Codex active slot when in the file. `ExportReport` unchanged. Task 2 reads exactly these keys.

- [ ] **Step 1: Update the existing export tests for version 2**

In `tests/transfer_roundtrip.rs`:

- `export_bulk_writes_the_envelope_file`: `text.starts_with("{\n  \"version\": 2,\n  \"exportedAt\": \"")`, `value["version"] == 2`, and after the `activeAccountNumber` assertion add `assert_eq!(value["activeByProvider"], json!({"codex": 3}));` and `assert_eq!(first["provider"], "codex");`.
- `import_validates_the_envelope_before_writing_anything`: the three version messages become `unsupported export version: 3 (expected 1 or 2)` (change the `"version": 2` case to `"version": 3`), `unsupported export version: None (expected 1 or 2)`, `unsupported export version: '1' (expected 1 or 2)`.

Add the mixed-roster fixtures and a new test after `export_to_stdout_prints_no_summary`:

```rust
fn claude_slot_file(email: &str, org: &str, org_name: &str, refresh: &str) -> Value {
    json!({
        "claudeAiOauth": {
            "accessToken": format!("cat-{refresh}"),
            "refreshToken": refresh,
            "expiresAt": 4_102_444_800_000i64,
            "scopes": ["user:inference", "user:profile"]
        },
        "oauthAccount": {
            "accountUuid": format!("uuid-{email}"),
            "emailAddress": email,
            "organizationUuid": org,
            "organizationName": org_name,
            "billingType": "stripe"
        }
    })
}

fn claude_record(email: &str, org: &str, org_name: &str) -> AccountRecord {
    let mut record = AccountRecord::new(email);
    record.provider = Provider::Claude;
    record.uuid = format!("uuid-{email}");
    record.organization_uuid = org.into();
    record.organization_name = org_name.into();
    record.added = "2026-09-01T00:00:00Z".into();
    record
}

/// Slots 1 (Codex, active), 2 (Claude OAuth, Acme, alias `cc`, active), 4 (Claude managed key).
fn seed_mixed(fx: &Fx) -> Roster {
    let mut ro = Roster::empty();
    add(
        fx,
        &mut ro,
        1,
        record("a@example.com", "acct-a"),
        Some(&chatgpt_auth("a@example.com", "acct-a", "rt-a")),
    );
    let mut cc = claude_record("c@example.com", "org-c", "Acme");
    cc.alias = Some("cc".into());
    add(
        fx,
        &mut ro,
        2,
        cc,
        Some(&claude_slot_file("c@example.com", "org-c", "Acme", "crt-c")),
    );
    let mut key = claude_record("api-key-4@token.local", "", "");
    key.kind = Some(AccountKind::ApiKey);
    add(
        fx,
        &mut ro,
        4,
        key,
        Some(&json!({
            "primaryApiKey": "sk-ant-api03-four",
            "oauthAccount": {"emailAddress": "api-key-4@token.local", "accountUuid": "",
                             "organizationUuid": null, "organizationName": null}
        })),
    );
    ro.set_active_for(Provider::Codex, Some(1));
    ro.set_active_for(Provider::Claude, Some(2));
    roster::write(&fx.paths, &ro).unwrap();
    ro
}

fn write_claude_live(fx: &Fx, creds: &Value, config: &Value) {
    fs::create_dir_all(&fx.paths.claude_home).unwrap();
    fs::write(fx.paths.claude_credentials_file(), creds.to_string()).unwrap();
    fs::write(fx.paths.claude_global_config_file(), config.to_string()).unwrap();
}

#[test]
fn export_carries_both_providers() {
    let fx = fixture();
    seed_mixed(&fx);
    let path = fx.root.join("mixed.ccsw");
    let report = export_accounts(&fx.paths, ExportTarget::File(path.clone()), None, false).unwrap();
    assert_eq!(report.written, 3);
    assert_eq!(
        report.notices,
        vec![format!("Exported 3 account(s) to {}", path.display())]
    );
    let value = report.envelope;
    assert_eq!(value["version"], 2);
    assert_eq!(value["activeAccountNumber"], 1);
    assert_eq!(value["activeByProvider"], json!({"claude": 2, "codex": 1}));
    let accounts = value["accounts"].as_array().unwrap();
    assert_eq!(accounts[0]["provider"], "codex");
    assert_eq!(accounts[1]["provider"], "claude");
    assert_eq!(accounts[1]["number"], 2);
    assert_eq!(accounts[1]["alias"], "cc");
    assert_eq!(accounts[1]["organizationName"], "Acme");
    assert!(accounts[1].get("planType").is_none());
    assert_eq!(
        accounts[1]["credentials"],
        claude_slot_file("c@example.com", "org-c", "Acme", "crt-c"),
        "the slot file is exported as stored"
    );
    assert_eq!(accounts[2]["provider"], "claude");
    assert_eq!(accounts[2]["kind"], "api_key");
    assert_eq!(accounts[2]["credentials"]["primaryApiKey"], "sk-ant-api03-four");

    // `--account` on a Claude slot by number or alias.
    for id in ["2", "cc"] {
        let report =
            export_accounts(&fx.paths, ExportTarget::File(path.clone()), Some(id), false).unwrap();
        assert_eq!(report.written, 1, "{id}");
        assert_eq!(report.envelope["accounts"][0]["email"], "c@example.com");
        assert_eq!(report.envelope["activeAccountNumber"], Value::Null);
        assert_eq!(report.envelope["activeByProvider"], json!({"claude": 2}));
    }
    // A single Codex account leaves `activeByProvider` out entirely when it is not active.
    let mut ro = roster_of(&fx);
    ro.set_active_for(Provider::Codex, Some(4));
    roster::write(&fx.paths, &ro).unwrap();
    let report =
        export_accounts(&fx.paths, ExportTarget::File(path.clone()), Some("1"), false).unwrap();
    assert!(report.envelope.get("activeByProvider").is_none());
}

#[test]
fn export_claude_active_prefers_the_live_login_only_when_it_matches() {
    let fx = fixture();
    seed_mixed(&fx);
    let export = |fx: &Fx| {
        export_accounts(&fx.paths, ExportTarget::File(fx.root.join("b.ccsw")), None, false)
            .unwrap()
            .envelope
    };
    let refresh = |value: &Value, index: usize| {
        value["accounts"][index]["credentials"]["claudeAiOauth"]["refreshToken"].clone()
    };
    let config = |email: &str, org: &str| {
        json!({"numStartups": 1, "oauthAccount": {"emailAddress": email, "accountUuid": "u",
               "organizationUuid": org, "organizationName": "Acme"}})
    };
    let live = |refresh: &str| {
        json!({"claudeAiOauth": {"accessToken": "live-at", "refreshToken": refresh,
               "expiresAt": 4_102_444_900_000i64, "scopes": ["user:inference"]},
               "mcpOAuth": {"srv": {"accessToken": "m"}}})
    };
    // No live login at all.
    assert_eq!(refresh(&export(&fx), 1), "crt-c");
    // The live login is the active slot's identity: its token wins, siblings and the
    // stored oauthAccount stay as stored.
    write_claude_live(&fx, &live("crt-live"), &config("C@example.com", "org-c"));
    let value = export(&fx);
    assert_eq!(refresh(&value, 1), "crt-live");
    assert!(value["accounts"][1]["credentials"].get("mcpOAuth").is_none());
    assert_eq!(value["accounts"][1]["credentials"]["oauthAccount"]["billingType"], "stripe");
    assert_eq!(
        slot_creds(&fx, 2)["claudeAiOauth"]["refreshToken"],
        "crt-c",
        "an export never modifies the stored snapshot"
    );
    // Another identity (same email, other org) is ignored.
    write_claude_live(&fx, &live("crt-z"), &config("c@example.com", "org-z"));
    assert_eq!(refresh(&export(&fx), 1), "crt-c");
    // Garbage live files are ignored.
    fs::write(fx.paths.claude_credentials_file(), "nope").unwrap();
    assert_eq!(refresh(&export(&fx), 1), "crt-c");
    // A live login matching an inactive Claude slot is not consulted.
    let mut ro = roster_of(&fx);
    ro.set_active_for(Provider::Claude, Some(4));
    roster::write(&fx.paths, &ro).unwrap();
    write_claude_live(&fx, &live("crt-live"), &config("c@example.com", "org-c"));
    assert_eq!(refresh(&export(&fx), 1), "crt-c");
}

#[test]
fn export_folds_a_newer_session_profile_token_into_a_claude_slot() {
    let fx = fixture();
    seed_mixed(&fx);
    let profile = fx.paths.session_dir(2, "c@example.com");
    fs::create_dir_all(&profile).unwrap();
    let mut rotated = claude_slot_file("c@example.com", "org-c", "Acme", "crt-rotated");
    rotated["claudeAiOauth"]["expiresAt"] = json!(4_102_444_900_000i64);
    let account = rotated.as_object_mut().unwrap().remove("oauthAccount").unwrap();
    fs::write(profile.join(".credentials.json"), rotated.to_string()).unwrap();
    fs::write(
        profile.join(".claude.json"),
        json!({"oauthAccount": account}).to_string(),
    )
    .unwrap();

    let report =
        export_accounts(&fx.paths, ExportTarget::File(fx.root.join("b.ccsw")), Some("2"), false)
            .unwrap();
    assert_eq!(
        report.envelope["accounts"][0]["credentials"]["claudeAiOauth"]["refreshToken"],
        "crt-rotated"
    );
    assert_eq!(
        slot_creds(&fx, 2)["claudeAiOauth"]["refreshToken"],
        "crt-rotated",
        "the profile's rotation is folded into the slot first"
    );
}
```

Add `use ccsw::provider::Provider;` to the test imports.

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `env -u CODEX_HOME cargo test --test transfer_roundtrip export_ 2>&1 | tail -30`
Expected: compile OK; `export_carries_both_providers` FAILS (`written` is 1, notice says "Skipped 2 Claude Code account(s)…"), `export_claude_active_prefers_the_live_login_only_when_it_matches` FAILS on the first `refresh(…, 1)` (index 1 is missing because Claude slots are skipped), `export_folds_a_newer_session_profile_token_into_a_claude_slot` FAILS (`--account 2` is a `TransferError`), `export_bulk_writes_the_envelope_file` FAILS on `version` 2.

- [ ] **Step 3: Implement the v2 envelope and the provider-aware credential selection**

In `src/transfer.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet};

use crate::claude::credentials::SlotFile;
use crate::claude::keychain::SystemSecurity;
use crate::claude::live::{ClaudeLive, LiveLogin};

pub const FORMAT_VERSION: u64 = 2;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    version: u64,
    exported_at: String,
    exported_from: &'static str,
    ccsw_version: &'static str,
    /// The same value under cswap's name, for readers that look for it.
    swap_version: &'static str,
    encrypted: bool,
    /// The Codex active slot (v1 readers); mirrors `activeByProvider.codex`.
    active_account_number: Option<u32>,
    /// Each provider's active slot when it is in the file.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    active_by_provider: BTreeMap<String, u32>,
    accounts: Vec<ExportedAccount>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportedAccount {
    number: u32,
    provider: Provider,
    email: String,
    // … unchanged fields …
}
```

`ExportedAccount::new` sets `provider: record.provider`. In `export_accounts`, delete the partition that skipped Claude slots and its notice, and build the live views and the active map:

```rust
    let live_codex = AuthJson::read(&paths.live_auth_file()).ok().flatten();
    // Only a Claude active slot that is being exported justifies reading Claude's login.
    let live_claude = roster
        .active_for(Provider::Claude)
        .filter(|active| slots.contains(active))
        .and_then(|_| ClaudeLive::new(paths, &SystemSecurity).read().ok());

    let mut notices = Vec::new();
    let mut skipped = Vec::new();
    let mut accounts = Vec::new();
    for slot in slots {
        let Some(record) = roster.record(slot) else {
            continue;
        };
        if record.provider == Provider::Claude
            && let Err(err) =
                crate::claude::session::reconcile(&store, slot, record, &SystemSecurity)
        {
            notice(
                &mut notices,
                format!(
                    "Warning: Account-{slot} ({}): {err}; exporting the stored snapshot",
                    record.email
                ),
            );
        }
        match export_credentials(
            &store,
            &roster,
            slot,
            record,
            live_codex.as_ref(),
            live_claude.as_ref(),
        )? {
            // … the three arms are unchanged …
        }
    }
    // … the empty checks are unchanged …
    let in_file = |slot: Option<u32>| slot.filter(|s| accounts.iter().any(|e| e.number == *s));
    let active_by_provider: BTreeMap<String, u32> = Provider::ALL
        .into_iter()
        .filter_map(|p| in_file(roster.active_for(p)).map(|s| (p.as_str().to_string(), s)))
        .collect();
    let envelope = Envelope {
        // …
        active_account_number: in_file(roster.active_for(Provider::Codex)),
        active_by_provider,
        accounts,
    };
```

Replace `export_credentials`:

```rust
/// The active slot of each provider exports the live login when it is the same
/// identity (the freshest tokens); everything else comes from the stored
/// snapshot. A Claude live login contributes its login part only; the slot's
/// stored `oauthAccount` stays.
fn export_credentials(
    store: &Store,
    roster: &Roster,
    slot: u32,
    record: &AccountRecord,
    live_codex: Option<&AuthJson>,
    live_claude: Option<&LiveLogin>,
) -> Result<Option<Value>> {
    let active = roster.active_for(record.provider) == Some(slot);
    let stored = credentials::read(store, slot)?;
    match record.provider {
        Provider::Codex => {
            if active
                && let Some(live) = live_codex
                && live
                    .identity()
                    .is_some_and(|identity| identity == record.identity())
            {
                return Ok(Some(live.0.clone()));
            }
            Ok(stored)
        }
        Provider::Claude => {
            let Some(stored) = stored else {
                return Ok(None);
            };
            if active
                && let Some(live) = live_claude
                && let Some(credential) = live.credential.as_ref()
                && live
                    .identity()
                    .is_some_and(|identity| identity == record.identity())
                && let Ok(file) = SlotFile::from_value(&stored)
            {
                return Ok(Some(SlotFile::new(credential, file.oauth_account).to_value()));
            }
            Ok(Some(stored))
        }
    }
}
```

Replace the unit test `export_skips_claude_slots` with `export_includes_claude_slots`: same `mixed_store`, assert `report.written == 2`, `report.notices.len() == 1` (the `Exported` line only), numbers `[1, 2]`, the file contains `claude@example.com`, and `export_accounts(…, Some("2"), false)` succeeds with `written == 1`. (`mixed_store`'s Claude snapshot has no `oauthAccount`; the export still writes it as stored.)

- [ ] **Step 4: Run the transfer tests and the full suite**

Run: `env -u CODEX_HOME cargo test --test transfer_roundtrip 2>&1 | tail -15 && env -u CODEX_HOME cargo test --lib transfer 2>&1 | tail -8`
Expected: all transfer_roundtrip tests PASS (import tests still pass: a v2 file is not yet importable, so `import_round_trips_an_export_into_an_empty_store` and `cmd_wrappers_map_results_to_exit_codes` FAIL with `unsupported export version: 2 (expected 1)` — that is expected until Task 2; every other test passes). Unit tests PASS.

Run: `env -u CODEX_HOME cargo test --all 2>&1 | grep -E '^test result|FAILED|failed' | head -20`
Expected: only the two round-trip tests above fail. Do not commit yet if anything else fails.

- [ ] **Step 5: Commit**

```bash
cargo fmt && git add src/transfer.rs tests/transfer_roundtrip.rs
git commit -m "feat(transfer): export both providers in a version 2 envelope"
```

(The two round-trip tests go green in Task 2; note them in the ledger as the expected intermediate state.)

---

### Task 2: Import v2, v1 and `.cswap` envelopes

**Files:**
- Modify: `src/transfer.rs` (`ParsedEnvelope`, `parse_envelope`, `validate_entries`, `import_accounts`, `live_login_slot`, unit tests)
- Test: `tests/transfer_roundtrip.rs`

**Interfaces:**
- Consumes: Task 1's envelope keys; `ccsw::claude::credentials::{ClaudeCredential, CredentialKind, OauthAccount, SlotFile, OAUTH_ACCOUNT_KEY, looks_like_api_key}`; `crate::claude::session::{mutation_lock, mark_backup_replacement, is_quiescent}` (pub(crate) / pub); `Roster::{find_slot, active_for, set_active_for}`.
- Produces: `import_accounts` accepts the three flavors; `ImportReport` unchanged. Task 3's CLI tests rely on the notice texts below.

- [ ] **Step 1: Write the failing import tests**

Append to `tests/transfer_roundtrip.rs`:

```rust
fn cswap_envelope(accounts: Vec<Value>, active: Option<u32>) -> Value {
    json!({
        "version": 1,
        "exportedAt": "2026-01-01T00:00:00Z",
        "exportedFrom": "macos",
        "swapVersion": "0.25.0",
        "encrypted": false,
        "activeAccountNumber": active,
        "accounts": accounts
    })
}

fn cswap_oauth_entry(number: u32, email: &str, org: &str, org_name: &str, refresh: &str) -> Value {
    json!({
        "number": number,
        "email": email,
        "uuid": format!("uuid-{email}"),
        "organizationUuid": org,
        "organizationName": org_name,
        "added": "2024-01-01T00:00:00Z",
        "credentials": {"claudeAiOauth": {"accessToken": format!("cat-{refresh}"), "refreshToken": refresh,
                        "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]}},
        "config": {"oauthAccount": {"emailAddress": email, "accountUuid": format!("uuid-{email}"),
                   "organizationUuid": org, "organizationName": org_name}}
    })
}

fn sessions_marker_count(fx: &Fx) -> usize {
    fs::read_dir(fx.paths.sessions_dir())
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .ends_with(".ccsw-stale-credentials")
                })
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn import_round_trips_a_mixed_export() {
    let source = fixture();
    seed_mixed(&source);
    let path = source.root.join("mixed.ccsw");
    export_accounts(&source.paths, ExportTarget::File(path.clone()), None, false).unwrap();

    let target = fixture();
    let report = import_accounts(&target.paths, ImportSource::File(path.clone()), false).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Imported a@example.com → slot 1",
            "Imported c@example.com → slot 2",
            "Imported api-key-4@token.local → slot 4",
            "Done: 3 imported, 0 overwritten, 0 skipped",
        ]
    );
    let imported = roster_of(&target);
    let original = roster_of(&source);
    assert_eq!(imported.sequence, vec![1, 2, 4]);
    for slot in [1, 2, 4] {
        assert_eq!(imported.record(slot), original.record(slot), "slot {slot}");
        assert_eq!(slot_creds(&target, slot), slot_creds(&source, slot));
    }
    assert_eq!(imported.active_for(Provider::Codex), Some(1));
    assert_eq!(imported.active_for(Provider::Claude), Some(2));
    assert_eq!(imported.active_account_number, Some(1));

    // The same email and organization under the other provider is another account.
    let mut twin = roster_of(&target);
    let mut codex_twin = record("c@example.com", "org-c");
    codex_twin.organization_name = "Acme".into();
    add(
        &target,
        &mut twin,
        5,
        codex_twin,
        Some(&chatgpt_auth("c@example.com", "org-c", "rt-twin")),
    );
    let report = import_accounts(&target.paths, ImportSource::File(path), false).unwrap();
    assert_eq!(report.skipped, 3);
    assert_eq!(roster_of(&target).sequence, vec![1, 2, 4, 5]);
    assert_eq!(slot_creds(&target, 5)["tokens"]["refresh_token"], "rt-twin");
}

#[test]
fn import_reads_a_cswap_export() {
    let fx = fixture();
    let before = now_unix();
    // Entry 1: a `--full` export — sibling keys and a full config.
    let mut full = cswap_oauth_entry(1, "Alice@Example.com", "org-a", "Acme", "crt-a");
    full["credentials"]["mcpOAuth"] = json!({"srv": {"accessToken": "mcp"}});
    full["config"]["numStartups"] = json!(9);
    full["config"]["projects"] = json!({"/tmp/p": {}});
    full["alias"] = json!("work");
    // Entry 2: a managed API key as cswap writes it — a bare string.
    let key = json!({
        "number": 2, "email": "api-key-2@token.local", "uuid": "", "organizationUuid": "",
        "organizationName": "", "credentials": "sk-ant-api03-two", "kind": "api_key",
        "config": {"oauthAccount": {"emailAddress": "api-key-2@token.local", "accountUuid": "",
                   "organizationUuid": null, "organizationName": null}}
    });
    // Entry 3: a setup-token with no config at all.
    let setup = json!({
        "number": 3, "email": "setup-token-3@token.local",
        "credentials": {"claudeAiOauth": {"accessToken": "sk-ant-oat01-x", "scopes": ["user:inference"]}}
    });
    let report = import_value(&fx, &cswap_envelope(vec![full, key, setup], Some(2)), false).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Imported alice@example.com → slot 1",
            "Imported api-key-2@token.local → slot 2",
            "Imported setup-token-3@token.local → slot 3",
            "Done: 3 imported, 0 overwritten, 0 skipped",
        ]
    );
    let after = roster_of(&fx);
    let alice = after.record(1).unwrap();
    assert_eq!(alice.provider, Provider::Claude);
    assert_eq!(alice.email, "alice@example.com", "the Claude identity is lowercase");
    assert_eq!(alice.organization_uuid, "org-a");
    assert_eq!(alice.organization_name, "Acme");
    assert_eq!(alice.alias.as_deref(), Some("work"));
    assert_eq!(alice.kind, None);
    assert_eq!(alice.added, "2024-01-01T00:00:00Z");
    assert_eq!(
        slot_creds(&fx, 1),
        json!({
            "claudeAiOauth": {"accessToken": "cat-crt-a", "refreshToken": "crt-a",
                              "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]},
            "oauthAccount": {"emailAddress": "Alice@Example.com", "accountUuid": "uuid-Alice@Example.com",
                             "organizationUuid": "org-a", "organizationName": "Acme"}
        }),
        "login part plus the lifted oauthAccount; siblings and other config keys dropped"
    );
    assert_eq!(
        after.find_slot(Provider::Claude, &Identity::new("alice@example.com", "org-a")),
        Some(1),
        "the slot is found the way `add claude` looks it up"
    );
    let key = after.record(2).unwrap();
    assert!(key.is_api_key());
    assert_eq!(key.provider, Provider::Claude);
    assert_eq!(slot_creds(&fx, 2)["primaryApiKey"], "sk-ant-api03-two");
    assert_eq!(slot_creds(&fx, 2)["oauthAccount"]["emailAddress"], "api-key-2@token.local");
    let setup = after.record(3).unwrap();
    assert_eq!(setup.kind, None);
    assert!(parse_iso(&setup.added).unwrap() >= before);
    assert_eq!(
        slot_creds(&fx, 3)["oauthAccount"],
        json!({"emailAddress": "setup-token-3@token.local", "accountUuid": "",
               "organizationUuid": null, "organizationName": null}),
        "no oauthAccount anywhere: synthesized from the entry"
    );
    assert_eq!(after.active_for(Provider::Claude), Some(2));
    assert_eq!(after.active_account_number, None, "Codex is untouched");

    // cswap writes 0 for "no active account".
    let fx = fixture();
    let only = cswap_oauth_entry(1, "b@example.com", "org-b", "", "crt-b");
    import_value(&fx, &cswap_envelope(vec![only], Some(0)), false).unwrap();
    assert_eq!(roster_of(&fx).active_for(Provider::Claude), None);
}

#[test]
fn import_keeps_v1_ccsw_envelopes_codex_and_defaults_v2_providers() {
    // A v1 ccsw file always carries `swapVersion` too; the ccsw marker wins.
    let fx = fixture();
    let v1 = envelope(
        vec![entry(1, "a@example.com", "acct-a", chatgpt_auth("a@example.com", "acct-a", "rt"))],
        Some(1),
    );
    assert_eq!(v1["cswitchVersion"], "0.1.0");
    import_value(&fx, &v1, false).unwrap();
    assert_eq!(roster_of(&fx).record(1).unwrap().provider, Provider::Codex);
    assert_eq!(roster_of(&fx).active_account_number, Some(1));

    // v2 without `provider` means Codex, as in the roster.
    let fx = fixture();
    let mut v2 = envelope(
        vec![entry(1, "a@example.com", "acct-a", chatgpt_auth("a@example.com", "acct-a", "rt"))],
        None,
    );
    v2["version"] = json!(2);
    import_value(&fx, &v2, false).unwrap();
    assert_eq!(roster_of(&fx).record(1).unwrap().provider, Provider::Codex);

    // v2 Claude entry whose slot file lacks `oauthAccount`: synthesized from the entry.
    let fx = fixture();
    let mut v2 = envelope(
        vec![json!({
            "number": 1, "provider": "claude", "email": "c@example.com", "uuid": "u-c",
            "organizationUuid": "org-c", "organizationName": "Acme",
            "credentials": {"claudeAiOauth": {"accessToken": "a", "refreshToken": "r"}}
        })],
        None,
    );
    v2["version"] = json!(2);
    import_value(&fx, &v2, false).unwrap();
    assert_eq!(
        slot_creds(&fx, 1)["oauthAccount"],
        json!({"emailAddress": "c@example.com", "accountUuid": "u-c",
               "organizationUuid": "org-c", "organizationName": "Acme"})
    );
}

#[test]
fn import_rejects_malformed_provider_entries() {
    let fx = fixture();
    let v2 = |patch: &dyn Fn(&mut Value)| {
        let mut e = json!({
            "number": 1, "provider": "claude", "email": "c@example.com",
            "credentials": {"claudeAiOauth": {"accessToken": "a", "refreshToken": "r"}}
        });
        patch(&mut e);
        let mut env = envelope(vec![e], None);
        env["version"] = json!(2);
        env
    };
    let cases: Vec<(Value, &str)> = vec![
        (
            v2(&|e| e["provider"] = json!("gemini")),
            "provider for c@example.com must be \"codex\" or \"claude\", got 'gemini'",
        ),
        (
            v2(&|e| e["provider"] = json!(1)),
            "provider for c@example.com must be \"codex\" or \"claude\", got 1",
        ),
        (
            v2(&|e| e["credentials"] = json!("sk-ant-oat01-not-a-key")),
            "API-key credentials for c@example.com must be a raw sk-ant-api… string",
        ),
        (
            v2(&|e| e["kind"] = json!("api_key")),
            "API-key credentials for c@example.com must be a raw sk-ant-api… string",
        ),
        (
            v2(&|e| e["credentials"] = json!({"mcpOAuth": {}})),
            "credentials for c@example.com hold no Claude login (expected claudeAiOauth or primaryApiKey)",
        ),
        (
            v2(&|e| e["credentials"] = json!(7)),
            "credentials for c@example.com must be a JSON object",
        ),
        (
            v2(&|e| e["config"] = json!([1])),
            "config for c@example.com must be a JSON object",
        ),
        (
            cswap_envelope(
                vec![
                    cswap_oauth_entry(1, "a@example.com", "org", "", "r1"),
                    cswap_oauth_entry(2, "A@example.com", "org", "", "r2"),
                ],
                None,
            ),
            "duplicate account in export: a@example.com (org=org)",
        ),
    ];
    for (value, message) in cases {
        let err = import_value(&fx, &value, false).unwrap_err();
        assert_eq!(err.type_name(), "TransferError", "{value}");
        assert_eq!(err.to_string(), message, "{value}");
    }
    assert!(!fx.paths.backup_root.exists(), "a rejected import writes nothing");
}

#[test]
fn import_force_on_a_claude_slot_marks_its_profile_stale() {
    let fx = fixture();
    seed_mixed(&fx);
    let profile = fx.paths.session_dir(2, "c@example.com");
    fs::create_dir_all(profile.join("sessions")).unwrap();
    let mut e = cswap_oauth_entry(2, "c@example.com", "org-c", "Acme", "crt-new");
    e["version"] = json!(2);
    let mut env = envelope(vec![e], None);
    env["version"] = json!(2);
    env["accounts"][0]["provider"] = json!("claude");

    // A quiescent profile: overwritten, marked stale, no warning.
    let report = import_value(&fx, &env, true).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Overwrote c@example.com (slot 2)",
            "Done: 0 imported, 1 overwritten, 0 skipped",
        ]
    );
    assert_eq!(slot_creds(&fx, 2)["claudeAiOauth"]["refreshToken"], "crt-new");
    assert_eq!(sessions_marker_count(&fx), 1, "the stale marker is written");

    // A live profile (this process's PID): still overwritten, with the warning.
    fs::write(
        profile.join("sessions").join("x.json"),
        json!({"pid": std::process::id()}).to_string(),
    )
    .unwrap();
    env["accounts"][0]["credentials"]["claudeAiOauth"]["refreshToken"] = json!("crt-newer");
    let report = import_value(&fx, &env, true).unwrap();
    assert_eq!(
        report.notices[0],
        "Warning: c@example.com (slot 2) has a live session-mode instance; its session profile keeps the pre-import credentials until it is restarted via 'ccsw run'."
    );
    assert_eq!(report.notices[1], "Overwrote c@example.com (slot 2)");
    assert_eq!(slot_creds(&fx, 2)["claudeAiOauth"]["refreshToken"], "crt-newer");

    // An unparseable stored snapshot is uncertain ownership: refused, untouched.
    credentials::write(&fx.store, 2, &json!({"claudeAiOauth": {"accessToken": "x"}})).unwrap();
    let err = import_value(&fx, &env, true).unwrap_err();
    assert_eq!(err.type_name(), "CredentialReadError");
    assert_eq!(slot_creds(&fx, 2), json!({"claudeAiOauth": {"accessToken": "x"}}));
}

#[test]
fn import_notes_a_written_claude_live_login() {
    let fx = fixture();
    let live = json!({"claudeAiOauth": {"accessToken": "la", "refreshToken": "crt-live",
                      "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]}});
    let config = json!({"oauthAccount": {"emailAddress": "C@example.com", "accountUuid": "u",
                        "organizationUuid": "org-c", "organizationName": "Acme"}});
    write_claude_live(&fx, &live, &config);
    let value = cswap_envelope(
        vec![cswap_oauth_entry(7, "c@example.com", "org-c", "Acme", "crt-c")],
        None,
    );
    let report = import_value(&fx, &value, false).unwrap();
    assert_eq!(
        report.notices.last().unwrap(),
        "Note: c@example.com is your current live login — activate the imported credentials with: ccsw switch 7 --force"
    );
    // Both providers' live logins can be noted in one run.
    write_live(&fx, &chatgpt_auth("a@example.com", "acct-a", "rt-live"));
    let mut both = envelope(
        vec![
            entry(1, "a@example.com", "acct-a", chatgpt_auth("a@example.com", "acct-a", "rt-a")),
            cswap_oauth_entry(7, "c@example.com", "org-c", "Acme", "crt-c2"),
        ],
        None,
    );
    both["version"] = json!(2);
    both["accounts"][1]["provider"] = json!("claude");
    let report = import_value(&fx, &both, true).unwrap();
    let notes: Vec<&String> = report.notices.iter().filter(|n| n.starts_with("Note: ")).collect();
    assert_eq!(notes.len(), 2, "{:?}", report.notices);
}
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `env -u CODEX_HOME cargo test --test transfer_roundtrip import_ 2>&1 | grep -E '^test |panicked|expected' | head -40`
Expected: `import_round_trips_a_mixed_export`, `import_round_trips_an_export_into_an_empty_store`, `import_reads_a_cswap_export`, `import_keeps_v1_ccsw_envelopes_codex_and_defaults_v2_providers`, `import_rejects_malformed_provider_entries`, `import_force_on_a_claude_slot_marks_its_profile_stale`, `import_notes_a_written_claude_live_login` FAIL (`unsupported export version: 2 (expected 1)`, cswap entries imported as Codex, missing messages). The pre-existing import tests still PASS.

- [ ] **Step 3: Implement flavor detection and provider-aware validation**

In `src/transfer.rs`:

```rust
use serde_json::{Map, Value, json};

use crate::claude::credentials::{
    ClaudeCredential, CredentialKind, OAUTH_ACCOUNT_KEY, OauthAccount, SlotFile, looks_like_api_key,
};

/// Which writer produced the file, decided from the root object alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    /// `version: 2` — `provider` per account (absent means Codex).
    V2,
    /// `version: 1` with `ccswVersion` / `cswitchVersion`: every account is Codex.
    CcswV1,
    /// `version: 1` from cswap (`swapVersion`, no ccsw marker): every account is Claude.
    Cswap,
}

struct ParsedEnvelope {
    flavor: Flavor,
    active_account_number: Option<u64>,
    active_by_provider: BTreeMap<Provider, u64>,
    accounts: Vec<Value>,
}

impl ParsedEnvelope {
    /// The envelope's active slot for `provider`; cswap's `0` means unset.
    fn active_for(&self, provider: Provider) -> Option<u64> {
        let number = match (self.flavor, provider) {
            (Flavor::V2, _) => self
                .active_by_provider
                .get(&provider)
                .copied()
                .or((provider == Provider::Codex).then_some(self.active_account_number).flatten()),
            (Flavor::CcswV1, Provider::Codex) | (Flavor::Cswap, Provider::Claude) => {
                self.active_account_number
            }
            _ => None,
        };
        number.filter(|n| *n > 0)
    }
}
```

`parse_envelope`:

```rust
    let version = root.get("version");
    let flavor = match version.and_then(Value::as_f64) {
        Some(v) if v == 2.0 => Flavor::V2,
        Some(v) if v == 1.0 => {
            if root.contains_key("ccswVersion") || root.contains_key("cswitchVersion") {
                Flavor::CcswV1
            } else if root.contains_key("swapVersion") {
                Flavor::Cswap
            } else {
                Flavor::CcswV1
            }
        }
        _ => {
            return Err(CcswError::transfer(format!(
                "unsupported export version: {} (expected 1 or 2)",
                python_repr(version)
            )));
        }
    };
    // … encrypted / accounts checks unchanged …
    let active_by_provider = root
        .get("activeByProvider")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    Some((Provider::parse_selector(key)?, value.as_u64()?))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ParsedEnvelope {
        flavor,
        active_account_number: root.get("activeAccountNumber").and_then(Value::as_u64),
        active_by_provider,
        accounts,
    })
```

`validate_entries(raw, flavor, local, notices)`: after `email` is validated, decide the provider and lowercase Claude emails:

```rust
        let provider = match flavor {
            Flavor::CcswV1 => Provider::Codex,
            Flavor::Cswap => Provider::Claude,
            Flavor::V2 => match entry.get("provider") {
                None | Some(Value::Null) => Provider::Codex,
                Some(value) => serde_json::from_value::<Provider>(value.clone()).map_err(|_| {
                    CcswError::transfer(format!(
                        "provider for {email} must be \"codex\" or \"claude\", got {}",
                        python_repr(Some(value))
                    ))
                })?,
            },
        };
        // Claude identities are lowercase everywhere (`OauthAccount::identity`).
        let email = match provider {
            Provider::Claude => email.to_lowercase(),
            Provider::Codex => email,
        };
```

Build the record before the credentials (the Claude path needs it for a synthesized `oauthAccount`), then:

```rust
        let flagged_api_key = entry.get("kind").and_then(Value::as_str) == Some("api_key");
        let (credentials, is_api_key) = match provider {
            Provider::Codex => match entry.get("credentials") {
                Some(value @ Value::Object(_)) => {
                    let auth = AuthJson::from_value(value.clone());
                    let api_key = auth.kind() == AuthKind::ApiKey;
                    (auth.0, api_key)
                }
                _ => {
                    return Err(CcswError::transfer(format!(
                        "credentials for {email} must be a JSON object"
                    )));
                }
            },
            Provider::Claude => claude_import_credentials(entry, &email, &record, flagged_api_key)?,
        };
        record.kind = (flagged_api_key || is_api_key).then_some(AccountKind::ApiKey);

        if !identities.insert((provider, email.clone(), organization_uuid.clone())) { /* unchanged message */ }
        // alias conflict: a local owner of another (provider, identity)
        let foreign_owner = alias_owner(local, &name)
            .and_then(|owner| local.record(owner))
            .is_some_and(|owner| (owner.provider, owner.identity()) != (provider, identity.clone()));
```

The Claude helper:

```rust
/// cswap's and ccsw's Claude shapes: a bare `sk-ant-api…` string, a slot file
/// (`oauthAccount` inside), or a cswap entry (`config.oauthAccount`). Only the
/// login part is kept; an `oauthAccount` found nowhere is synthesized from the
/// entry's own fields.
fn claude_import_credentials(
    entry: &Map<String, Value>,
    email: &str,
    record: &AccountRecord,
    flagged_api_key: bool,
) -> Result<(Value, bool)> {
    let config_account = match entry.get("config") {
        None | Some(Value::Null) => None,
        Some(Value::Object(config)) => config
            .get(OAUTH_ACCOUNT_KEY)
            .filter(|value| value.is_object())
            .cloned(),
        Some(_) => {
            return Err(CcswError::transfer(format!(
                "config for {email} must be a JSON object"
            )));
        }
    };
    let mut credential = match entry.get("credentials") {
        Some(Value::String(text)) if looks_like_api_key(text) => ClaudeCredential::managed_key(text),
        Some(Value::String(_)) => {
            return Err(CcswError::transfer(format!(
                "API-key credentials for {email} must be a raw sk-ant-api… string"
            )));
        }
        Some(value @ Value::Object(_)) => ClaudeCredential::from_value(value.clone()),
        _ => {
            return Err(CcswError::transfer(format!(
                "credentials for {email} must be a JSON object"
            )));
        }
    };
    let embedded_account = credential
        .0
        .as_object_mut()
        .and_then(|map| map.remove(OAUTH_ACCOUNT_KEY))
        .filter(Value::is_object);
    if credential.kind() == CredentialKind::Unknown {
        return Err(CcswError::transfer(format!(
            "credentials for {email} hold no Claude login (expected claudeAiOauth or primaryApiKey)"
        )));
    }
    let is_api_key = credential.kind() == CredentialKind::ApiKey;
    if flagged_api_key && !is_api_key {
        return Err(CcswError::transfer(format!(
            "API-key credentials for {email} must be a raw sk-ant-api… string"
        )));
    }
    let account = embedded_account
        .or(config_account)
        .map(OauthAccount)
        .unwrap_or_else(|| oauth_account_from_record(record));
    Ok((SlotFile::new(&credential, account).to_value(), is_api_key))
}

/// The `oauthAccount` an entry implies when it carries none (cswap's token shape,
/// with the entry's identity fields filled in).
fn oauth_account_from_record(record: &AccountRecord) -> OauthAccount {
    let text_or_null = |text: &str| {
        if text.is_empty() { Value::Null } else { Value::String(text.to_string()) }
    };
    OauthAccount(json!({
        "emailAddress": record.email,
        "accountUuid": record.uuid,
        "organizationUuid": text_or_null(&record.organization_uuid),
        "organizationName": text_or_null(&record.organization_name),
    }))
}
```

`ImportEntry` gains `provider: Provider` (also set on `record.provider`).

- [ ] **Step 4: Implement the provider-aware write pass**

In `import_accounts`:

```rust
    let mut resolved_active: BTreeMap<Provider, u32> = BTreeMap::new();

    for entry in entries {
        let provider = entry.provider;
        let identity = entry.record.identity();
        let email = identity.email.clone();
        let envelope_active = envelope.active_for(provider) == Some(u64::from(entry.number));
        let (slot, outcome) = match roster.find_slot(provider, &identity) {
            // … the four arms are unchanged, except `resolved_active.insert(provider, slot)` …
        };
        let strike = (outcome == Outcome::Overwrote)
            .then(|| strike_state(&usage, slot, &identity, &entry.credentials, now));
        // Phase-3 ownership: a Claude slot being replaced coordinates with refreshes
        // and invalidates its session profile once that profile is idle.
        let _consume = match (provider, roster.record(slot)) {
            (Provider::Claude, Some(old)) => {
                let lock = crate::claude::session::mutation_lock(&store, slot)?;
                let profile = store.paths.session_dir(slot, &old.email);
                if profile.exists() && !crate::claude::session::is_quiescent(&profile) {
                    notice(
                        &mut report.notices,
                        format!(
                            "Warning: {email} (slot {slot}) has a live session-mode instance; its session profile keeps the pre-import credentials until it is restarted via 'ccsw run'."
                        ),
                    );
                }
                crate::claude::session::mark_backup_replacement(&store, slot, old, &entry.credentials)?;
                lock
            }
            _ => None,
        };

        credentials::write(&store, slot, &entry.credentials)?;
        // … unchanged …
        if envelope_active {
            resolved_active.insert(provider, slot);
        }
    }

    let mut seeded = false;
    for (provider, slot) in &resolved_active {
        if roster.active_for(*provider).is_none_or(|active| active == 0) {
            roster.set_active_for(*provider, Some(*slot));
            seeded = true;
        }
    }
    if seeded {
        roster::write(paths, &roster)?;
    }
    // … summary unchanged …
    for (provider, slot) in live_login_slots(paths, &store, &roster) {
        if report.written_slots.contains(&slot)
            && let Some(record) = roster.record(slot)
        {
            debug_assert_eq!(record.provider, provider);
            notice(/* unchanged Note text */);
        }
    }
```

`live_login_slots` replaces `live_login_slot`:

```rust
/// The managed slot holding each provider's live login: by identity, or by key
/// for an API key. Claude's files are read only when a Claude slot exists.
fn live_login_slots(paths: &Paths, store: &Store, roster: &Roster) -> Vec<(Provider, u32)> {
    let mut slots = Vec::new();
    if let Some(live) = AuthJson::read(&paths.live_auth_file()).ok().flatten() {
        let slot = match live.identity() {
            Some(identity) => roster.find_slot(Provider::Codex, &identity),
            None => live.api_key().and_then(|key| {
                roster.slots_of(Provider::Codex).into_iter().find(|slot| {
                    credentials::read(store, *slot)
                        .ok()
                        .flatten()
                        .is_some_and(|value| AuthJson::from_value(value).api_key() == Some(key))
                })
            }),
        };
        slots.extend(slot.map(|slot| (Provider::Codex, slot)));
    }
    if !roster.slots_of(Provider::Claude).is_empty()
        && let Ok(live) = ClaudeLive::new(paths, &SystemSecurity).read()
        && let Some(credential) = live.credential.as_ref()
    {
        let slot = match credential.kind() {
            CredentialKind::ApiKey => credential.api_key().and_then(|key| {
                roster.slots_of(Provider::Claude).into_iter().find(|slot| {
                    credentials::read(store, *slot)
                        .ok()
                        .flatten()
                        .and_then(|value| SlotFile::from_value(&value).ok())
                        .is_some_and(|file| file.credential.api_key() == Some(key))
                })
            }),
            CredentialKind::OAuth | CredentialKind::SetupToken => live
                .identity()
                .and_then(|identity| roster.find_slot(Provider::Claude, &identity)),
            CredentialKind::Unknown => None,
        };
        slots.extend(slot.map(|slot| (Provider::Claude, slot)));
    }
    slots
}
```

Update the module doc comment (first lines of `src/transfer.rs`) to name the three flavors.

- [ ] **Step 5: Run the transfer tests, then the full suite**

Run: `env -u CODEX_HOME cargo test --test transfer_roundtrip 2>&1 | tail -5 && env -u CODEX_HOME cargo test --lib transfer 2>&1 | tail -5`
Expected: all PASS, including the two round-trip tests left red in Task 1.

Run: `env -u CODEX_HOME cargo test --all 2>&1 | grep -E '^test result|FAILED|failed' | head -20`
Expected: every `test result: ok`.

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add src/transfer.rs tests/transfer_roundtrip.rs
git commit -m "feat(transfer): import v2, v1 and cswap envelopes by provider"
```

---

### Task 3: CLI coverage, documentation and the v0.7.0 bump

**Files:**
- Create: `tests/cli_transfer.rs`
- Modify: `src/cli/legacy.rs` (`--full` help line), `README.md` (intro lines 7–8, "Other commands", a short "Export / import" paragraph), `CHANGELOG.md` (new `v0.7.0` section), `handover.md` (phase-4 status), `docs/specs/2026-10-07-ccsw-claude-provider-design.md` (append "Phase 4 implementation notes"), `Cargo.toml` + `Cargo.lock` (`0.7.0`).

**Interfaces:**
- Consumes: Task 2's notice texts and the `tests/support` harness (`Cli::new`, `add_chatgpt`, `add_claude`, `run`, `run_with_stdin`, `roster`, `credential`, `claude_credentials`, `claude_config`, `write_claude_live`).
- Produces: the release-ready branch.

- [ ] **Step 1: Write the failing CLI tests**

Create `tests/cli_transfer.rs`:

```rust
//! Export / import through the built binary: a mixed roster round trip and a
//! cswap export piped through stdin.

mod support;

use serde_json::json;
use support::{Cli, read_json};

#[test]
fn export_and_import_a_mixed_roster_through_the_binary() {
    let source = Cli::new();
    source.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    source.add_claude("one@example.com", "org-1", "Acme", "crt-1");
    let path = source.root.path().join("backup.ccsw");
    let path_str = path.to_str().unwrap();

    let run = source.run(&["export", path_str]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert_eq!(
        run.stderr,
        format!("Exported 2 account(s) to {}\n", path.display())
    );
    let value = read_json(&path);
    assert_eq!(value["version"], 2);
    assert_eq!(value["activeByProvider"], json!({"claude": 2, "codex": 1}));
    assert_eq!(value["accounts"][0]["provider"], "codex");
    assert_eq!(value["accounts"][1]["provider"], "claude");
    assert_eq!(
        value["accounts"][1]["credentials"]["claudeAiOauth"]["refreshToken"],
        "crt-1"
    );
    assert!(
        value["accounts"][1]["credentials"].get("mcpOAuth").is_none(),
        "machine-scoped siblings never travel"
    );
    assert_eq!(
        value["accounts"][1]["credentials"]["oauthAccount"]["emailAddress"],
        "one@example.com"
    );

    let target = Cli::new();
    let run = target.run(&["import", path_str]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "Imported alice@example.com → slot 1\n\
         Imported one@example.com → slot 2\n\
         Done: 2 imported, 0 overwritten, 0 skipped\n"
    );
    let roster = target.roster();
    assert_eq!(roster["accounts"]["2"]["provider"], "claude");
    assert_eq!(roster["activeByProvider"], json!({"claude": 2, "codex": 1}));
    assert_eq!(target.credential(2), source.credential(2));

    // The imported Claude login is switchable on the target.
    let run = target.run(&["switch", "2"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        target.claude_credentials()["claudeAiOauth"]["refreshToken"],
        "crt-1"
    );
    assert_eq!(
        target.claude_config()["oauthAccount"]["organizationName"],
        "Acme"
    );

    // Importing again skips both; `--force` overwrites both.
    let run = target.run(&["import", path_str]);
    assert_eq!(run.status, 0);
    assert!(
        run.stderr.ends_with("Done: 0 imported, 0 overwritten, 2 skipped\n"),
        "{}",
        run.stderr
    );
    let run = target.run(&["--import", path_str, "--force"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stderr.contains("Done: 0 imported, 2 overwritten, 0 skipped\n"),
        "{}",
        run.stderr
    );
}

#[test]
fn imports_a_cswap_export_from_stdin() {
    let cli = Cli::new();
    let cswap = json!({
        "version": 1,
        "exportedAt": "2026-01-01T00:00:00Z",
        "exportedFrom": "macos",
        "swapVersion": "0.25.0",
        "encrypted": false,
        "activeAccountNumber": 1,
        "accounts": [
            {
                "number": 1, "email": "Alice@Example.com", "uuid": "acct-uuid",
                "organizationUuid": "org-a", "organizationName": "Acme",
                "added": "2024-01-01T00:00:00Z",
                "credentials": {"claudeAiOauth": {"accessToken": "at", "refreshToken": "crt-a",
                                "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]}},
                "config": {"oauthAccount": {"emailAddress": "Alice@Example.com", "accountUuid": "acct-uuid",
                           "organizationUuid": "org-a", "organizationName": "Acme"}}
            },
            {
                "number": 2, "email": "api-key-2@token.local", "uuid": "", "organizationUuid": "",
                "organizationName": "", "added": "2024-01-01T00:00:00Z",
                "credentials": "sk-ant-api03-key", "kind": "api_key",
                "config": {"oauthAccount": {"emailAddress": "api-key-2@token.local", "accountUuid": "",
                           "organizationUuid": null, "organizationName": null}}
            }
        ]
    });
    let run = cli.run_with_stdin(&["import", "-"], &cswap.to_string());
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "Imported alice@example.com → slot 1\n\
         Imported api-key-2@token.local → slot 2\n\
         Done: 2 imported, 0 overwritten, 0 skipped\n"
    );
    let roster = cli.roster();
    assert_eq!(roster["accounts"]["1"]["provider"], "claude");
    assert_eq!(roster["accounts"]["1"]["email"], "alice@example.com");
    assert_eq!(roster["accounts"]["2"]["kind"], "api_key");
    assert_eq!(roster["activeByProvider"], json!({"claude": 1}));
    assert_eq!(cli.credential(2)["primaryApiKey"], "sk-ant-api03-key");

    // The same login captured live refreshes slot 1 instead of adding a twin.
    cli.write_claude_live("Alice@Example.com", "org-a", "Acme", "crt-a2");
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .starts_with("Updated credentials for Account 1 (alice@example.com [Acme])"),
        "{}",
        run.stdout
    );
    assert_eq!(cli.roster()["sequence"], json!([1, 2]));
    assert_eq!(cli.credential(1)["claudeAiOauth"]["refreshToken"], "crt-a2");

    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0);
    assert!(run.stdout.contains("alice@example.com"), "{}", run.stdout);
}
```

- [ ] **Step 2: Run the CLI tests to verify the state**

Run: `env -u CODEX_HOME cargo test --test cli_transfer 2>&1 | tail -20`
Expected: both PASS already if Tasks 1–2 are complete (this task adds end-to-end coverage, not behavior). If either fails, the failure is a Task 1/2 defect: fix it there with a failing library test first and ledger the ruling.

- [ ] **Step 3: Update the help text and the docs**

`src/cli/legacy.rs`, the `--full` option: `--full                Accepted for compatibility (use with 'export'); ccsw exports\n                        always carry each account's stored identity`. (Keep the two-line layout of its neighbours; run `env -u CODEX_HOME cargo test --lib cli::legacy` afterwards.)

`README.md`:
- Lines 7–8: replace `Session mode supports both providers. Export/import still covers Codex accounts; mixed-provider transfers are a later phase.` with `Session mode, export and import cover both providers.`
- Under "Other commands", after the `export` / `import` line, add a paragraph:

```markdown
`ccsw export` writes a version-2 `.ccsw` file with every Codex and Claude account
(`--account` limits it to one). `ccsw import` reads those files, version-1 `.ccsw` files
from earlier releases (all Codex) and `cswap` exports (`.cswap`, all Claude Code), matching
accounts on provider, email and organization; `--force` overwrites matches in place and a
quarantined dead-token slot is replaced without it. Exports hold credentials in plain JSON:
keep them private.
```

`CHANGELOG.md`, above `## v0.6.0`:

```markdown
## v0.7.0 — 2026-10-08

### Added

- Export and import both providers. The `.ccsw` envelope is version 2: every
  account carries `provider`, `activeByProvider` records each provider's active
  slot and Claude credentials are the stored slot file. The active Claude login
  is exported from the live backend when it matches, and session-profile
  rotations are folded into the slot first.
- Import `cswap` exports (`swapVersion` without a ccsw marker): accounts become
  Claude Code accounts, `config.oauthAccount` is kept, a bare `sk-ant-api…`
  credential becomes a managed key and emails take the lowercase Claude identity.
- Version-1 `.ccsw` files still import as Codex accounts.

### Changed

- Overwriting a Claude slot on import coordinates with in-flight refreshes,
  marks its session profile stale and warns when that profile is live.
- Exports from this release are version 2; earlier releases cannot import them.
```

`docs/specs/2026-10-07-ccsw-claude-provider-design.md`, append:

```markdown

## Phase 4 implementation notes (2026-10-08)

- The envelope adds `activeByProvider` (the roster's name, §5) beside
  `activeAccountNumber`; import seeds each provider's active slot only when it is unset
  locally. cswap's `activeAccountNumber` seeds the Claude side, `0` meaning unset.
- Flavor is decided from the root: `version` 2; `version` 1 with `ccswVersion` or
  `cswitchVersion` is a ccsw v1 file (Codex); `version` 1 with `swapVersion` alone is a
  cswap file (Claude). A v2 account without `provider` is Codex, as in the roster.
- Claude entries keep only the login part of `credentials`; `oauthAccount` comes from the
  slot file, else `config.oauthAccount`, else the entry's own fields. Credentials without
  a login (`claudeAiOauth` / `primaryApiKey`) are refused. Claude emails are lowercased,
  the identity rule of `OauthAccount::identity`.
- Export reconciles session-profile tokens (`claude::session::reconcile`) before reading a
  Claude snapshot and reports a failed reconcile as a warning while exporting the snapshot.
- Import overwrite of a Claude slot takes the fingerprint consume lock, writes the stale
  profile marker and warns about a live profile instead of refusing; an unparseable stored
  snapshot is refused (uncertain ownership).
```

`handover.md`: in `## 当前状态` add `- Claude export/import 第四阶段已实现，版本设为 \`v0.7.0\`；工作区为 \`hraesvelg\`，分支为 \`newbdez33/hraesvelg\`。`; replace the `## 后续阶段` block's first and third bullets with a short `## 已完成：export/import v2 与 .cswap 导入` section (plan path, flavor rule, lowercase Claude identity, ownership protocol on overwrite, `activeByProvider`, test summary) and leave the "首次快照加载前进入自动视图会显示 OFF" note under `## 后续阶段` (now the only item: "无已规划的后续阶段").

- [ ] **Step 4: Bump the version**

`Cargo.toml`: `version = "0.7.0"`. Run `cargo build 2>&1 | tail -2` so `Cargo.lock` follows, then `git diff --stat Cargo.lock` to confirm only the `ccsw` entry changed.

- [ ] **Step 5: Run the quality gate**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings 2>&1 | tail -3 && env -u CODEX_HOME cargo test --all 2>&1 | grep -E '^test result|FAILED|failed'`
Expected: fmt silent, clippy `Finished`, every `test result: ok`.

- [ ] **Step 6: Commit**

```bash
git add tests/cli_transfer.rs src/cli/legacy.rs README.md CHANGELOG.md handover.md docs/specs/2026-10-07-ccsw-claude-provider-design.md Cargo.toml Cargo.lock
git commit -m "feat(transfer): cover the CLI, document provider transfers and release v0.7.0"
```

---

## Implementation validation

(Filled in by the executor: final review outcome, rulings, gate results, PR and release references.)
