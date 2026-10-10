# ccsw `import --from-cswap` + musl release target — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ccsw import --from-cswap [DIR] [--retire] [--json]` reads a claude-swap store (roster, Keychain or base64 credential backups, config backups) straight into the ccsw roster, and the release matrix gains a static `x86_64-unknown-linux-musl` build; shipped as **v0.8.0**.

**Architecture:** A new `cswap_store` module turns a claude-swap store directory into the in-memory shape of a cswap export envelope (`version: 1`, `swapVersion`, `accounts[]`), so the existing `transfer::import_accounts` path (identity dedupe, slot allocation, Codex untouched) does the writing. `ImportSource` gains a `CswapStore` variant; `import_cmd` adds retire-on-success, a distinct exit code for "nothing to import", and a `--json` report for the desktop app. Keychain reads go through the existing `SecurityCli` trait so unit tests use the `Fake` recorder.

**Tech Stack:** Rust 2024 edition (toolchain stable), serde_json, base64 crate (check `Cargo.toml` — add `base64 = "0.22"` if absent), tempfile (dev-dep, already used), GitHub Actions release workflow.

**Spec:** `token-beats` repo, `docs/desktop/specs/ccsw-engine.spec.md` § "ccsw side". The desktop plan (`token-beats/docs/plans/2026-10-10-desktop-ccsw-engine.md`) consumes the CLI contract defined here.

## Global Constraints

- claude-swap store layout (from claude-swap 0.25.0 source, `switcher.py:324-325, 888`, `credentials.py:49, 937, 996`): roster `sequence.json` (`accounts` object keyed by slot number string → `{email, organizationUuid, organizationName, uuid, added}`, plus `activeAccountNumber`); credentials `credentials/.creds-{number}-{email}.enc` = base64 of the credentials JSON (not encrypted); on macOS the primary copy is a Keychain item, service `claude-swap`, account `account-{number}-{email}`; config backup `configs/.claude-config-{number}-{email}.json` (the `.claude.json` snapshot, carries `oauthAccount`).
- Default store dir: macOS and Windows `~/.claude-swap-backup`; Linux and WSL `$XDG_DATA_HOME/claude-swap`, default `~/.local/share/claude-swap`.
- Exit codes of `import --from-cswap`: `0` imported ≥ 1 account; `2` nothing to import (no store, empty roster, or already retired); `1` any error. `--retire` renames the store to `<dir>.migrated-<YYYYMMDD-HHMMSS>` only on exit 0.
- `--json` on `import` prints exactly one JSON document to stdout: `{"schemaVersion": 2, "imported": n, "overwritten": n, "skipped": n, "replaced": n, "retired": "<path>" | null}`; errors use the existing `--json` error envelope (`{"schemaVersion": 2, "error": {"type", "message"}}`, exit 1); the "nothing to import" case prints `{"schemaVersion": 2, "imported": 0, "overwritten": 0, "skipped": 0, "replaced": 0, "retired": null, "reason": "no-store" | "empty" | "retired"}` with exit 2.
- Nothing in this change touches Codex accounts or the Codex live login.
- Version becomes `0.8.0` in `Cargo.toml` + `Cargo.lock`; CHANGELOG gets a `## v0.8.0 — <date>` entry; `handover.md` gets a bullet; README documents the flag and the musl asset. `FALLBACK_CLI_VERSION` (`src/claude/usage.rs:26`) is checked against the local Claude Code version at release time (release checklist, `handover.md:79`).
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` must pass (CI gate, `.github/workflows/ci.yml`).

## Review Focus

1. **A store whose `.enc` file is not valid base64 or not JSON** — the account must be skipped with a notice naming the slot, the rest imported, exit 0. (Task 1 test `a_corrupt_enc_file_skips_that_account_only`.)
2. **macOS with the Keychain locked** (`security` exits 36) — fall back to the `.enc` file for that slot; no item and no file → skipped with a notice, never an abort. (Task 1 test `keychain_failure_falls_back_to_the_enc_file`.)
3. **Running `import --from-cswap --retire` twice** — the second run finds the renamed directory, prints nothing-to-import, exits 2, and does not rename anything again. (Task 2 test `a_second_run_after_retire_exits_2`.)
4. **An account already present in ccsw** (same provider+email+org) — without `--force` the existing slot wins and the importer reports it as `skipped`. The store is retired only when `imported ≥ 1`: everything skipped → exit 0, `imported: 0`, `skipped: n`, nothing renamed. (Task 2 test `all_accounts_already_present_imports_nothing_and_keeps_the_store`.)
5. **An email containing characters unsafe for a file name** — claude-swap used the raw email in file and Keychain names; the reader must use the exact `email` string from `sequence.json`, not a normalized one, when building the `.enc` path and the Keychain account. (Task 1 test `file_and_keychain_names_use_the_raw_email`.)

---

### Task 1: `cswap_store` — read a claude-swap store into a cswap export envelope

**Files:**
- Create: `src/cswap_store.rs`
- Modify: `src/lib.rs:27` (add `pub mod cswap_store;` after `pub mod transfer;`)
- Modify: `Cargo.toml` (add `base64 = "0.22"` under `[dependencies]` if it is not already there — `rg -n '^base64' Cargo.toml`)
- Test: unit tests inside `src/cswap_store.rs`

**Interfaces:**
- Consumes: `crate::claude::keychain::{Keychain, SecurityCli}` (`Keychain::new(&dyn SecurityCli)`, `get_password(service, account) -> Result<Option<String>, KeychainError>` — `src/claude/keychain.rs:38-51`); `crate::paths::user_home()` (`pub(crate) fn user_home() -> Option<PathBuf>`, `src/paths.rs:7`); `crate::errors::{CcswError, Result}` (`CcswError::transfer(msg)`); `crate::transfer::platform_name()` (make it `pub` if it is `pub(crate)` — it is `pub fn platform_name()` at `transfer.rs:120`).
- Produces:
  ```rust
  pub const KEYCHAIN_SERVICE: &str = "claude-swap";
  pub fn default_dir() -> Option<PathBuf>;
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Probe { Missing, Retired, Empty, Ready { accounts: usize } }
  pub fn probe(dir: &Path) -> Result<Probe>;
  pub struct StoreEnvelope { pub envelope: serde_json::Value, pub notices: Vec<String>, pub readable: usize }
  pub fn envelope(dir: &Path, security: &dyn SecurityCli, use_keychain: bool) -> Result<StoreEnvelope>;
  pub fn retire(dir: &Path) -> Result<PathBuf>;
  ```

- [ ] **Step 1: Write the failing tests**

Append to the new file `src/cswap_store.rs` (the module body comes in Step 3; write the test module first so the file compiles with stubs only after Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::keychain::CliOutput;
    use base64::Engine as _;
    use serde_json::json;
    use std::cell::RefCell;
    use std::fs;

    /// Records `security` calls; `reply` scripts the next stdout, `fail`
    /// makes the next call exit 36 (Keychain locked).
    struct Fake {
        replies: RefCell<Vec<CliOutput>>,
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl Fake {
        fn new() -> Self {
            Self { replies: RefCell::new(Vec::new()), calls: RefCell::new(Vec::new()) }
        }
        fn reply(&self, stdout: &str) {
            self.replies.borrow_mut().push(CliOutput { status: 0, stdout: stdout.to_string() });
        }
        fn fail(&self) {
            self.replies.borrow_mut().push(CliOutput { status: 36, stdout: String::new() });
        }
    }
    impl SecurityCli for Fake {
        fn run(&self, args: &[String], _stdin: Option<&str>) -> std::io::Result<CliOutput> {
            self.calls.borrow_mut().push(args.to_vec());
            let mut replies = self.replies.borrow_mut();
            Ok(if replies.is_empty() {
                CliOutput { status: 44, stdout: String::new() } // item not found
            } else {
                replies.remove(0)
            })
        }
    }

    fn creds(refresh: &str) -> serde_json::Value {
        json!({"claudeAiOauth": {"accessToken": "at", "refreshToken": refresh,
               "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]}})
    }

    /// A store with two accounts: slot 1 (alice) and slot 2 (bob), both
    /// as base64 `.enc` files plus a config backup each.
    fn store(dir: &Path) {
        fs::create_dir_all(dir.join("credentials")).unwrap();
        fs::create_dir_all(dir.join("configs")).unwrap();
        fs::write(
            dir.join("sequence.json"),
            json!({
                "accounts": {
                    "1": {"email": "alice@example.com", "organizationUuid": "org-a",
                          "organizationName": "Acme", "uuid": "u-a", "added": "2026-01-01T00:00:00Z"},
                    "2": {"email": "Bob+x@Example.com", "organizationUuid": "",
                          "organizationName": "", "uuid": "u-b", "added": "2026-01-02T00:00:00Z"}
                },
                "activeAccountNumber": 1
            })
            .to_string(),
        )
        .unwrap();
        let b64 = |v: &serde_json::Value| base64::engine::general_purpose::STANDARD.encode(v.to_string());
        fs::write(dir.join("credentials/.creds-1-alice@example.com.enc"), b64(&creds("crt-a"))).unwrap();
        fs::write(dir.join("credentials/.creds-2-Bob+x@Example.com.enc"), b64(&creds("crt-b"))).unwrap();
        fs::write(
            dir.join("configs/.claude-config-1-alice@example.com.json"),
            json!({"oauthAccount": {"emailAddress": "alice@example.com", "organizationUuid": "org-a",
                   "organizationName": "Acme", "accountUuid": "u-a"}, "numStartups": 3}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn probe_distinguishes_missing_retired_empty_and_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude-swap-backup");
        assert_eq!(probe(&dir).unwrap(), Probe::Missing);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("sequence.json"), r#"{"accounts": {}, "activeAccountNumber": null}"#).unwrap();
        assert_eq!(probe(&dir).unwrap(), Probe::Empty);
        store(&dir);
        assert_eq!(probe(&dir).unwrap(), Probe::Ready { accounts: 2 });
        let retired = retire(&dir).unwrap();
        assert!(retired.file_name().unwrap().to_string_lossy().starts_with("claude-swap-backup.migrated-"));
        assert_eq!(probe(&dir).unwrap(), Probe::Retired);
    }

    #[test]
    fn envelope_from_enc_files_matches_the_cswap_export_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        let fake = Fake::new();
        let out = envelope(&dir, &fake, false).unwrap();
        assert!(fake.calls.borrow().is_empty(), "file backend never calls security");
        assert_eq!(out.readable, 2);
        assert!(out.notices.is_empty(), "{:?}", out.notices);
        let env = &out.envelope;
        assert_eq!(env["version"], 1);
        assert!(env["swapVersion"].is_string());
        assert_eq!(env["encrypted"], false);
        assert_eq!(env["activeAccountNumber"], 1);
        let accounts = env["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0]["number"], 1);
        assert_eq!(accounts[0]["email"], "alice@example.com");
        assert_eq!(accounts[0]["organizationUuid"], "org-a");
        assert_eq!(accounts[0]["credentials"]["claudeAiOauth"]["refreshToken"], "crt-a");
        assert_eq!(accounts[0]["config"]["oauthAccount"]["organizationUuid"], "org-a");
        assert!(accounts[1].get("config").is_none(), "no config backup → no config key");
        assert_eq!(accounts[1]["credentials"]["claudeAiOauth"]["refreshToken"], "crt-b");
    }

    #[test]
    fn file_and_keychain_names_use_the_raw_email() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        let fake = Fake::new();
        fake.reply(&creds("kc-a").to_string()); // slot 1 from the Keychain
        fake.reply(&creds("kc-b").to_string()); // slot 2 from the Keychain
        let out = envelope(&dir, &fake, true).unwrap();
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains(&"account-1-alice@example.com".to_string()), "{:?}", calls[0]);
        assert!(calls[0].contains(&KEYCHAIN_SERVICE.to_string()));
        assert!(calls[1].contains(&"account-2-Bob+x@Example.com".to_string()), "raw email, not lowercased: {:?}", calls[1]);
        let accounts = out.envelope["accounts"].as_array().unwrap();
        assert_eq!(accounts[0]["credentials"]["claudeAiOauth"]["refreshToken"], "kc-a");
        assert_eq!(accounts[1]["credentials"]["claudeAiOauth"]["refreshToken"], "kc-b");
    }

    #[test]
    fn keychain_failure_falls_back_to_the_enc_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        let fake = Fake::new();
        fake.fail(); // slot 1: Keychain locked → .enc
        // slot 2: default reply = item not found (44) → .enc
        let out = envelope(&dir, &fake, true).unwrap();
        let accounts = out.envelope["accounts"].as_array().unwrap();
        assert_eq!(accounts[0]["credentials"]["claudeAiOauth"]["refreshToken"], "crt-a");
        assert_eq!(accounts[1]["credentials"]["claudeAiOauth"]["refreshToken"], "crt-b");
        assert_eq!(out.readable, 2);
    }

    #[test]
    fn a_corrupt_enc_file_skips_that_account_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        fs::write(dir.join("credentials/.creds-2-Bob+x@Example.com.enc"), "%%not-base64%%").unwrap();
        let out = envelope(&dir, &Fake::new(), false).unwrap();
        assert_eq!(out.readable, 1);
        let accounts = out.envelope["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["number"], 1);
        assert_eq!(out.notices.len(), 1);
        assert!(out.notices[0].contains("Skipping Account-2 (Bob+x@Example.com)"), "{}", out.notices[0]);
    }

    #[test]
    fn a_slot_with_no_credentials_anywhere_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        fs::remove_file(dir.join("credentials/.creds-1-alice@example.com.enc")).unwrap();
        let out = envelope(&dir, &Fake::new(), true).unwrap();
        assert_eq!(out.readable, 1);
        assert_eq!(out.envelope["accounts"].as_array().unwrap()[0]["number"], 2);
        assert!(out.notices[0].contains("no stored credentials"), "{}", out.notices[0]);
    }

    #[test]
    fn default_dir_follows_the_platform() {
        let dir = default_dir().unwrap();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        if cfg!(target_os = "linux") {
            assert_eq!(name, "claude-swap");
        } else {
            assert_eq!(name, ".claude-swap-backup");
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cargo test --lib cswap_store 2>&1 | head -20`
Expected: `error[E0432]`/`E0425` — `probe`, `envelope`, `retire`, `default_dir`, `Probe`, `KEYCHAIN_SERVICE` not found (the module is empty apart from tests).

- [ ] **Step 3: Write the module**

Prepend to `src/cswap_store.rs` (above the test module):

```rust
//! Read a claude-swap store directly, as the shape of a cswap export, so
//! `import --from-cswap` can reuse the bundle importer (ccsw-engine spec,
//! "ccsw side"). Layout from claude-swap 0.25.0: `sequence.json` holds the
//! roster; per-account credentials are a Keychain item (macOS, service
//! `claude-swap`, account `account-{n}-{email}`) with a base64 `.enc` file
//! fallback (`credentials/.creds-{n}-{email}.enc`, the only backend off
//! macOS); `configs/.claude-config-{n}-{email}.json` is the `.claude.json`
//! snapshot the export's `config` field carries.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::claude::keychain::{Keychain, SecurityCli};
use crate::errors::{CcswError, Result};
use crate::paths::user_home;
use crate::transfer::platform_name;

pub const KEYCHAIN_SERVICE: &str = "claude-swap";
const ROSTER: &str = "sequence.json";
const RETIRED_MARK: &str = ".migrated-";

/// Where claude-swap kept its store on this platform (`paths.py` in
/// claude-swap: macOS/Windows `~/.claude-swap-backup`, Linux/WSL
/// `$XDG_DATA_HOME/claude-swap`).
pub fn default_dir() -> Option<PathBuf> {
    let home = user_home()?;
    if cfg!(target_os = "linux") {
        let xdg = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        Some(xdg.join("claude-swap"))
    } else {
        Some(home.join(".claude-swap-backup"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// No directory (or no `sequence.json`) at the path.
    Missing,
    /// The directory is gone but a `<dir>.migrated-*` sibling exists.
    Retired,
    /// A roster with no accounts.
    Empty,
    Ready { accounts: usize },
}

pub fn probe(dir: &Path) -> Result<Probe> {
    if !dir.join(ROSTER).is_file() {
        return Ok(if retired_sibling_exists(dir) { Probe::Retired } else { Probe::Missing });
    }
    let roster = read_roster(dir)?;
    let count = roster_accounts(&roster).len();
    Ok(if count == 0 { Probe::Empty } else { Probe::Ready { accounts: count } })
}

fn retired_sibling_exists(dir: &Path) -> bool {
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        return false;
    };
    let prefix = format!("{}{}", name.to_string_lossy(), RETIRED_MARK);
    fs::read_dir(parent)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        })
        .unwrap_or(false)
}

fn read_roster(dir: &Path) -> Result<Map<String, Value>> {
    let path = dir.join(ROSTER);
    let text = fs::read_to_string(&path).map_err(|err| {
        CcswError::transfer(format!("could not read {}: {err}", path.display()))
    })?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(CcswError::transfer(format!("{} is not a JSON object", path.display()))),
        Err(err) => Err(CcswError::transfer(format!("{} is not valid JSON: {err}", path.display()))),
    }
}

/// `(slot number, record)` pairs in ascending slot order.
fn roster_accounts(roster: &Map<String, Value>) -> Vec<(u32, Map<String, Value>)> {
    let mut out: Vec<(u32, Map<String, Value>)> = roster
        .get("accounts")
        .and_then(Value::as_object)
        .map(|accounts| {
            accounts
                .iter()
                .filter_map(|(slot, record)| {
                    Some((slot.parse::<u32>().ok()?, record.as_object()?.clone()))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|(slot, _)| *slot);
    out
}

pub struct StoreEnvelope {
    /// A cswap export (`version: 1`, `swapVersion`, `accounts[]`) ready for
    /// `transfer::import_accounts`.
    pub envelope: Value,
    /// Skipped-slot notices, in slot order.
    pub notices: Vec<String>,
    /// Accounts that made it into the envelope.
    pub readable: usize,
}

/// Build the envelope. `use_keychain` is `paths.keychain_enabled` (macOS
/// with `CCSW_KEYCHAIN` not off): the Keychain item is tried first and the
/// `.enc` file is the fallback; off macOS only the file is read.
pub fn envelope(dir: &Path, security: &dyn SecurityCli, use_keychain: bool) -> Result<StoreEnvelope> {
    let roster = read_roster(dir)?;
    let keychain = Keychain::new(security);
    let mut accounts = Vec::new();
    let mut notices = Vec::new();
    for (slot, record) in roster_accounts(&roster) {
        let email = record
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if email.is_empty() {
            notices.push(format!("Skipping Account-{slot}: no email in {ROSTER}"));
            continue;
        }
        let credentials = match read_credentials(dir, &keychain, use_keychain, slot, &email) {
            Ok(Some(value)) => value,
            Ok(None) => {
                notices.push(format!(
                    "Skipping Account-{slot} ({email}): no stored credentials — add it again with: ccsw add claude"
                ));
                continue;
            }
            Err(reason) => {
                notices.push(format!("Skipping Account-{slot} ({email}): {reason}"));
                continue;
            }
        };
        let mut entry = Map::new();
        entry.insert("number".into(), json!(slot));
        entry.insert("email".into(), json!(email));
        for key in ["organizationUuid", "organizationName", "uuid", "added"] {
            if let Some(value) = record.get(key).filter(|v| v.is_string()) {
                entry.insert(key.into(), value.clone());
            }
        }
        entry.insert("credentials".into(), credentials);
        if let Some(config) = read_config(dir, slot, &email) {
            entry.insert("config".into(), config);
        }
        accounts.push(Value::Object(entry));
    }
    let readable = accounts.len();
    let envelope = json!({
        "version": 1,
        "swapVersion": "store",
        "exportedFrom": platform_name(),
        "exportedAt": crate::model::now_iso(),
        "encrypted": false,
        "activeAccountNumber": roster.get("activeAccountNumber").cloned().unwrap_or(Value::Null),
        "accounts": accounts,
    });
    Ok(StoreEnvelope { envelope, notices, readable })
}

/// Keychain first (when enabled), then the `.enc` file. `Ok(None)` = no
/// credential anywhere; `Err(reason)` = a copy exists but is unreadable.
fn read_credentials(
    dir: &Path,
    keychain: &Keychain<'_>,
    use_keychain: bool,
    slot: u32,
    email: &str,
) -> std::result::Result<Option<Value>, String> {
    if use_keychain {
        let account = format!("account-{slot}-{email}");
        match keychain.get_password(KEYCHAIN_SERVICE, &account) {
            Ok(Some(text)) => {
                return serde_json::from_str::<Value>(&text)
                    .map(Some)
                    .map_err(|err| format!("Keychain item is not JSON: {err}"));
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(slot, error = %err, "claude-swap Keychain item unreadable; trying the .enc file");
            }
        }
    }
    let path = dir.join("credentials").join(format!(".creds-{slot}-{email}.enc"));
    let encoded = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("could not read {}: {err}", path.display())),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|err| format!("{} is not base64: {err}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .map(Some)
        .map_err(|err| format!("{} does not hold JSON credentials: {err}", path.display()))
}

/// The `.claude.json` snapshot claude-swap kept per account, when present
/// and a JSON object.
fn read_config(dir: &Path, slot: u32, email: &str) -> Option<Value> {
    let path = dir.join("configs").join(format!(".claude-config-{slot}-{email}.json"));
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&text).ok().filter(Value::is_object)
}

/// Rename the store to `<dir>.migrated-<YYYYMMDD-HHMMSS>` so a leftover
/// cswap finds no accounts. Reversible by renaming back.
pub fn retire(dir: &Path) -> Result<PathBuf> {
    let stamp = crate::model::now_iso()
        .replace(['-', ':'], "")
        .replace('T', "-")
        .trim_end_matches('Z')
        .to_string();
    let mut name = dir
        .file_name()
        .ok_or_else(|| CcswError::transfer("store path has no directory name"))?
        .to_os_string();
    name.push(format!("{RETIRED_MARK}{stamp}"));
    let target = dir.with_file_name(name);
    fs::rename(dir, &target).map_err(|err| {
        CcswError::transfer(format!(
            "could not retire {} → {}: {err}",
            dir.display(),
            target.display()
        ))
    })?;
    Ok(target)
}
```

Add `pub mod cswap_store;` to `src/lib.rs` after line 27 (`pub mod transfer;`). If `crate::model::now_iso` is not `pub`, use the same function the transfer module already imports (`use crate::model::{..., now_iso, ...}` at `transfer.rs:28`) — it is `pub`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib cswap_store`
Expected: 7 passed. If `default_dir_follows_the_platform` fails on a CI runner without `HOME`, that is a test-environment problem: `user_home()` returns `None` only when neither `HOME` nor the passwd entry exists; keep the test.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/cswap_store.rs src/lib.rs Cargo.toml Cargo.lock
git commit -m "feat(transfer): read a claude-swap store as a cswap export envelope"
```

---

### Task 2: `ImportSource::CswapStore`, retire-on-success, exit codes, `--json` report

**Files:**
- Modify: `src/transfer.rs` — `ImportSource` (line ~44), `import_cmd` (line ~155), `read_source` (line ~610)
- Test: `tests/cli_transfer.rs` (new tests at the end of the file)

**Interfaces:**
- Consumes: Task 1 (`cswap_store::{default_dir, probe, envelope, retire, Probe}`); `crate::claude::keychain::SystemSecurity`; `paths.keychain_enabled` (`src/paths.rs:22`); `crate::jsonout::render_document` (used by `cli/list.rs` for `--json` output) — check its signature: `pub fn render_document(value: &Value) -> String`.
- Produces:
  ```rust
  pub enum ImportSource { File(PathBuf), Stdin, CswapStore { dir: PathBuf } }
  pub struct ImportOptions { pub force: bool, pub from_cswap: bool, pub retire: bool, pub json: bool }
  pub fn import_cmd(paths: &Paths, path: &str, opts: ImportOptions) -> i32;   // exit code
  pub const EXIT_NOTHING_TO_IMPORT: i32 = 2;
  ```
  The `--json` document (stdout, exactly one):
  `{"schemaVersion": 2, "imported": n, "overwritten": n, "skipped": n, "replaced": n, "retired": "<path>" | null}` plus `"reason": "no-store" | "empty" | "retired"` on exit 2.

- [ ] **Step 1: Write the failing integration tests**

Append to `tests/cli_transfer.rs`:

```rust
fn cswap_store(root: &std::path::Path, emails: &[(u32, &str, &str)]) -> std::path::PathBuf {
    use base64::Engine as _;
    let dir = root.join(".claude-swap-backup");
    std::fs::create_dir_all(dir.join("credentials")).unwrap();
    std::fs::create_dir_all(dir.join("configs")).unwrap();
    let mut accounts = serde_json::Map::new();
    for (slot, email, refresh) in emails {
        accounts.insert(
            slot.to_string(),
            json!({"email": email, "organizationUuid": "org-t", "organizationName": "Team",
                   "uuid": format!("u-{slot}"), "added": "2026-01-01T00:00:00Z"}),
        );
        let creds = json!({"claudeAiOauth": {"accessToken": "at", "refreshToken": refresh,
                           "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]}});
        std::fs::write(
            dir.join("credentials").join(format!(".creds-{slot}-{email}.enc")),
            base64::engine::general_purpose::STANDARD.encode(creds.to_string()),
        )
        .unwrap();
        std::fs::write(
            dir.join("configs").join(format!(".claude-config-{slot}-{email}.json")),
            json!({"oauthAccount": {"emailAddress": email, "organizationUuid": "org-t",
                   "organizationName": "Team", "accountUuid": format!("u-{slot}")}}).to_string(),
        )
        .unwrap();
    }
    std::fs::write(
        dir.join("sequence.json"),
        json!({"accounts": accounts, "activeAccountNumber": emails.first().map(|e| e.0)}).to_string(),
    )
    .unwrap();
    dir
}

#[test]
fn import_from_cswap_reads_the_default_store_and_retires_it() {
    let cli = Cli::new();
    // HOME is the test root (support::Cli), so the default dir is under it.
    let dir = cswap_store(cli.root.path(), &[(1, "one@example.com", "crt-1"), (2, "two@example.com", "crt-2")]);
    let run = cli.run(&["import", "--from-cswap", "--retire", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let report = run.json();
    assert_eq!(report["schemaVersion"], 2);
    assert_eq!(report["imported"], 2);
    assert_eq!(report["skipped"], 0);
    let retired = report["retired"].as_str().expect("retired path");
    assert!(retired.contains(".claude-swap-backup.migrated-"), "{retired}");
    assert!(!dir.exists(), "the store was renamed");
    assert!(std::path::Path::new(retired).join("sequence.json").is_file());
    let roster = cli.roster();
    assert_eq!(roster["accounts"]["1"]["provider"], "claude");
    assert_eq!(roster["accounts"]["1"]["email"], "one@example.com");
    assert_eq!(roster["accounts"]["2"]["email"], "two@example.com");
    assert_eq!(cli.credential(1)["claudeAiOauth"]["refreshToken"], "crt-1");
    // No live Claude login in the test home: the imported active slot seeds the roster.
    assert_eq!(roster["activeByProvider"]["claude"], 1);
}

#[test]
fn a_second_run_after_retire_exits_2() {
    let cli = Cli::new();
    cswap_store(cli.root.path(), &[(1, "one@example.com", "crt-1")]);
    assert_eq!(cli.run(&["import", "--from-cswap", "--retire"]).status, 0);
    let again = cli.run(&["import", "--from-cswap", "--retire", "--json"]);
    assert_eq!(again.status, 2, "{}", again.stderr);
    let report = again.json();
    assert_eq!(report["imported"], 0);
    assert_eq!(report["reason"], "retired");
    assert_eq!(report["retired"], serde_json::Value::Null);
    assert_eq!(cli.roster()["accounts"].as_object().unwrap().len(), 1, "nothing imported twice");
}

#[test]
fn import_from_cswap_without_a_store_exits_2_and_says_so() {
    let cli = Cli::new();
    let run = cli.run(&["import", "--from-cswap"]);
    assert_eq!(run.status, 2, "{}", run.stderr);
    assert!(run.stderr.contains("no claude-swap store"), "{}", run.stderr);
    let run = cli.run(&["import", "--from-cswap", "--json"]);
    assert_eq!(run.status, 2);
    assert_eq!(run.json()["reason"], "no-store");
}

#[test]
fn import_from_cswap_accepts_an_explicit_directory_and_keeps_it_without_retire() {
    let cli = Cli::new();
    let dir = cswap_store(&cli.root.path().join("elsewhere"), &[(3, "three@example.com", "crt-3")]);
    let run = cli.run(&["import", "--from-cswap", dir.to_str().unwrap()]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stderr.contains("Imported three@example.com → slot 1"), "{}", run.stderr);
    assert!(dir.join("sequence.json").is_file(), "no --retire → untouched");
}

#[test]
fn all_accounts_already_present_imports_nothing_and_keeps_the_store() {
    let cli = Cli::new();
    let dir = cswap_store(cli.root.path(), &[(1, "one@example.com", "crt-1")]);
    assert_eq!(cli.run(&["import", "--from-cswap"]).status, 0);
    // Same identity again, no --force: the importer skips it.
    let run = cli.run(&["import", "--from-cswap", "--retire", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let report = run.json();
    assert_eq!(report["imported"], 0);
    assert_eq!(report["skipped"], 1);
    assert_eq!(report["retired"], serde_json::Value::Null, "imported 0 → not retired");
    assert!(dir.join("sequence.json").is_file());
}

#[test]
fn a_corrupt_slot_is_reported_and_the_rest_imported() {
    let cli = Cli::new();
    let dir = cswap_store(cli.root.path(), &[(1, "one@example.com", "crt-1"), (2, "two@example.com", "crt-2")]);
    std::fs::write(dir.join("credentials/.creds-2-two@example.com.enc"), "@@@").unwrap();
    let run = cli.run(&["import", "--from-cswap", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.json()["imported"], 1);
    assert!(run.stderr.contains("Skipping Account-2 (two@example.com)"), "{}", run.stderr);
}
```

Add `base64 = "0.22"` under `[dev-dependencies]` too if the test crate cannot see it (dev-deps are separate; the dependency from Task 1 is a normal dependency, so `base64::` is visible to integration tests only if listed in dev-dependencies as well — add it).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test cli_transfer from_cswap`
Expected: FAIL — `ccsw: error: unrecognized arguments: --from-cswap` (status 2 from the parser, and the `--json` runs fail the same way).

- [ ] **Step 3: Implement the source, options and exit codes**

In `src/transfer.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportSource {
    File(PathBuf),
    Stdin,
    /// A claude-swap store directory (`import --from-cswap`).
    CswapStore { dir: PathBuf },
}

/// Flags of `import` as the front controller parsed them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportOptions {
    pub force: bool,
    pub from_cswap: bool,
    pub retire: bool,
    pub json: bool,
}

/// `import --from-cswap` found nothing to import.
pub const EXIT_NOTHING_TO_IMPORT: i32 = 2;

/// `import PATH` as the front controller calls it: `-` is stdin. With
/// `--from-cswap`, PATH is the store directory (default: the platform's
/// claude-swap location). Returns the exit status.
pub fn import_cmd(paths: &Paths, path: &str, opts: ImportOptions) -> i32 {
    if opts.from_cswap {
        return import_from_cswap_cmd(paths, path, opts);
    }
    let source = if path == "-" {
        ImportSource::Stdin
    } else {
        ImportSource::File(expand_tilde(path))
    };
    exit_status(import_accounts(paths, source, opts.force).map(|_| ()))
}

fn import_from_cswap_cmd(paths: &Paths, path: &str, opts: ImportOptions) -> i32 {
    let dir = if path.is_empty() {
        match crate::cswap_store::default_dir() {
            Some(dir) => dir,
            None => return fail(opts.json, "could not determine the home directory"),
        }
    } else {
        expand_tilde(path)
    };
    let probe = match crate::cswap_store::probe(&dir) {
        Ok(probe) => probe,
        Err(err) => return fail(opts.json, &err.to_string()),
    };
    let reason = match probe {
        crate::cswap_store::Probe::Missing => Some("no-store"),
        crate::cswap_store::Probe::Retired => Some("retired"),
        crate::cswap_store::Probe::Empty => Some("empty"),
        crate::cswap_store::Probe::Ready { .. } => None,
    };
    if let Some(reason) = reason {
        let human = match reason {
            "no-store" => format!("no claude-swap store at {}", dir.display()),
            "retired" => format!("the claude-swap store at {} was already migrated", dir.display()),
            _ => format!("the claude-swap store at {} has no accounts", dir.display()),
        };
        if opts.json {
            print!("{}", crate::jsonout::render_document(&json!({
                "schemaVersion": crate::model::SCHEMA_VERSION,
                "imported": 0, "overwritten": 0, "skipped": 0, "replaced": 0,
                "retired": Value::Null, "reason": reason,
            })));
        } else {
            printer::error(&format!("Nothing to import: {human}"));
        }
        return EXIT_NOTHING_TO_IMPORT;
    }
    let report = match import_accounts(paths, ImportSource::CswapStore { dir: dir.clone() }, opts.force) {
        Ok(report) => report,
        Err(err) => return fail(opts.json, &err.to_string()),
    };
    let retired = if opts.retire && report.imported >= 1 {
        match crate::cswap_store::retire(&dir) {
            Ok(target) => {
                printer::error(&format!("Retired the claude-swap store → {}", target.display()));
                Some(target)
            }
            Err(err) => return fail(opts.json, &err.to_string()),
        }
    } else {
        None
    };
    if opts.json {
        print!("{}", crate::jsonout::render_document(&json!({
            "schemaVersion": crate::model::SCHEMA_VERSION,
            "imported": report.imported, "overwritten": report.overwritten,
            "skipped": report.skipped, "replaced": report.replaced,
            "retired": retired.as_ref().map(|p| p.display().to_string()),
        })));
    }
    0
}

/// Error → exit 1, as the `--json` envelope or a stderr line.
fn fail(json: bool, message: &str) -> i32 {
    if json {
        print!("{}", crate::jsonout::render_document(&json!({
            "schemaVersion": crate::model::SCHEMA_VERSION,
            "error": {"type": "transfer", "message": message},
        })));
    } else {
        printer::error(&format!("Error: {message}"));
    }
    1
}
```

`printer::error` writes to stderr (it is what `exit_status` uses). `import_accounts` already prints its notices to stderr as they happen (module doc, `transfer.rs:4-6`); keep that so `Imported … → slot n` lines appear in both modes (stdout stays pure JSON).

In `read_source`, add the variant:

```rust
        ImportSource::CswapStore { dir } => {
            let paths = Paths::from_env()?;
            let store = crate::cswap_store::envelope(
                dir,
                &crate::claude::keychain::SystemSecurity,
                paths.keychain_enabled,
            )?;
            for notice in &store.notices {
                printer::error(notice);
            }
            if store.readable == 0 {
                return Err(CcswError::transfer(format!(
                    "no readable credentials in {}",
                    dir.display()
                )));
            }
            serde_json::to_vec(&store.envelope)
                .map_err(|err| CcswError::transfer(format!("could not encode the store: {err}")))
        }
```

If `read_source` has no access to `Paths`, thread `paths` through: `import_accounts` already receives `paths`, so change `read_source(&source)` to `read_source(paths, &source)` and add the parameter.

Import `json!` and `Value` at the top of `transfer.rs` if not already imported (`use serde_json::{Value, json};`). `crate::model::SCHEMA_VERSION` is the `--json` schema constant (`model.rs:13`).

- [ ] **Step 4: Wire the CLI (parser + dispatch)**

`src/cli/legacy.rs`:

1. `Options` (line ~130): add `pub from_cswap: bool,` and `pub retire: bool,` after `pub force: bool,`.
2. Parse arms (line ~180): add
   ```rust
            "--from-cswap" => opts.from_cswap = true,
            "--retire" => opts.retire = true,
   ```
3. The `--import` arm (line ~253) must accept a missing value (mirror the `--add-token` arm at line ~226):
   ```rust
            "--import" => {
                let value = match &inline_value {
                    Some(value) => value.clone(),
                    None => match argv.get(i + 1) {
                        Some(next) if !looks_like_option(next, false) => {
                            i += 1;
                            next.clone()
                        }
                        _ => String::new(),
                    },
                };
                select(&mut opts, Command::Import(value))?;
            }
   ```
4. `validate` (line ~292), after the `--force` check:
   ```rust
    if matches!(command, Some(Import(path)) if path.is_empty()) && !opts.from_cswap {
        return Err("argument --import: expected one argument".into());
    }
    if opts.from_cswap && !is(|c| matches!(c, Import(_))) {
        return Err("--from-cswap can only be used with 'import'".into());
    }
    if opts.retire && !opts.from_cswap {
        return Err("--retire can only be used with 'import --from-cswap'".into());
    }
   ```
   and widen the `--json` check: `if opts.json && !is(|c| matches!(c, List | Status | Switch | SwitchTo(_) | Import(_)))` with the message `"--json can only be used with 'list', 'status', 'switch', or 'import'"` (update the existing test string at line ~570).
5. Help text (line ~381): change the import line to
   `  ccsw import <path> [--force]     import accounts (a .ccsw / .cswap export)` and add
   `  ccsw import --from-cswap [DIR] [--retire]`
   `                                  import a claude-swap store directly (default DIR: the platform's claude-swap location); --retire renames it afterwards`
   plus option lines under `options:`:
   ```
  --from-cswap          With 'import': read a claude-swap store directory instead of an export file
  --retire              With 'import --from-cswap': rename the store to <dir>.migrated-<stamp> once at least one account was imported
   ```
   and update the `--json` option text to mention `'import'`.
6. `src/cli/mod.rs:120`:
   ```rust
        Command::Import(path) => Ok(crate::transfer::import_cmd(
            &switcher.store.paths,
            &path,
            crate::transfer::ImportOptions {
                force: opts.force,
                from_cswap: opts.from_cswap,
                retire: opts.retire,
                json,
            },
        )),
   ```

Unit tests in `legacy.rs` (`cross_flag_validation_in_order`, `parse_selects_commands_and_options`, `help_and_version`):

```rust
        assert_eq!(
            check(&["--list", "--from-cswap"]),
            "--from-cswap can only be used with 'import'"
        );
        assert_eq!(
            check(&["--import", "f", "--retire"]),
            "--retire can only be used with 'import --from-cswap'"
        );
        assert_eq!(
            check(&["--import"]),
            "argument --import: expected one argument"
        );
```
in the ok list: `&["--import", "--from-cswap", "--retire", "--json"]`, `&["--import", "/tmp/s", "--from-cswap"]`;
in `parse_selects_commands_and_options`:
```rust
        let opts = parse(&argv(&["--import", "--from-cswap", "--retire"])).unwrap();
        assert_eq!(opts.command, Some(Command::Import(String::new())));
        assert!(opts.from_cswap && opts.retire);
```
in `help_and_version`: `assert!(help.contains("--from-cswap"));`.

- [ ] **Step 5: Run everything**

Run: `cargo test --lib legacy && cargo test --test cli_transfer`
Expected: all pass, including the six new integration tests. If `imports_a_cswap_export_from_stdin` (existing) changed its stderr because `import_accounts` now also prints a `Retired…` line — it must not: retire notices are printed only in `import_from_cswap_cmd`.

- [ ] **Step 6: fmt, clippy, full suite, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/transfer.rs src/cli/legacy.rs src/cli/mod.rs tests/cli_transfer.rs Cargo.toml Cargo.lock
git commit -m "feat(import): --from-cswap reads a claude-swap store, --retire renames it, --json reports"
```

---

### Task 3: musl release target, docs, version 0.8.0

**Files:**
- Modify: `.github/workflows/release.yml:17-33` (matrix + toolchain step)
- Modify: `README.md` (Install section line ~32-40; import paragraph line ~281; command list line ~273)
- Modify: `CHANGELOG.md` (new top entry), `handover.md` (status bullet + current workspace line), `Cargo.toml:3`, `Cargo.lock` (package `ccsw` version)
- Modify: `src/claude/usage.rs:26` only if `claude --version` on the release machine differs from `FALLBACK_CLI_VERSION`

**Interfaces:**
- Produces: release assets `ccsw-v0.8.0-x86_64-unknown-linux-musl.tar.gz` (new) alongside the four existing ones, all listed in `SHA256SUMS`.

- [ ] **Step 1: Add the musl target to the matrix**

In `release.yml` after the `x86_64-unknown-linux-gnu` entry:

```yaml
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
```

Add a step before the build step (bash on every OS, no-op elsewhere):

```yaml
      - name: musl tooling
        if: matrix.target == 'x86_64-unknown-linux-musl'
        run: sudo apt-get update && sudo apt-get install -y musl-tools
```

The existing `dtolnay/rust-toolchain@stable` step already receives `targets: ${{ matrix.target }}`, so `rustup target add` is covered. The Package step's `*)` branch already handles the new target (tar.gz with `ccsw` inside).

- [ ] **Step 2: Prove the musl build locally**

Run (macOS host): `rustup target add x86_64-unknown-linux-musl && cargo build --release --locked --target x86_64-unknown-linux-musl 2>&1 | tail -3`
Expected: on macOS this needs a musl cross linker and may fail at link time; if so, run the check in Docker instead:
```bash
docker run --rm --platform linux/amd64 -v "$PWD":/src -w /src rust:1.88-slim bash -c \
  'apt-get update >/dev/null && apt-get install -y -q musl-tools >/dev/null && rustup target add x86_64-unknown-linux-musl && cargo build --release --locked --target x86_64-unknown-linux-musl && file target/x86_64-unknown-linux-musl/release/ccsw && ./target/x86_64-unknown-linux-musl/release/ccsw --version'
```
Expected: `statically linked` in the `file` output and `ccsw 0.8.0`. (Use a Rust image at or above the toolchain floor named in README "Rust 1.88 or newer".)

- [ ] **Step 3: Docs + version**

- `Cargo.toml` `version = "0.8.0"`; `Cargo.lock` package `ccsw` version `0.8.0` (edit the two lines, or run `cargo update -p ccsw --offline`).
- `CHANGELOG.md`, above `## v0.7.5 — 2026-10-09`:
  ```markdown
  ## v0.8.0 — 2026-10-10

  ### Added

  - `ccsw import --from-cswap [DIR] [--retire] [--json]` reads a claude-swap store
    directly — the roster in `sequence.json`, each account's credentials from the
    macOS Keychain (service `claude-swap`) or its base64 `.enc` file, and the
    `.claude.json` snapshot — and imports it through the regular importer. DIR
    defaults to claude-swap's location on the platform (`~/.claude-swap-backup`;
    `$XDG_DATA_HOME/claude-swap` on Linux). `--retire` renames the store to
    `<dir>.migrated-<stamp>` once at least one account was imported, so a leftover
    cswap finds no accounts. Exit 2 means there was nothing to import (no store,
    empty roster, already migrated); `--json` prints the report for scripts such
    as the Token Beats desktop app, which runs this once on first launch.
  - A static `x86_64-unknown-linux-musl` release archive, for hosts whose glibc is
    older than the builder's (WSL distributions).
  ```
- `README.md`: Install section — mention the musl archive ("Linux x86_64 (glibc, and a static musl build)"); command list line 273 → `ccsw import backup.ccsw [--force] / ccsw import --from-cswap [DIR] [--retire]`; after the import paragraph (line ~285) add two sentences: "`ccsw import --from-cswap` reads a claude-swap store in place (no export needed): the roster from `sequence.json`, credentials from the macOS Keychain or the `.enc` files, configs from `configs/`. Add `--retire` to rename the store afterwards so cswap cannot keep refreshing the same tokens; `--json` prints the report."
- `handover.md`: add a status bullet in the style of the existing ones (`import --from-cswap` + musl, PR number, 版本 `v0.8.0`, 发版清单核对 result for `FALLBACK_CLI_VERSION`), and set 当前工作区/分支 to `.worktrees/import-from-cswap` / `newbdez33/import-from-cswap`.
- Release checklist: run `claude --version`; if it differs from `FALLBACK_CLI_VERSION` in `src/claude/usage.rs:26`, update the constant and the test fixture that asserts it (`rg -n '2\.1\.295' src tests`).

- [ ] **Step 4: Gate + commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add .github/workflows/release.yml README.md CHANGELOG.md handover.md Cargo.toml Cargo.lock src/claude/usage.rs
git commit -m "chore(release): v0.8.0 — import --from-cswap, musl release target"
```

- [ ] **Step 5: PR, merge, tag**

```bash
git push -u origin newbdez33/import-from-cswap
gh pr create --repo newbdez33/ccsw --base main --head newbdez33/import-from-cswap \
  --title "feat(import): --from-cswap reads a claude-swap store; musl release target (v0.8.0)" \
  --body-file - <<'EOF'
(summarize Tasks 1–3; list the exit-code and --json contract from "Global Constraints"; note the desktop app depends on it)
EOF
# after review + green CI:
gh pr merge <n> --repo newbdez33/ccsw --squash
git fetch origin && git tag -a v0.8.0 -m "$(git log -1 --format=%s origin/main)" origin/main && git push origin v0.8.0
gh run watch --repo newbdez33/ccsw $(gh run list --repo newbdez33/ccsw --workflow release.yml --limit 1 --json databaseId -q '.[0].databaseId')
gh release view v0.8.0 --repo newbdez33/ccsw --json assets -q '.assets[].name'   # expect 5 archives + SHA256SUMS
```
Expected: `ccsw-v0.8.0-x86_64-unknown-linux-musl.tar.gz` is among the assets. No AI attribution footer in the commit or PR body.
