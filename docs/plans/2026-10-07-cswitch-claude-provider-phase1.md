# cswitch Claude provider — phase 1 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One `cswitch` roster that manages Codex *and* Claude Code accounts under one global slot space: `add` captures both live logins, `switch 2` acts on whichever provider slot 2 belongs to, `list` / `status` / `--json` report both, and the TUI shows a section per provider.

**Architecture:** A `Provider` tag on every roster record selects between the existing `src/codex/` mechanics and a new `src/claude/` tree ported from claude-swap (paths, Keychain via `/usr/bin/security`, credential read/write across backends, Claude Code's directory-lock protocol, OAuth refresh, usage API). The usage collector, the switcher and the CLI dispatch on the record's provider; the TUI groups one flat snapshot by provider. Phase 1 covers the roster, the Claude core, `add` / `add-token` / `switch` / `list` / `status`, JSON v2 and the TUI sections. Auto-switch (phase 2), session mode (phase 3) and export/import v2 (phase 4) get their own plans.

**Tech Stack:** Rust 2024 (MSRV 1.88), serde/serde_json, reqwest + tokio (existing), ratatui/crossterm (existing), sha2/hex (existing), `filetime` (new, for touching lock directories), axum (dev, existing mock server).

**Spec:** `docs/specs/2026-10-07-cswitch-claude-provider-design.md` (amends `docs/specs/2026-09-29-cswitch-design.md`). The research notes `docs/research/cswap-cli-contract.md`, `cswap-tui.md` and `cswap-model-autoswitch.md` are the contract where both specs are silent.

## Global Constraints

- Rust edition 2024, `rust-version = "1.88"`; the quality gate is `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` (every task ends green).
- Program name `cswitch`; help header `Multi-Account Switcher for OpenAI Codex and Claude Code`.
- The words `codex` and `claude` are reserved: refused as aliases (`alias 'claude' is reserved for the provider selector`), accepted as a selector argument of `switch`, `add`, `list`, `status` (phase 1).
- Slots are one global space; `add` allocates `max(existing)+1` across providers; `provider` is always written to `sequence.json` and defaults to `codex` on read.
- Codex behaviour for a single-provider store is unchanged: every existing integration test keeps passing except where this plan edits an assertion (JSON `schemaVersion` → 2, new `provider` field).
- Claude strings are verbatim from the spec §4: Keychain service `Claude Code-credentials` (live OAuth) and `Claude Code` (managed key), account `$USER`; locks `<home>/.oauth_refresh.lock` and `<home>.lock` (stale 60 s), `<global config>.lock` (stale 10 s), touched every 3 s, 9 s wait budget per lock; refresh `POST https://platform.claude.com/v1/oauth/token` with client id `9d1c250a-e61b-44d9-88ed-5944d1962f5e`; usage `GET https://api.anthropic.com/api/oauth/usage` with `anthropic-beta: oauth-2025-04-20`; follow-ups `Restart Claude Code to apply immediately — otherwise the session can take up to ~30 seconds to pick up the new account.` (Keychain) and `New account is active on your next message — no restart needed.` (file).
- `CSWITCH_KEYCHAIN=off` forces the file backend (tests, CI, and users who want it); `CSWITCH_CLAUDE_USAGE_URL` / `CSWITCH_CLAUDE_TOKEN_URL` override the endpoints; `CLAUDE_CONFIG_DIR` is honoured exactly as Claude Code does (`.claude.json` moves inside it).
- No network while any lock is held. Every file write is atomic and 0600 (`fsutil::write_json_private` / `atomic_write_private`).
- The active Claude account is never refreshed by cswitch; a switch replaces only `claudeAiOauth` inside the live credential object and keeps every other top-level key.
- JSON payloads of `list`, `status`, `switch`, `config` and the error envelope carry `schemaVersion: 2`; `settings.json`, `cache/usage.json` and `autoswitch_state.json` keep their own versions; auto-switch events stay at 1 until phase 2.
- Commit after every task with a conventional message (`feat(...)`, `test(...)`, `docs(...)`); no `Co-Authored-By` lines.

## Review Focus

1. **A live `.credentials.json` with sibling keys** (Claude Code's `mcpOAuth` tokens next to `claudeAiOauth`): a switch must keep the siblings byte-for-byte and replace only `claudeAiOauth` — pinned in Task 6 (`replace_oauth_keeps_siblings_and_oauth_only_strips_them`) and Task 16 (`switch_claude_keeps_sibling_keys`).
2. **A v0.1 `sequence.json` with no `provider` field**: it must load as all-Codex, `switch 2` must still rewrite `auth.json`, and the next write must add `"provider": "codex"` — Task 1 (`provider_defaults_to_codex_on_read`) and Task 16 (`v1_roster_is_read_as_codex`).
3. **`cswitch switch <claude slot>` while a Codex account is live**: `auth.json`, its backups and the Codex daemon must be untouched — Task 16 (`switching_a_claude_slot_leaves_codex_alone`).
4. **The same email under both providers** (a Codex login and a Claude login for `me@example.com`): `add` must create two slots, never fold one into the other — Task 1 (`find_slot_is_scoped_by_provider`) and Task 16 (`add_captures_both_logins_even_with_the_same_email`).
5. **A lock directory left behind by a crashed Claude Code**: a directory older than its staleness is taken over, a fresh one makes the switch fail with `LockError` after the budget instead of hanging — Task 8 (`stale_lock_is_stolen`, `fresh_lock_times_out`).

---

## File structure

New files:

| path | responsibility |
|---|---|
| `src/provider.rs` | `Provider { Codex, Claude }`: serde `"codex"`/`"claude"`, `as_str`, `title`, `tool_name`, `parse_selector`. |
| `src/claude/mod.rs` | module root, re-exports. |
| `src/claude/paths.rs` | Claude config home, global config path (legacy rule), credentials path, lock paths, backup dir. |
| `src/claude/keychain.rs` | `/usr/bin/security` wrapper behind a `SecurityCli` trait (get / set via hex on stdin / delete / exists, 5 s timeout), `account_name()`. |
| `src/claude/credentials.rs` | `ClaudeCredential`, `OauthAccount`, `SlotFile` (pure model + freshness + fingerprint); `ClaudeLive` (read/write across Keychain and file, global-config splice, backups). |
| `src/claude/locks.rs` | Claude Code's proper-lockfile directory locks with staleness, touching and a wait budget. |
| `src/claude/oauth.rs` | token refresh against platform.claude.com. |
| `src/claude/usage.rs` | usage client and normalization (`spend`, `scoped`). |
| `tests/cli_claude.rs` | end-to-end tests of the mixed roster through the binary. |

Modified files (owner task in parentheses): `src/model.rs` (1, 2, 3), `src/store/roster.rs` (1), `src/store/credentials.rs` (11), `src/store/usage_store.rs` (3), `src/usage_math.rs` (2), `src/jsonout.rs` (2, 3), `src/paths.rs` (4), `src/store/mod.rs` (4), `src/codex/usage.rs` (10), `src/collect.rs` (11), `src/switcher.rs` (12, 13, 14), `src/cli/{legacy,mod,accounts,list,switch}.rs` (15), `src/tui/{snapshot,widgets,switch,dashboard,modals,app,worker,test_support}.rs` (17), `tests/support/{mod,usage_mock}.rs` (16), `tests/{cli_list,cli_switch,cli_accounts,config_cmd,tui_render}.rs` (3, 18), `examples/tui_screenshot.rs` (18), `README.md`, `CHANGELOG.md` (18), `Cargo.toml` (8).

---

### Task 1: `Provider` and the provider-tagged roster

**Files:**
- Create: `src/provider.rs`
- Modify: `src/lib.rs`, `src/model.rs`, `src/store/roster.rs`, `src/collect.rs:93`, `src/switcher.rs:415,565,622`, `src/transfer.rs:375,692`, `src/session.rs:108,671`
- Test: unit tests inside `src/provider.rs`, `src/model.rs`, `src/store/roster.rs`

**Interfaces:**
- Produces: `crate::provider::Provider` (`Copy`, `Default = Codex`, serde lowercase, `as_str() -> &'static str`, `title() -> &'static str` (`Codex` / `Claude`), `tool_name()` (`Codex` / `Claude Code`), `parse_selector(&str) -> Option<Provider>`, `Provider::ALL`); `AccountRecord.provider: Provider`; `Roster.active_by_provider: BTreeMap<String, u32>` with `active_for(Provider) -> Option<u32>` and `set_active_for(Provider, Option<u32>)`; `Roster::find_slot(provider, &Identity)`; `Roster::slots_of(provider) -> Vec<u32>`; `normalize_alias` rejects the selector words.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing provider tests**

Create `src/provider.rs` with only the tests first:

```rust
//! The two account providers cswitch manages and the selector grammar.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Codex,
    Claude,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_and_strings() {
        assert_eq!(serde_json::to_value(Provider::Codex).unwrap(), "codex");
        assert_eq!(serde_json::to_value(Provider::Claude).unwrap(), "claude");
        let back: Provider = serde_json::from_str("\"claude\"").unwrap();
        assert_eq!(back, Provider::Claude);
        assert_eq!(Provider::default(), Provider::Codex);
        assert_eq!(Provider::Claude.as_str(), "claude");
        assert_eq!(Provider::Codex.title(), "Codex");
        assert_eq!(Provider::Claude.tool_name(), "Claude Code");
        assert_eq!(Provider::Codex.to_string(), "codex");
        assert_eq!(Provider::ALL, [Provider::Codex, Provider::Claude]);
    }

    #[test]
    fn selector_parsing_is_case_insensitive_and_strict() {
        assert_eq!(Provider::parse_selector("codex"), Some(Provider::Codex));
        assert_eq!(Provider::parse_selector("Claude"), Some(Provider::Claude));
        assert_eq!(Provider::parse_selector("claude "), None);
        assert_eq!(Provider::parse_selector("2"), None);
        assert_eq!(Provider::parse_selector("a@b.co"), None);
    }
}
```

Add `pub mod provider;` to `src/lib.rs` right after `pub mod paths;`.

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib provider::`
Expected: compile errors `no function or associated item named as_str` etc.

- [ ] **Step 3: Implement `Provider`**

Below the enum in `src/provider.rs`:

```rust
impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Codex, Provider::Claude];

    /// The roster / JSON / selector spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    /// Capitalized, for headings such as `Codex accounts:`.
    pub fn title(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
        }
    }

    /// The product whose login this provider manages.
    pub fn tool_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }

    /// `codex` / `claude` (any case) as a command-line selector; anything
    /// else is an account identifier.
    pub fn parse_selector(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            _ => None,
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
```

- [ ] **Step 4: Run the provider tests**

Run: `cargo test --lib provider::`
Expected: 2 passed.

- [ ] **Step 5: Write the failing model and roster tests**

In `src/model.rs` tests add:

```rust
    #[test]
    fn provider_defaults_to_codex_on_read_and_is_always_written() {
        let raw = serde_json::json!({
            "activeAccountNumber": 1, "lastUpdated": "x", "sequence": [1],
            "accounts": {"1": {"email": "a@b.c"}}
        });
        let roster: Roster = serde_json::from_value(raw).unwrap();
        assert_eq!(roster.record(1).unwrap().provider, Provider::Codex);
        assert!(roster.active_by_provider.is_empty());
        let json = serde_json::to_value(&roster).unwrap();
        assert_eq!(json["accounts"]["1"]["provider"], "codex");
        assert!(json.get("activeByProvider").is_none(), "empty map is omitted");
    }

    #[test]
    fn active_by_provider_mirrors_the_codex_slot() {
        let mut roster = Roster::empty();
        roster.add_record(1, AccountRecord::new("a@b.c"));
        let mut claude = AccountRecord::new("c@b.c");
        claude.provider = Provider::Claude;
        roster.add_record(5, claude);
        roster.set_active_for(Provider::Claude, Some(5));
        assert_eq!(roster.active_account_number, None);
        assert_eq!(roster.active_for(Provider::Claude), Some(5));
        roster.set_active_for(Provider::Codex, Some(1));
        assert_eq!(roster.active_account_number, Some(1));
        assert_eq!(roster.active_for(Provider::Codex), Some(1));
        roster.set_active(Some(1));
        let json = serde_json::to_value(&roster).unwrap();
        assert_eq!(json["activeByProvider"], serde_json::json!({"claude": 5, "codex": 1}));
        roster.set_active_for(Provider::Claude, None);
        assert_eq!(roster.active_for(Provider::Claude), None);
    }

    #[test]
    fn find_slot_is_scoped_by_provider() {
        let mut roster = Roster::empty();
        let mut codex = AccountRecord::new("me@example.com");
        codex.organization_uuid = "acct-1".into();
        let mut claude = AccountRecord::new("me@example.com");
        claude.organization_uuid = "acct-1".into();
        claude.provider = Provider::Claude;
        roster.add_record(1, codex);
        roster.add_record(2, claude);
        let identity = Identity::new("me@example.com", "acct-1");
        assert_eq!(roster.find_slot(Provider::Codex, &identity), Some(1));
        assert_eq!(roster.find_slot(Provider::Claude, &identity), Some(2));
        assert_eq!(roster.slots_of(Provider::Claude), vec![2]);
        assert_eq!(roster.slots_of(Provider::Codex), vec![1]);
    }

    #[test]
    fn claude_display_tag_is_org_or_personal() {
        let mut record = AccountRecord::new("a@b.c");
        record.provider = Provider::Claude;
        record.plan_type = Some("pro".into());
        assert_eq!(record.display_tag(), "personal", "no plan labels for Claude");
        record.organization_name = "Acme".into();
        assert_eq!(record.display_tag(), "Acme");
    }
```

Add `use crate::provider::Provider;` to the `tests` module imports (`use super::*;` already brings it in once the struct uses it).

In `src/store/roster.rs` tests add:

```rust
    #[test]
    fn alias_cannot_be_a_provider_selector() {
        for word in ["codex", "Claude"] {
            let err = normalize_alias(word).unwrap_err();
            assert_eq!(err.type_name(), "ValidationError");
            assert_eq!(
                err.to_string(),
                format!(
                    "alias '{}' is reserved for the provider selector",
                    word.to_lowercase()
                )
            );
        }
    }

    #[test]
    fn move_and_swap_renumber_every_provider_marker() {
        let mut roster = roster_with(&[(1, "a@x.com", ""), (2, "b@x.com", "")]);
        roster.record_mut(2).unwrap().provider = Provider::Claude;
        roster.set_active_for(Provider::Codex, Some(1));
        roster.set_active_for(Provider::Claude, Some(2));
        roster.swap_slots(1, 2).unwrap();
        assert_eq!(roster.active_for(Provider::Codex), Some(2));
        assert_eq!(roster.active_for(Provider::Claude), Some(1));
        assert_eq!(roster.active_account_number, Some(2));
        assert_eq!(roster.move_slot(1, 7).unwrap(), MoveOutcome::Relocated);
        assert_eq!(roster.active_for(Provider::Claude), Some(7));
    }
```

and `use crate::provider::Provider;` in that tests module.

- [ ] **Step 6: Run the tests to see them fail**

Run: `cargo test --lib model:: store::roster::`
Expected: compile errors (`no field provider`, `no method set_active_for`, `find_slot` takes 1 argument).

- [ ] **Step 7: Add `provider` to the record and the active map to the roster**

In `src/model.rs`:

```rust
use crate::provider::Provider;
```

In `AccountRecord`, as the **first** field (so it is the first key written):

```rust
pub struct AccountRecord {
    /// Which product's login this is; absent in v0.1 rosters, which were all Codex.
    #[serde(default)]
    pub provider: Provider,
    pub email: String,
```

In `AccountRecord::new` add `provider: Provider::Codex,` first. Replace `display_tag`:

```rust
    /// The bracketed tag shown after the email. Codex: workspace name, else
    /// the plan label, else `personal`. Claude: organization name, else
    /// `personal` (cswap's rule; no plan labels).
    pub fn display_tag(&self) -> String {
        if !self.organization_name.is_empty() {
            return self.organization_name.clone();
        }
        if self.provider == Provider::Claude {
            return "personal".to_string();
        }
        plan_label(self.plan_type.as_deref()).unwrap_or_else(|| "personal".to_string())
    }
```

In `Roster` add after `active_account_number`:

```rust
    /// The active slot per provider (`{"codex": 2, "claude": 5}`); the Codex
    /// entry mirrors `activeAccountNumber`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub active_by_provider: BTreeMap<String, u32>,
```

and in `Roster::empty` add `active_by_provider: BTreeMap::new(),`. Replace `find_slot` and add the helpers:

```rust
    /// First slot (in sequence order) of `provider` whose record has this identity.
    pub fn find_slot(&self, provider: Provider, identity: &Identity) -> Option<u32> {
        self.sequence.iter().copied().find(|slot| {
            self.record(*slot)
                .is_some_and(|r| r.provider == provider && r.identity() == *identity)
        })
    }

    /// The slots of one provider, in sequence order.
    pub fn slots_of(&self, provider: Provider) -> Vec<u32> {
        self.sequence
            .iter()
            .copied()
            .filter(|slot| self.record(*slot).is_some_and(|r| r.provider == provider))
            .collect()
    }

    pub fn active_for(&self, provider: Provider) -> Option<u32> {
        self.active_by_provider.get(provider.as_str()).copied()
    }

    /// Set one provider's active slot; the Codex entry also drives
    /// `activeAccountNumber`.
    pub fn set_active_for(&mut self, provider: Provider, slot: Option<u32>) {
        match slot {
            Some(slot) => {
                self.active_by_provider
                    .insert(provider.as_str().to_string(), slot);
            }
            None => {
                self.active_by_provider.remove(provider.as_str());
            }
        }
        if provider == Provider::Codex {
            self.active_account_number = slot;
        }
        self.touch();
    }
```

In `src/store/roster.rs` change `set_active` to delegate:

```rust
    pub fn set_active(&mut self, slot: Option<u32>) {
        self.set_active_for(Provider::Codex, slot);
    }
```

(add `use crate::provider::Provider;` at the top) and extend `renumber` so the map follows the swap, after the `active_account_number` match:

```rust
        for value in self.active_by_provider.values_mut() {
            if *value == a {
                *value = b;
            } else if *value == b {
                *value = a;
            }
        }
```

In `normalize_alias`, after the `starts_with('-')` check:

```rust
    if Provider::parse_selector(&alias).is_some() {
        return Err(CswitchError::validation(format!(
            "alias '{alias}' is reserved for the provider selector"
        )));
    }
```

Note the `move_slot` → `Relocated` path calls `renumber(src, target)` already, so the map follows a relocation too.

- [ ] **Step 8: Update the `find_slot` call sites to pass a provider**

Every current caller is Codex-only; change them so the crate compiles again:

- `src/collect.rs:93` → `roster.find_slot(Provider::Codex, &identity)` (add `use crate::provider::Provider;`).
- `src/switcher.rs:415` → `live.identity().and_then(|id| roster.find_slot(Provider::Codex, &id))`; `:565` and `:622` → `roster.find_slot(Provider::Codex, &identity)` (add the import).
- `src/transfer.rs:375` and `:692` → `roster.find_slot(Provider::Codex, &identity)` (add the import; phase 4 generalizes).
- `src/session.rs:108` and `:671` → `roster.find_slot(Provider::Codex, &identity)` (add the import; phase 3 generalizes).

- [ ] **Step 9: Run the whole suite**

Run: `cargo test --all`
Expected: everything passes except assertions that compare a whole record to JSON without `provider`; fix each by adding `"provider": "codex"` (look in `tests/transfer_roundtrip.rs` and `src/transfer.rs` tests: run `grep -n '"uuid"' tests/transfer_roundtrip.rs src/transfer.rs` to find the literal records). Then `cargo clippy --all-targets -- -D warnings` and `cargo fmt`.

- [ ] **Step 10: Commit**

```bash
git add src/provider.rs src/lib.rs src/model.rs src/store/roster.rs src/collect.rs src/switcher.rs src/transfer.rs src/session.rs tests/transfer_roundtrip.rs
git commit -m "feat(model): tag every roster record with its provider and track one active slot per provider"
```

---

### Task 2: `spend` on the usage model, the `$$` row, and the JSON projection

**Files:**
- Modify: `src/model.rs`, `src/jsonout.rs`, `src/cli/list.rs`, `src/tui/data.rs`, `src/tui/widgets.rs`
- Test: unit tests in those files

**Interfaces:**
- Produces: `model::Spend { used: f64, limit: f64, pct: f64, currency: String, resets_at: Option<String> }` with `Spend::amounts() -> String` (`$12.50 / $50.00`); `NormalizedUsage.spend: Option<Spend>`; `tui::data::spend_row(&NormalizedUsage, now) -> Option<DisplayRow>`; JSON `usage.spend`.
- Consumes: `DisplayRow`, `usage_bar`, `usage_lines` as they exist.

- [ ] **Step 1: Write the failing model and JSON tests**

In `src/model.rs` tests:

```rust
    #[test]
    fn spend_serializes_and_counts_as_usage() {
        let usage = NormalizedUsage {
            spend: Some(Spend {
                used: 12.5,
                limit: 50.0,
                pct: 25.0,
                currency: "USD".into(),
                resets_at: None,
            }),
            ..NormalizedUsage::default()
        };
        assert!(!usage.is_empty());
        let json = serde_json::to_value(&usage).unwrap();
        assert_eq!(json["spend"]["used"], 12.5);
        assert!(json["spend"].get("resets_at").is_none());
        let back: NormalizedUsage = serde_json::from_value(json).unwrap();
        assert_eq!(back.spend.as_ref().unwrap().amounts(), "$12.50 / $50.00");
        let legacy: NormalizedUsage =
            serde_json::from_str("{\"five_hour\": {\"pct\": 1.0}}").unwrap();
        assert_eq!(legacy.spend, None);
    }
```

In `src/jsonout.rs` tests, inside `projection_carries_windows_pace_and_credits` append:

```rust
        let mut with_spend = usage();
        with_spend.spend = Some(crate::model::Spend {
            used: 7.29,
            limit: 50.0,
            pct: 14.58,
            currency: "USD".into(),
            resets_at: Some(format_iso(NOW + 86_400)),
        });
        let value = usage_projection(&with_spend, Some(NOW as f64), NOW);
        assert_eq!(
            value["spend"],
            json!({"used": 7.29, "limit": 50.0, "pct": 14.58, "currency": "USD", "resetsAt": format_iso(NOW + 86_400)})
        );
```

In `src/cli/list.rs` tests add:

```rust
    #[test]
    fn spend_row_comes_first_with_amounts() {
        let now = 1_790_000_000;
        let mut e = entry();
        e.last_good = Some(NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: 10.0,
                resets_at: None,
            }),
            spend: Some(crate::model::Spend {
                used: 12.5,
                limit: 50.0,
                pct: 25.0,
                currency: "USD".into(),
                resets_at: None,
            }),
            ..NormalizedUsage::default()
        });
        let lines = texts(&usage_lines(&e, now, "  "));
        assert_eq!(lines, ["  ├ $$:  25%  $12.50 / $50.00", "  └ 5h:  10%"]);
    }
```

In `src/tui/data.rs` tests add:

```rust
    #[test]
    fn spend_row_text() {
        let now = 1_790_000_000;
        let usage = NormalizedUsage {
            spend: Some(crate::model::Spend {
                used: 12.5,
                limit: 50.0,
                pct: 25.0,
                currency: "USD".into(),
                resets_at: Some(format_iso(now + 7980)),
            }),
            ..NormalizedUsage::default()
        };
        let row = spend_row(&usage, now).unwrap();
        assert_eq!(row.label, "$$");
        assert_eq!(row.pct, 25.0);
        assert_eq!(row.suffix, "resets 2h 13m  $12.50 / $50.00");
        assert!(row.suffix_full.starts_with("resets 2h 13m · "));
        assert!(row.suffix_full.ends_with("  $12.50 / $50.00"));
        assert_eq!(spend_row(&NormalizedUsage::default(), now), None);
    }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib model::tests::spend jsonout:: cli::list:: tui::data::`
Expected: compile errors about `Spend` / `spend`.

- [ ] **Step 3: Implement `Spend` and thread it through**

`src/model.rs`, next to `Credits`:

```rust
/// Claude's extra-usage spend window (`$$`): dollars, never cents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    pub used: f64,
    pub limit: f64,
    pub pct: f64,
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

fn default_currency() -> String {
    "USD".to_string()
}

impl Spend {
    /// `$12.50 / $50.00`.
    pub fn amounts(&self) -> String {
        format!("${:.2} / ${:.2}", self.used, self.limit)
    }
}
```

In `NormalizedUsage` add after `credits`:

```rust
    /// Claude extra-usage spend; absent for Codex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<Spend>,
```

and in `is_empty` add `&& self.spend.is_none()`.

`src/jsonout.rs` `usage_projection`, after the `credits` block:

```rust
    if let Some(spend) = &usage.spend {
        let mut value = Map::new();
        value.insert("used".into(), json!(spend.used));
        value.insert("limit".into(), json!(spend.limit));
        value.insert("pct".into(), json!(spend.pct));
        value.insert("currency".into(), json!(spend.currency));
        if let Some(resets_at) = &spend.resets_at {
            value.insert("resetsAt".into(), json!(resets_at));
        }
        out.insert("spend".into(), Value::Object(value));
    }
```

`src/cli/list.rs` `usage_lines`: build the `$$` text before the window rows. Replace the `let rows = usage_rows(...)` block's `texts` construction with:

```rust
    let rows = usage_rows(usage, entry.fetched_at);
    let width = rows
        .iter()
        .map(|r| r.label.len() + 1)
        .chain(usage.spend.iter().map(|_| 3))
        .max()
        .unwrap_or(0);
    let mut texts: Vec<String> = Vec::new();
    if let Some(spend) = &usage.spend {
        let mut body = format!("{:>3.0}%", spend.pct);
        if let Some(reset) = spend.resets_at.as_deref().and_then(crate::usage_math::parse_reset) {
            let (countdown, clock) = countdown_and_clock(reset, now);
            body.push_str(&format!("   resets {clock:<12}  in {countdown}"));
        }
        body.push_str(&format!("  {}", spend.amounts()));
        texts.push(format!("{:<width$} {body}", "$$:"));
    }
    texts.extend(rows.iter().map(|row| {
```

(keep the existing closure body for the window rows; it now feeds `extend` instead of `collect`). The `credits` and age handling after it stay as they are.

`src/tui/data.rs`, after `display_rows`:

```rust
/// The `$$` row of a Claude card: pct plus `resets …` and the amounts.
pub fn spend_row(usage: &NormalizedUsage, now: i64) -> Option<DisplayRow> {
    let spend = usage.spend.as_ref()?;
    let resets_at = spend.resets_at.as_deref().and_then(parse_reset);
    let reset = reset_text(resets_at, now).unwrap_or_default();
    let reset_full = match reset_clock(resets_at, now) {
        Some(clock) if !reset.is_empty() => format!("{reset} · {clock}"),
        _ => reset.clone(),
    };
    Some(DisplayRow {
        label: "$$".to_string(),
        pct: spend.pct,
        suffix: join(&[reset, spend.amounts()]),
        suffix_full: join(&[reset_full, spend.amounts()]),
        maxed_pool: false,
        ahead: false,
        resets_at,
    })
}
```

with `use crate::usage_math::{binding_pct, parse_reset, usage_rows};` at the top.

`src/tui/widgets.rs` `account_card`: replace

```rust
    let rows = usage
        .last_good
        .as_ref()
        .map(|last| display_rows(last, usage.fetched_at, now as i64))
        .unwrap_or_default();
```

with

```rust
    let rows: Vec<DisplayRow> = usage
        .last_good
        .as_ref()
        .map(|last| {
            spend_row(last, now as i64)
                .into_iter()
                .chain(display_rows(last, usage.fetched_at, now as i64))
                .collect()
        })
        .unwrap_or_default();
```

and import `DisplayRow` and `spend_row` from `super::data`. `mini_line` keeps using `display_rows` alone (the `$$` row never appears on a mini).

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib model:: jsonout:: cli::list:: tui::`
Expected: all pass. Then `cargo test --all` (the `e2e_usage` JSON assertions are untouched because Codex rows carry no `spend`).

- [ ] **Step 5: Commit**

```bash
git add src/model.rs src/jsonout.rs src/cli/list.rs src/tui/data.rs src/tui/widgets.rs
git commit -m "feat(usage): carry Claude's extra-usage spend as a \$\$ row in list, cards and JSON"
```

---

### Task 3: JSON schema 2, the `provider` field, per-provider `active`, and the Keychain sentinel

**Files:**
- Modify: `src/model.rs`, `src/store/usage_store.rs`, `src/jsonout.rs`, `src/cli/list.rs`, `src/switcher.rs` (the `SwitchOutcome` literals only), `src/tui/app.rs` (test literals), `tests/cli_list.rs`, `tests/cli_switch.rs`, `tests/cli_accounts.rs`, `tests/config_cmd.rs`
- Test: `src/jsonout.rs`, `src/store/usage_store.rs`

**Interfaces:**
- Produces: `model::SCHEMA_VERSION = 2`; `model::ActiveSlots { codex: Option<u32>, claude: Option<u32> }` (`Default`, `Serialize` as `{"codex":…,"claude":…}`, `get(Provider)`, `set(Provider, Option<u32>)`, `iter()`); `SwitchOutcome.provider: Provider`; `UsageSentinel::KeychainUnavailable`; `jsonout::list_payload(&ActiveSlots, rows, warnings)`; `jsonout::status_payload(&[ProviderStatus], total, now)` where `switcher::ProviderStatus { provider, current: CurrentAccount, row: Option<AccountRow> }`; `switcher::StatusSnapshot { providers: Vec<ProviderStatus>, total: usize }`.
- Consumes: `AccountRow`, `CurrentAccount`.

- [ ] **Step 1: Write the failing tests**

`src/store/usage_store.rs` tests (add to the existing `tests` module):

```rust
    #[test]
    fn keychain_sentinel_wording() {
        assert_eq!(
            UsageSentinel::KeychainUnavailable.label(),
            "keychain unavailable — locked or in use; try again"
        );
        assert_eq!(
            UsageSentinel::KeychainUnavailable.usage_status(),
            "keychain_unavailable"
        );
    }
```

`src/jsonout.rs`: update `payload_shapes` so the literals read `"schemaVersion": 2`, and replace the three `list_payload` / `status_payload` calls:

```rust
        let list = list_payload(&ActiveSlots::default(), vec![], &[]);
        assert_eq!(
            list,
            json!({"schemaVersion": 2, "activeAccountNumber": null, "active": {"codex": null, "claude": null}, "accounts": []})
        );
        let mut actives = ActiveSlots::default();
        actives.set(Provider::Codex, Some(1));
        actives.set(Provider::Claude, Some(5));
        let list = list_payload(&actives, vec![json!({"number": 1})], &["w".to_string()]);
        assert_eq!(list["activeAccountNumber"], 1);
        assert_eq!(list["active"], json!({"codex": 1, "claude": 5}));
        assert_eq!(list["warnings"], json!(["w"]));

        let none = [
            ProviderStatus { provider: Provider::Codex, current: CurrentAccount::NoLogin, row: None },
            ProviderStatus { provider: Provider::Claude, current: CurrentAccount::NoLogin, row: None },
        ];
        assert_eq!(
            status_payload(&none, 0, NOW),
            json!({"schemaVersion": 2, "active": {"codex": null, "claude": null}, "totalManagedAccounts": 0})
        );
        let unmanaged = [
            ProviderStatus {
                provider: Provider::Codex,
                current: CurrentAccount::Unmanaged { email: "u@x.com".into() },
                row: None,
            },
            ProviderStatus { provider: Provider::Claude, current: CurrentAccount::NoLogin, row: None },
        ];
        assert_eq!(
            status_payload(&unmanaged, 2, NOW)["active"],
            json!({"codex": {"email": "u@x.com", "managed": false}, "claude": null})
        );
        let mut record = AccountRecord::new("a@x.com");
        record.provider = Provider::Claude;
        let managed = [
            ProviderStatus { provider: Provider::Codex, current: CurrentAccount::NoLogin, row: None },
            ProviderStatus {
                provider: Provider::Claude,
                current: CurrentAccount::Managed { slot: 5, email: "a@x.com".into(), api_key: false },
                row: Some(AccountRow { slot: 5, record, usage: entry(None, None), is_active: true }),
            },
        ];
        let value = status_payload(&managed, 2, NOW);
        assert!(value["active"]["codex"].is_null());
        assert_eq!(value["active"]["claude"]["managed"], true);
        assert_eq!(value["active"]["claude"]["provider"], "claude");
        assert_eq!(value["active"]["claude"]["number"], 5);
        assert!(value["active"]["claude"].get("active").is_none());
        assert_eq!(value["totalManagedAccounts"], 2);
```

and in the `switch_payload` part set `provider: Provider::Claude` on the outcome literal and assert:

```rust
        assert_eq!(value["schemaVersion"], 2);
        assert_eq!(value["provider"], "claude");
        assert_eq!(value["from"]["provider"], "claude");
        assert_eq!(value["to"]["provider"], "claude");
```

In `row_status_and_additive_fields` assert `assert_eq!(row["provider"], "codex");` after the `number` check. Fix the error-envelope literal to `"schemaVersion": 2`. Imports for the test module: `use crate::model::{ActiveSlots, AccountRef, Credits, ScopedWindow}; use crate::provider::Provider; use crate::switcher::{AccountRow, ProviderStatus};`.

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib jsonout:: store::usage_store::tests::keychain`
Expected: compile errors.

- [ ] **Step 3: Implement**

`src/model.rs`: set `pub const SCHEMA_VERSION: u32 = 2;` and add

```rust
/// The active slot of each provider, as `list.active` / `status.active` report it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ActiveSlots {
    pub codex: Option<u32>,
    pub claude: Option<u32>,
}

impl ActiveSlots {
    pub fn get(&self, provider: Provider) -> Option<u32> {
        match provider {
            Provider::Codex => self.codex,
            Provider::Claude => self.claude,
        }
    }

    pub fn set(&mut self, provider: Provider, slot: Option<u32>) {
        match provider {
            Provider::Codex => self.codex = slot,
            Provider::Claude => self.claude = slot,
        }
    }

    /// `(provider, slot)` for every provider, in `Provider::ALL` order.
    pub fn iter(&self) -> impl Iterator<Item = (Provider, Option<u32>)> + '_ {
        Provider::ALL.into_iter().map(|p| (p, self.get(p)))
    }

    /// The lowest active slot of any provider (the TUI cursor's starting point).
    pub fn lowest(&self) -> Option<u32> {
        self.codex.into_iter().chain(self.claude).min()
    }
}
```

Add to `SwitchOutcome`, after `switched`:

```rust
    /// The provider the switch acted on.
    #[serde(default)]
    pub provider: Provider,
```

Then `cargo build` lists every `SwitchOutcome { … }` literal: `src/switcher.rs` (the `noop` closures in `switch`, `rotate`, `best`, and the two literals in `perform_switch`), `src/tui/app.rs` tests (two), `src/autoswitch.rs` if any. Add `provider: Provider::Codex,` to each; Tasks 13–14 replace the Codex literal in `switcher.rs` with the record's provider.

`src/store/usage_store.rs`: add the `KeychainUnavailable` variant to `UsageSentinel`, with label `"keychain unavailable — locked or in use; try again"` and status `"keychain_unavailable"`. Any exhaustive `match` elsewhere (grep `UsageSentinel::` in `src/`) gets the new arm: `src/cli/list.rs` and `src/tui/data.rs` match through `label()`, so only `usage_store.rs` changes.

`src/switcher.rs`: replace `StatusSnapshot`:

```rust
/// One provider's live login and (when managed) its row.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderStatus {
    pub provider: Provider,
    pub current: CurrentAccount,
    pub row: Option<AccountRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusSnapshot {
    /// One entry per provider, in `Provider::ALL` order.
    pub providers: Vec<ProviderStatus>,
    pub total: usize,
}
```

and make `Switcher::status()` build it with the Codex entry as today plus a Claude entry of `CurrentAccount::NoLogin` / `None` (Task 14 fills it in):

```rust
        Ok(StatusSnapshot {
            providers: vec![
                ProviderStatus { provider: Provider::Codex, current, row },
                ProviderStatus { provider: Provider::Claude, current: CurrentAccount::NoLogin, row: None },
            ],
            total,
        })
```

Also change `ListSnapshot.active: Option<u32>` to `pub actives: ActiveSlots` and set it in `list_snapshot` with `let mut actives = ActiveSlots::default(); actives.set(Provider::Codex, active);` (Task 14 adds Claude). Update `AccountsSnapshot::from_list` (`src/tui/snapshot.rs`) to `active_number: list.actives.lowest()` and its test literal `ListSnapshot { actives: …, … }`.

`src/jsonout.rs`:

```rust
use crate::model::{AccountRecord, ActiveSlots, CurrentAccount, NormalizedUsage, SCHEMA_VERSION, SwitchOutcome, WindowUsage, format_iso};
use crate::switcher::ProviderStatus;
```

In `row_fields` insert `row.insert("provider".into(), json!(record.provider.as_str()));` right after `number`. Replace `list_payload` and `status_payload`:

```rust
pub fn list_payload(actives: &ActiveSlots, rows: Vec<Value>, warnings: &[String]) -> Value {
    let mut payload = json!({
        "schemaVersion": SCHEMA_VERSION,
        "activeAccountNumber": actives.codex,
        "active": actives,
        "accounts": rows,
    });
    if !warnings.is_empty() {
        payload["warnings"] = json!(warnings);
    }
    payload
}

/// `status --json`: one entry per provider under `active`: null, `{email, managed: false}`,
/// or the managed row (with `managed: true`).
pub fn status_payload(statuses: &[ProviderStatus], total: usize, now: i64) -> Value {
    let mut active = Map::new();
    for status in statuses {
        let value = match (&status.current, &status.row) {
            (CurrentAccount::NoLogin, _) => Value::Null,
            (CurrentAccount::Managed { slot, .. }, Some(row)) => {
                let mut fields = row_fields(*slot, &row.record, &row.usage, now);
                fields.insert("managed".into(), json!(true));
                Value::Object(fields)
            }
            (current, _) => json!({"email": current.email().unwrap_or(""), "managed": false}),
        };
        active.insert(status.provider.as_str().to_string(), value);
    }
    json!({
        "schemaVersion": SCHEMA_VERSION,
        "active": Value::Object(active),
        "totalManagedAccounts": total,
    })
}
```

In `switch_payload`, after the version line:

```rust
    let provider = json!(outcome.provider.as_str());
    for key in ["from", "to"] {
        if let Some(reference) = payload.get_mut(key).filter(|v| v.is_object()) {
            reference["provider"] = provider.clone();
        }
    }
```

`src/cli/list.rs`: `list_cmd` passes `&snapshot.actives`; `first_run` passes `&ActiveSlots::default()`; `status_cmd` becomes:

```rust
    if json {
        print!(
            "{}",
            jsonout::render_document(&jsonout::status_payload(
                &snapshot.providers,
                snapshot.total,
                now_unix()
            ))
        );
        return Ok(0);
    }
```

and `status_lines` iterates `snapshot.providers`, rendering the existing block for each provider whose `current` is not `NoLogin` or whose row exists, prefixed with the provider title when more than one block is printed:

```rust
pub fn status_lines(snapshot: &StatusSnapshot) -> Vec<Line> {
    let blocks: Vec<&ProviderStatus> = snapshot
        .providers
        .iter()
        .filter(|s| !matches!(s.current, CurrentAccount::NoLogin))
        .collect();
    if blocks.is_empty() {
        return vec![
            Line::new()
                .push(Style::Bold, "Status:")
                .push(Style::Dimmed, " No active Codex or Claude account"),
        ];
    }
    let labelled = blocks.len() > 1;
    let mut lines = Vec::new();
    for (i, status) in blocks.iter().enumerate() {
        if i > 0 {
            lines.push(Line::new());
        }
        let header = if labelled {
            Line::new().push(Style::Bold, format!("{} status:", status.provider.title()))
        } else {
            Line::new().push(Style::Bold, "Status:")
        };
        match (&status.current, &status.row) {
            (CurrentAccount::Managed { slot, .. }, Some(row)) => {
                lines.push(
                    header
                        .push(Style::Plain, " ")
                        .push(Style::Accent, format!("Account-{slot}"))
                        .push(Style::Plain, format!(" ({} ", row.record.email))
                        .push(Style::Muted, format!("[{}]", row.record.display_tag()))
                        .push(Style::Plain, ")"),
                );
                lines.push(Line::plain("  ").push(
                    Style::Dimmed,
                    format!("Total managed accounts: {}", snapshot.total),
                ));
                lines.extend(usage_lines(&row.usage, now_unix(), "  "));
            }
            (current, _) => {
                let email = current
                    .email()
                    .filter(|e| !e.is_empty())
                    .unwrap_or("API key");
                lines.push(header.push(Style::Dimmed, format!(" {email} (not managed)")));
            }
        }
    }
    lines
}
```

Update the `status_lines_variants` unit test to build `StatusSnapshot { providers: vec![ProviderStatus { … }, ProviderStatus { provider: Provider::Claude, current: CurrentAccount::NoLogin, row: None }], total }` for the three cases; the expected texts stay the same except the no-login case, which now reads `Status: No active Codex or Claude account`.

Integration test assertions to update (the values only): `tests/cli_list.rs:15` → `json!({"schemaVersion": 2, "activeAccountNumber": null, "active": {"codex": null, "claude": null}, "accounts": []})`; `:107` → `2`; `:141` → `json!({"schemaVersion": 2, "active": {"codex": null, "claude": null}, "totalManagedAccounts": 0})`; `:149` → `json!({"schemaVersion": 2, "active": {"codex": {"email": "carol@example.com", "managed": false}, "claude": null}, "totalManagedAccounts": <the test's total>})`; `:161–164` → index through `payload["active"]["codex"]`; `tests/e2e_usage.rs:139–142` → `payload["active"]["codex"][…]`; `tests/cli_switch.rs:96` → `2`; `tests/cli_accounts.rs:439` → `"schemaVersion": 2`; `tests/config_cmd.rs:158,170,192` → `2` (line 265, the settings file, stays `1`). Also add `assert_eq!(payload["accounts"][0]["provider"], "codex");` in `tests/cli_list.rs` next to line 107. Any `Status: No active Codex account` assertion in `tests/cli_list.rs` becomes `Status: No active Codex or Claude account`.

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add -A src tests
git commit -m "feat(json): schemaVersion 2 with a provider on every row and one active entry per provider"
```

---

### Task 4: Claude paths on `Paths` and the Keychain switch

Deviation from spec §14: the Claude paths live on the existing `Paths` (one path authority) instead of a separate `src/claude/paths.rs`; `src/claude/` starts in Task 5.

**Files:**
- Modify: `src/paths.rs`, `src/store/mod.rs` (`temp_store`)
- Test: `src/paths.rs`

**Interfaces:**
- Produces: `Paths.claude_home`, `Paths.claude_config_base`, `Paths.keychain_enabled: bool`; `Paths::from_values(cswitch_home, codex_home, claude_config_dir: Option<PathBuf>, user_home)`; `claude_credentials_file()`, `claude_global_config_file()`, `claude_refresh_lock_dir()`, `claude_legacy_lock_dir()`, `claude_config_lock_dir()`, `claude_backups_dir()`.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing tests**

Replace the `defaults_and_overrides` test in `src/paths.rs` and add two more:

```rust
    #[test]
    fn defaults_and_overrides() {
        let home = Path::new("/home/u");
        let paths = Paths::from_values(None, None, None, home).unwrap();
        assert_eq!(paths.backup_root, home.join(".cswitch"));
        assert_eq!(paths.codex_home, home.join(".codex"));
        assert_eq!(paths.live_auth_file(), home.join(".codex/auth.json"));
        assert_eq!(paths.claude_home, home.join(".claude"));
        assert_eq!(paths.claude_credentials_file(), home.join(".claude/.credentials.json"));
        assert_eq!(paths.claude_global_config_file(), home.join(".claude.json"));
        assert_eq!(paths.claude_refresh_lock_dir(), home.join(".claude/.oauth_refresh.lock"));
        assert_eq!(paths.claude_legacy_lock_dir(), home.join(".claude.lock"));
        assert_eq!(paths.claude_config_lock_dir(), home.join(".claude.json.lock"));
        assert_eq!(paths.claude_backups_dir(), home.join(".cswitch/backups/claude"));
        assert!(!paths.keychain_enabled, "from_values never touches the Keychain");

        let paths = Paths::from_values(
            Some(PathBuf::from("")),
            Some(PathBuf::from("/tmp/codex")),
            Some(PathBuf::from("/tmp/cc")),
            home,
        )
        .unwrap();
        assert_eq!(paths.backup_root, home.join(".cswitch"));
        assert_eq!(paths.codex_home, PathBuf::from("/tmp/codex"));
        assert_eq!(paths.claude_home, PathBuf::from("/tmp/cc"));
        assert_eq!(
            paths.claude_global_config_file(),
            PathBuf::from("/tmp/cc/.claude.json"),
            "CLAUDE_CONFIG_DIR moves .claude.json inside it"
        );
        assert_eq!(paths.claude_config_lock_dir(), PathBuf::from("/tmp/cc/.claude.json.lock"));

        let err = Paths::from_values(None, Some(PathBuf::from("/tmp/../x")), None, home).unwrap_err();
        assert!(err.to_string().contains(".."));
        let err = Paths::from_values(None, None, Some(PathBuf::from("/tmp/../c")), home).unwrap_err();
        assert!(err.to_string().contains("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn legacy_claude_config_wins_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_values(None, None, Some(dir.path().to_path_buf()), dir.path()).unwrap();
        assert_eq!(paths.claude_global_config_file(), dir.path().join(".claude.json"));
        std::fs::write(dir.path().join(".config.json"), "{}").unwrap();
        assert_eq!(paths.claude_global_config_file(), dir.path().join(".config.json"));
    }

    #[test]
    fn keychain_switch_reads_the_environment() {
        assert!(!keychain_enabled_from(Some("off")));
        assert!(!keychain_enabled_from(Some("0")));
        assert_eq!(keychain_enabled_from(None), cfg!(target_os = "macos"));
        assert_eq!(keychain_enabled_from(Some("on")), cfg!(target_os = "macos"));
    }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib paths::`
Expected: compile errors (wrong arity, missing fields).

- [ ] **Step 3: Implement**

In `src/paths.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `$CSWITCH_HOME`, default `~/.cswitch`.
    pub backup_root: PathBuf,
    /// `$CODEX_HOME`, default `~/.codex`.
    pub codex_home: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`, default `~/.claude`.
    pub claude_home: PathBuf,
    /// Where `.claude.json` lives: `$CLAUDE_CONFIG_DIR` when set, else the user home.
    pub claude_config_base: PathBuf,
    /// macOS with `CSWITCH_KEYCHAIN` not `off`; the file backend otherwise.
    pub keychain_enabled: bool,
}

/// `CSWITCH_KEYCHAIN=off|0|false` disables the Keychain; it is never used off macOS.
pub fn keychain_enabled_from(value: Option<&str>) -> bool {
    let off = value.is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "false"));
    cfg!(target_os = "macos") && !off
}
```

`from_env` gains the two environment reads:

```rust
    pub fn from_env() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| CswitchError::config("could not determine home directory"))?;
        let mut paths = Self::from_values(
            std::env::var_os("CSWITCH_HOME").map(PathBuf::from),
            std::env::var_os("CODEX_HOME").map(PathBuf::from),
            std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            &home,
        )?;
        paths.keychain_enabled = keychain_enabled_from(std::env::var("CSWITCH_KEYCHAIN").ok().as_deref());
        Ok(paths)
    }
```

`from_values` takes `claude_config_dir: Option<PathBuf>` as the third parameter; after the `codex_home` block:

```rust
        let claude_dir = claude_config_dir.filter(|p| !p.as_os_str().is_empty());
        if let Some(path) = &claude_dir
            && path.components().any(|c| matches!(c, Component::ParentDir))
        {
            return Err(CswitchError::config(format!(
                "CLAUDE_CONFIG_DIR contains '..' component which is not allowed: {}",
                path.display()
            )));
        }
        let (claude_home, claude_config_base) = match claude_dir {
            Some(path) => (path.clone(), path),
            None => (user_home.join(".claude"), user_home.to_path_buf()),
        };
        Ok(Self {
            backup_root,
            codex_home,
            claude_home,
            claude_config_base,
            keychain_enabled: false,
        })
```

Accessors, after `codex_config_file`:

```rust
    /// `<claude home>/.credentials.json`.
    pub fn claude_credentials_file(&self) -> PathBuf {
        self.claude_home.join(".credentials.json")
    }

    /// `<claude home>/.config.json` when it exists (legacy), else
    /// `<base>/.claude.json` — the rule Claude Code itself applies.
    pub fn claude_global_config_file(&self) -> PathBuf {
        let legacy = self.claude_home.join(".config.json");
        if legacy.exists() {
            legacy
        } else {
            self.claude_config_base.join(".claude.json")
        }
    }

    /// Claude Code's primary credential-refresh lock directory.
    pub fn claude_refresh_lock_dir(&self) -> PathBuf {
        self.claude_home.join(".oauth_refresh.lock")
    }

    /// `~/.claude.lock`: the legacy credential lock, a sibling of the config home.
    pub fn claude_legacy_lock_dir(&self) -> PathBuf {
        with_suffix(&self.claude_home, ".lock")
    }

    /// `~/.claude.json.lock`: the global-config lock.
    pub fn claude_config_lock_dir(&self) -> PathBuf {
        with_suffix(&self.claude_global_config_file(), ".lock")
    }

    /// Where the outgoing Claude login is backed up before a switch.
    pub fn claude_backups_dir(&self) -> PathBuf {
        self.backup_root.join("backups").join("claude")
    }
```

and the helper at module level:

```rust
/// `/a/b` + `.lock` → `/a/b.lock` (no extension games with dotfiles).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}
```

`src/store/mod.rs` `temp_store`: pass `Some(dir.path().join("claude"))` as the third argument so every unit test gets a private Claude home with the file backend. Update the other `from_values` callers found by `grep -rn "from_values(" src tests examples` (the `credential_store_gate` and `slug_and_session_dir` tests in `paths.rs`).

- [ ] **Step 4: Run the tests**

Run: `cargo test --all`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add src/paths.rs src/store/mod.rs
git commit -m "feat(paths): resolve the Claude config home, global config, lock dirs and backups; CSWITCH_KEYCHAIN switch"
```

---

### Task 5: `claude::keychain` — the `security` wrapper

**Files:**
- Create: `src/claude/mod.rs`, `src/claude/keychain.rs`
- Modify: `src/lib.rs`
- Test: `src/claude/keychain.rs`

**Interfaces:**
- Produces: `claude::keychain::{LIVE_SERVICE, MANAGED_SERVICE, SecurityCli, CliOutput, SystemSecurity, Keychain, KeychainError, account_name}`.
- Consumes: `hex`, `libc` (both in `Cargo.toml`).

- [ ] **Step 1: Create the module root and the failing tests**

`src/claude/mod.rs`:

```rust
//! Everything that touches Claude Code: config files, the macOS Keychain,
//! Claude Code's lock protocol, token refresh and the usage API (spec
//! `docs/specs/2026-10-07-cswitch-claude-provider-design.md` §4).

pub mod credentials;
pub mod keychain;
pub mod live;
pub mod locks;
pub mod oauth;
pub mod usage;
```

(create the five other files as empty `//!` stubs now so the crate compiles: `credentials.rs`, `live.rs`, `locks.rs`, `oauth.rs`, `usage.rs` each containing one doc line.) Add `pub mod claude;` to `src/lib.rs` after `pub mod codex;`.

`src/claude/keychain.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Records every call and answers from a queue.
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<(Vec<String>, Option<String>)>>,
        replies: RefCell<VecDeque<io::Result<CliOutput>>>,
    }

    impl Fake {
        fn reply(&self, status: i32, stdout: &str) {
            self.replies.borrow_mut().push_back(Ok(CliOutput {
                status,
                stdout: stdout.to_string(),
            }));
        }
        fn fail(&self) {
            self.replies
                .borrow_mut()
                .push_back(Err(io::Error::new(io::ErrorKind::TimedOut, "stuck")));
        }
        fn calls(&self) -> Vec<(Vec<String>, Option<String>)> {
            self.calls.borrow().clone()
        }
    }

    impl SecurityCli for Fake {
        fn run(&self, args: &[String], stdin_line: Option<&str>) -> io::Result<CliOutput> {
            self.calls
                .borrow_mut()
                .push((args.to_vec(), stdin_line.map(str::to_string)));
            self.replies.borrow_mut().pop_front().expect("scripted reply")
        }
    }

    #[test]
    fn get_password_distinguishes_missing_from_broken() {
        let fake = Fake::default();
        let keychain = Keychain::new(&fake);
        fake.reply(0, "{\"claudeAiOauth\":{}}\n");
        assert_eq!(
            keychain.get_password(LIVE_SERVICE, "me").unwrap(),
            Some("{\"claudeAiOauth\":{}}".to_string())
        );
        fake.reply(44, "");
        assert_eq!(keychain.get_password(LIVE_SERVICE, "me").unwrap(), None);
        fake.reply(36, "");
        assert!(matches!(
            keychain.get_password(LIVE_SERVICE, "me"),
            Err(KeychainError::Unavailable(_))
        ));
        fake.fail();
        assert!(keychain.get_password(LIVE_SERVICE, "me").is_err());
        let calls = fake.calls();
        assert_eq!(
            calls[0].0,
            ["find-generic-password", "-a", "me", "-w", "-s", LIVE_SERVICE]
        );
        assert_eq!(calls[0].1, None);
    }

    #[test]
    fn set_password_goes_through_stdin_as_hex_and_falls_back_to_argv() {
        let fake = Fake::default();
        let keychain = Keychain::new(&fake);
        fake.reply(0, "");
        keychain
            .set_password(LIVE_SERVICE, "me", "{\"a\":\"b\"}")
            .unwrap();
        let (args, line) = fake.calls()[0].clone();
        assert_eq!(args, ["-i"]);
        assert_eq!(
            line.unwrap(),
            format!(
                "add-generic-password -U -a \"me\" -s \"{LIVE_SERVICE}\" -X {}",
                hex::encode("{\"a\":\"b\"}")
            )
        );
        fake.reply(0, "");
        let long = "x".repeat(3000);
        keychain.set_password(MANAGED_SERVICE, "me", &long).unwrap();
        let (args, line) = fake.calls()[1].clone();
        assert_eq!(line, None, "a line over 4032 bytes uses argv");
        assert_eq!(args[0], "add-generic-password");
        assert_eq!(args[7], hex::encode(&long));
        fake.reply(1, "");
        assert!(keychain.set_password(LIVE_SERVICE, "me", "v").is_err());
    }

    #[test]
    fn quoting_escapes_backslashes_and_quotes() {
        assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn delete_and_exists() {
        let fake = Fake::default();
        let keychain = Keychain::new(&fake);
        fake.reply(0, "");
        assert!(keychain.delete_password(LIVE_SERVICE, "me").unwrap());
        fake.reply(44, "");
        assert!(!keychain.delete_password(LIVE_SERVICE, "me").unwrap());
        fake.reply(0, "keychain: ...");
        assert!(keychain.item_exists(MANAGED_SERVICE, "me").unwrap());
        fake.reply(44, "");
        assert!(!keychain.item_exists(MANAGED_SERVICE, "me").unwrap());
        let calls = fake.calls();
        assert_eq!(calls[0].0[0], "delete-generic-password");
        assert_eq!(calls[2].0, ["find-generic-password", "-a", "me", "-s", MANAGED_SERVICE]);
    }

    #[test]
    fn account_name_prefers_user() {
        // $USER is set in every CI shell we run on; the fallbacks are exercised by inspection.
        if let Ok(user) = std::env::var("USER") {
            assert_eq!(account_name(), user);
        }
        assert!(!account_name().is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib claude::keychain::`
Expected: compile errors.

- [ ] **Step 3: Implement the wrapper**

`src/claude/keychain.rs`:

```rust
//! macOS Keychain access through `/usr/bin/security` (spec §4): the same
//! commands Claude Code runs, so creator == reader and nothing prompts.
//! Compiled everywhere; only meaningful on macOS.

use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Claude Code's live OAuth credential.
pub const LIVE_SERVICE: &str = "Claude Code-credentials";
/// Claude Code's managed API key (`/login` with an `sk-ant-api…` key).
pub const MANAGED_SERVICE: &str = "Claude Code";
/// Pinned: a `security` earlier on PATH must never see a credential.
const SECURITY: &str = "/usr/bin/security";
/// `errSecItemNotFound` as surfaced by find/delete-generic-password.
const NOT_FOUND_RC: i32 = 44;
/// A wedged Keychain (locked, headless) must not hang the CLI.
const TIMEOUT: Duration = Duration::from_secs(5);
/// `security -i` reads stdin with a 4096-byte line buffer; keep headroom.
const STDIN_LINE_LIMIT: usize = 4096 - 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOutput {
    pub status: i32,
    pub stdout: String,
}

/// How `security` is run; tests substitute a recorder.
pub trait SecurityCli {
    fn run(&self, args: &[String], stdin_line: Option<&str>) -> io::Result<CliOutput>;
}

/// The real binary, with a bounded wait.
pub struct SystemSecurity;

impl SecurityCli for SystemSecurity {
    fn run(&self, args: &[String], stdin_line: Option<&str>) -> io::Result<CliOutput> {
        let mut command = Command::new(SECURITY);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(if stdin_line.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command.spawn()?;
        if let Some(line) = stdin_line {
            let mut stdin = child.stdin.take().expect("piped stdin");
            stdin.write_all(line.as_bytes())?;
            stdin.write_all(b"\n")?;
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                let mut stdout = String::new();
                if let Some(mut out) = child.stdout.take() {
                    out.read_to_string(&mut stdout)?;
                }
                return Ok(CliOutput {
                    status: status.code().unwrap_or(-1),
                    stdout,
                });
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "security did not answer within 5 s",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeychainError {
    /// Locked, denied, timed out, or any exit other than "not found".
    Unavailable(String),
}

impl fmt::Display for KeychainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(detail) => write!(f, "keychain unavailable: {detail}"),
        }
    }
}

impl std::error::Error for KeychainError {}

/// The account name of the live item, mirroring Claude Code's `getUsername()`:
/// `$USER`, then the OS user name, then a stable fallback.
pub fn account_name() -> String {
    if let Some(user) = std::env::var_os("USER").filter(|u| !u.is_empty()) {
        return user.to_string_lossy().into_owned();
    }
    #[cfg(unix)]
    if let Some(name) = unix_user_name() {
        return name;
    }
    "claude-code-user".to_string()
}

#[cfg(unix)]
fn unix_user_name() -> Option<String> {
    // SAFETY: getpwuid returns a pointer to static storage or null; the
    // C string is read immediately and never retained.
    unsafe {
        let entry = libc::getpwuid(libc::geteuid());
        if entry.is_null() {
            return None;
        }
        let name = std::ffi::CStr::from_ptr((*entry).pw_name);
        Some(name.to_string_lossy().into_owned())
    }
}

pub struct Keychain<'a> {
    cli: &'a dyn SecurityCli,
}

fn argv<const N: usize>(parts: [&str; N]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Double-quote a `security -i` argument.
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

impl<'a> Keychain<'a> {
    pub fn new(cli: &'a dyn SecurityCli) -> Self {
        Self { cli }
    }

    fn unavailable(what: &str, out: &CliOutput) -> KeychainError {
        KeychainError::Unavailable(format!("{what} exited {}", out.status))
    }

    /// `None` when the item does not exist.
    pub fn get_password(&self, service: &str, account: &str) -> Result<Option<String>, KeychainError> {
        let args = argv(["find-generic-password", "-a", account, "-w", "-s", service]);
        match self.cli.run(&args, None) {
            Ok(out) if out.status == 0 => Ok(Some(out.stdout.trim_end_matches(['\n', '\r']).to_string())),
            Ok(out) if out.status == NOT_FOUND_RC => Ok(None),
            Ok(out) => Err(Self::unavailable("find-generic-password", &out)),
            Err(err) => Err(KeychainError::Unavailable(err.to_string())),
        }
    }

    pub fn item_exists(&self, service: &str, account: &str) -> Result<bool, KeychainError> {
        let args = argv(["find-generic-password", "-a", account, "-s", service]);
        match self.cli.run(&args, None) {
            Ok(out) if out.status == 0 => Ok(true),
            Ok(out) if out.status == NOT_FOUND_RC => Ok(false),
            Ok(out) => Err(Self::unavailable("find-generic-password", &out)),
            Err(err) => Err(KeychainError::Unavailable(err.to_string())),
        }
    }

    /// Create or update (`-U`) the item. The value is hex-encoded (`-X`) and the
    /// whole command goes through `security -i` on stdin so the secret never
    /// appears in argv; a command over the stdin line limit uses argv instead.
    pub fn set_password(&self, service: &str, account: &str, value: &str) -> Result<(), KeychainError> {
        let hex_value = hex::encode(value.as_bytes());
        let line = format!(
            "add-generic-password -U -a {} -s {} -X {hex_value}",
            quote(account),
            quote(service)
        );
        let result = if line.len() <= STDIN_LINE_LIMIT {
            self.cli.run(&argv(["-i"]), Some(&line))
        } else {
            self.cli.run(
                &argv(["add-generic-password", "-U", "-a", account, "-s", service, "-X", &hex_value]),
                None,
            )
        };
        match result {
            Ok(out) if out.status == 0 => Ok(()),
            Ok(out) => Err(Self::unavailable("add-generic-password", &out)),
            Err(err) => Err(KeychainError::Unavailable(err.to_string())),
        }
    }

    /// `true` when an item was removed, `false` when there was none.
    pub fn delete_password(&self, service: &str, account: &str) -> Result<bool, KeychainError> {
        let args = argv(["delete-generic-password", "-a", account, "-s", service]);
        match self.cli.run(&args, None) {
            Ok(out) if out.status == 0 => Ok(true),
            Ok(out) if out.status == NOT_FOUND_RC => Ok(false),
            Ok(out) => Err(Self::unavailable("delete-generic-password", &out)),
            Err(err) => Err(KeychainError::Unavailable(err.to_string())),
        }
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib claude::keychain:: && cargo clippy --all-targets -- -D warnings`
Expected: 5 passed, clippy clean (the stub modules are empty).

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/claude
git commit -m "feat(claude): wrap /usr/bin/security for Claude Code's Keychain items"
```

---

### Task 6: `claude::credentials` — the credential, `oauthAccount` and slot-file model

**Files:**
- Create: `src/claude/credentials.rs` (replacing the stub)
- Test: `src/claude/credentials.rs`

**Interfaces:**
- Produces: `ClaudeCredential(pub Value)` with `parse`, `from_value`, `wrap_setup_token`, `managed_key`, `kind() -> CredentialKind {OAuth, SetupToken, ApiKey, Unknown}`, `access_token`, `refresh_token`, `expires_at_ms`, `api_key`, `is_expiring(now_ms)`, `is_expired(now_ms)`, `replace_oauth_from(&other)`, `oauth_only()`, `is_newer_than(&other)`, `fingerprint()`; `looks_like_api_key`, `looks_like_setup_token`; `OauthAccount(pub Value)` with `synthesized(email)`, `email_address`, `account_uuid`, `organization_uuid`, `organization_name`, `identity() -> Option<Identity>`; `SlotFile { credential, oauth_account }` with `new`, `to_value`, `from_value`, `identity`; constants `OAUTH_KEY`, `MANAGED_KEY`, `OAUTH_ACCOUNT_KEY`, `EXPIRY_BUFFER_MS`.
- Consumes: `model::Identity`, `sha2`, `hex`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn oauth(access: &str, refresh: Option<&str>, expires_at: Option<i64>) -> ClaudeCredential {
        let mut inner = json!({"accessToken": access, "scopes": ["user:inference", "user:profile"]});
        if let Some(refresh) = refresh {
            inner["refreshToken"] = json!(refresh);
        }
        if let Some(expires_at) = expires_at {
            inner["expiresAt"] = json!(expires_at);
        }
        ClaudeCredential::from_value(json!({OAUTH_KEY: inner, "mcpOAuth": {"srv": {"accessToken": "m"}}}))
    }

    #[test]
    fn kinds_and_accessors() {
        let full = oauth("at", Some("rt"), Some(1_000));
        assert_eq!(full.kind(), CredentialKind::OAuth);
        assert_eq!(full.access_token(), Some("at"));
        assert_eq!(full.refresh_token(), Some("rt"));
        assert_eq!(full.expires_at_ms(), Some(1_000));
        assert_eq!(full.scopes(), vec!["user:inference", "user:profile"]);
        assert_eq!(full.api_key(), None);
        let setup = ClaudeCredential::wrap_setup_token("sk-ant-oat01-x");
        assert_eq!(setup.kind(), CredentialKind::SetupToken);
        assert_eq!(setup.access_token(), Some("sk-ant-oat01-x"));
        assert_eq!(setup.scopes(), vec!["user:inference"]);
        assert_eq!(setup.expires_at_ms(), None);
        let key = ClaudeCredential::managed_key("sk-ant-api03-k");
        assert_eq!(key.kind(), CredentialKind::ApiKey);
        assert_eq!(key.api_key(), Some("sk-ant-api03-k"));
        assert_eq!(ClaudeCredential::from_value(json!({})).kind(), CredentialKind::Unknown);
        assert_eq!(ClaudeCredential::from_value(json!({OAUTH_KEY: {"accessToken": ""}})).kind(), CredentialKind::Unknown);
        assert!(ClaudeCredential::parse("[1]").is_err());
        assert!(ClaudeCredential::parse("{").is_err());
        assert!(looks_like_api_key("sk-ant-api03-abc"));
        assert!(!looks_like_api_key("{\"sk-ant-api\": 1}"));
        assert!(!looks_like_api_key("sk-ant-oat01-abc"));
        assert!(looks_like_setup_token(" sk-ant-oat01-abc"));
        assert!(!looks_like_setup_token("sk-openai"));
    }

    #[test]
    fn expiry_uses_the_five_minute_buffer() {
        let cred = oauth("at", Some("rt"), Some(1_000_000));
        assert!(!cred.is_expiring(1_000_000 - EXPIRY_BUFFER_MS - 1));
        assert!(cred.is_expiring(1_000_000 - EXPIRY_BUFFER_MS));
        assert!(!cred.is_expired(999_999));
        assert!(cred.is_expired(1_000_000));
        let setup = ClaudeCredential::wrap_setup_token("t");
        assert!(!setup.is_expiring(i64::MAX / 2), "no expiresAt never expires");
        assert!(!setup.is_expired(i64::MAX / 2));
    }

    #[test]
    fn replace_oauth_keeps_siblings_and_oauth_only_strips_them() {
        let mut live = oauth("old", Some("rt-old"), Some(1));
        let fresh = oauth("new", Some("rt-new"), Some(2));
        live.replace_oauth_from(&fresh);
        assert_eq!(live.access_token(), Some("new"));
        assert_eq!(live.0["mcpOAuth"]["srv"]["accessToken"], "m");
        let only = fresh.oauth_only();
        assert_eq!(only.0, json!({OAUTH_KEY: fresh.0[OAUTH_KEY]}));
        let key_only = ClaudeCredential::managed_key("k").oauth_only();
        assert_eq!(key_only.0, json!({MANAGED_KEY: "k"}));
        let mut empty = ClaudeCredential::from_value(json!({}));
        empty.replace_oauth_from(&fresh);
        assert_eq!(empty.kind(), CredentialKind::OAuth);
    }

    #[test]
    fn freshness_rule() {
        let stored = oauth("a", Some("rt-1"), Some(100));
        assert!(!oauth("b", Some("rt-1"), Some(200)).is_newer_than(&stored), "same refresh token");
        assert!(oauth("b", Some("rt-2"), Some(101)).is_newer_than(&stored));
        assert!(!oauth("b", Some("rt-2"), Some(100)).is_newer_than(&stored), "equal expiry is not newer");
        assert!(!oauth("b", Some("rt-2"), None).is_newer_than(&stored));
        assert!(oauth("b", Some("rt-2"), Some(1)).is_newer_than(&oauth("a", Some("rt-1"), None)));
        assert!(!ClaudeCredential::wrap_setup_token("t").is_newer_than(&stored), "no refresh token");
        let key_a = ClaudeCredential::managed_key("a");
        let key_b = ClaudeCredential::managed_key("b");
        assert!(key_b.is_newer_than(&key_a));
        assert!(!key_a.is_newer_than(&key_a.clone()));
        assert!(!stored.is_newer_than(&key_a));
    }

    #[test]
    fn fingerprint_prefers_the_refresh_token() {
        use sha2::{Digest, Sha256};
        let cred = oauth("a", Some("rt-1"), None);
        assert_eq!(
            cred.fingerprint(),
            Some(format!("sha256:{}", hex::encode(Sha256::digest(b"rt-1"))))
        );
        assert!(ClaudeCredential::managed_key("k").fingerprint().unwrap().starts_with("sha256-full:"));
        assert_eq!(ClaudeCredential::from_value(serde_json::Value::Null).fingerprint(), None);
    }

    #[test]
    fn oauth_account_identity_and_slot_file_round_trip() {
        let account = OauthAccount(json!({
            "accountUuid": "acc-1", "emailAddress": "Me@Example.com",
            "organizationUuid": "org-1", "organizationName": "Acme", "billingType": "stripe"
        }));
        assert_eq!(account.identity(), Some(Identity::new("me@example.com", "org-1")));
        assert_eq!(account.organization_name(), "Acme");
        assert_eq!(account.account_uuid(), "acc-1");
        let synthesized = OauthAccount::synthesized("k@token.local");
        assert_eq!(synthesized.identity(), Some(Identity::new("k@token.local", "")));
        assert_eq!(synthesized.organization_name(), "");
        assert_eq!(OauthAccount(json!({})).identity(), None);

        let slot = SlotFile::new(&oauth("a", Some("rt"), Some(5)), account.clone());
        let value = slot.to_value();
        assert_eq!(value[OAUTH_KEY]["refreshToken"], "rt");
        assert!(value.get("mcpOAuth").is_none(), "siblings are not stored");
        assert_eq!(value[OAUTH_ACCOUNT_KEY]["billingType"], "stripe", "oauthAccount is kept verbatim");
        let back = SlotFile::from_value(&value).unwrap();
        assert_eq!(back, slot);
        assert_eq!(back.identity(), account.identity());
        assert!(SlotFile::from_value(&json!({OAUTH_KEY: {}})).is_err(), "oauthAccount is required");
        let key = SlotFile::new(&ClaudeCredential::managed_key("k"), synthesized);
        assert_eq!(key.to_value()[MANAGED_KEY], "k");
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib claude::credentials::`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Claude Code's credential object (`{"claudeAiOauth": …}` plus machine-scoped
//! siblings, or a managed `sk-ant-api…` key), the global config's
//! `oauthAccount` (the identity), and the slot file cswitch stores for a
//! Claude account (spec §5).

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::errors::{CswitchError, Result};
use crate::model::Identity;

pub const OAUTH_KEY: &str = "claudeAiOauth";
pub const MANAGED_KEY: &str = "primaryApiKey";
pub const OAUTH_ACCOUNT_KEY: &str = "oauthAccount";
/// A token is "expiring" this long before `expiresAt` (cswap's buffer).
pub const EXPIRY_BUFFER_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    /// Access + refresh token.
    OAuth,
    /// `claude setup-token`: an access token with no refresh token, never refreshed.
    SetupToken,
    /// A managed `sk-ant-api…` key.
    ApiKey,
    Unknown,
}

/// A bare `sk-ant-api…` key; a JSON object never is.
pub fn looks_like_api_key(text: &str) -> bool {
    let text = text.trim();
    text.starts_with("sk-ant-api") && !text.starts_with('{')
}

pub fn looks_like_setup_token(text: &str) -> bool {
    text.trim().starts_with("sk-ant-oat")
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeCredential(pub Value);

impl ClaudeCredential {
    pub fn from_value(value: Value) -> Self {
        Self(value)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text)
            .map_err(|err| CswitchError::credential_read(format!("invalid JSON: {err}")))?;
        if !value.is_object() {
            return Err(CswitchError::credential_read("not a JSON object"));
        }
        Ok(Self(value))
    }

    /// The object cswap writes for a setup-token.
    pub fn wrap_setup_token(token: &str) -> Self {
        Self(json!({OAUTH_KEY: {"accessToken": token.trim(), "scopes": ["user:inference"]}}))
    }

    /// A managed API key, under the key Claude Code uses in its global config.
    pub fn managed_key(key: &str) -> Self {
        Self(json!({MANAGED_KEY: key.trim()}))
    }

    fn oauth(&self) -> Option<&Map<String, Value>> {
        self.0.get(OAUTH_KEY)?.as_object()
    }

    fn oauth_text(&self, key: &str) -> Option<&str> {
        self.oauth()?
            .get(key)?
            .as_str()
            .filter(|s| !s.is_empty())
    }

    pub fn kind(&self) -> CredentialKind {
        if self.access_token().is_some() {
            return if self.refresh_token().is_some() {
                CredentialKind::OAuth
            } else {
                CredentialKind::SetupToken
            };
        }
        if self.api_key().is_some() {
            return CredentialKind::ApiKey;
        }
        CredentialKind::Unknown
    }

    pub fn access_token(&self) -> Option<&str> {
        self.oauth_text("accessToken")
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.oauth_text("refreshToken")
    }

    /// `expiresAt` in milliseconds (a number; a float is truncated).
    pub fn expires_at_ms(&self) -> Option<i64> {
        let value = self.oauth()?.get("expiresAt")?;
        value.as_i64().or_else(|| value.as_f64().map(|f| f as i64))
    }

    pub fn scopes(&self) -> Vec<String> {
        self.oauth()
            .and_then(|o| o.get("scopes"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn api_key(&self) -> Option<&str> {
        self.0
            .get(MANAGED_KEY)?
            .as_str()
            .filter(|k| !k.trim().is_empty())
    }

    /// Within the 5-minute buffer of `expiresAt`; never without one.
    pub fn is_expiring(&self, now_ms: i64) -> bool {
        self.expires_at_ms()
            .is_some_and(|expires_at| now_ms + EXPIRY_BUFFER_MS >= expires_at)
    }

    pub fn is_expired(&self, now_ms: i64) -> bool {
        self.expires_at_ms()
            .is_some_and(|expires_at| expires_at <= now_ms)
    }

    /// Replace this object's `claudeAiOauth` with `fresh`'s; every other
    /// top-level key (Claude Code's `mcpOAuth` tokens, …) stays as it is.
    pub fn replace_oauth_from(&mut self, fresh: &ClaudeCredential) {
        if !self.0.is_object() {
            self.0 = json!({});
        }
        let map = self.0.as_object_mut().expect("object");
        match fresh.0.get(OAUTH_KEY) {
            Some(oauth) => {
                map.insert(OAUTH_KEY.to_string(), oauth.clone());
            }
            None => {
                map.remove(OAUTH_KEY);
            }
        }
    }

    /// Just the login: `{"claudeAiOauth": …}` or `{"primaryApiKey": …}`.
    pub fn oauth_only(&self) -> ClaudeCredential {
        match self.kind() {
            CredentialKind::ApiKey => Self::managed_key(self.api_key().unwrap_or_default()),
            _ => Self(json!({OAUTH_KEY: self.0.get(OAUTH_KEY).cloned().unwrap_or(Value::Null)})),
        }
    }

    /// The freshness rule for folding a live copy back: a different refresh
    /// token with a strictly later `expiresAt` (a missing stamp is not proof);
    /// API keys are newer when the key differs.
    pub fn is_newer_than(&self, other: &ClaudeCredential) -> bool {
        if self.kind() == CredentialKind::ApiKey || other.kind() == CredentialKind::ApiKey {
            return self.kind() == CredentialKind::ApiKey && self.api_key() != other.api_key();
        }
        if self.refresh_token().is_none() || self.refresh_token() == other.refresh_token() {
            return false;
        }
        match (self.expires_at_ms(), other.expires_at_ms()) {
            (Some(mine), Some(theirs)) => mine > theirs,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// `sha256:<refresh token>` when present, else `sha256-full:<canonical JSON>`.
    pub fn fingerprint(&self) -> Option<String> {
        if self.0.is_null() {
            return None;
        }
        if let Some(token) = self.refresh_token() {
            return Some(format!("sha256:{}", hex::encode(Sha256::digest(token.as_bytes()))));
        }
        let canonical = serde_json::to_string(&self.0).ok()?;
        Some(format!("sha256-full:{}", hex::encode(Sha256::digest(canonical.as_bytes()))))
    }
}

/// The `oauthAccount` object of Claude Code's global config, kept verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct OauthAccount(pub Value);

impl OauthAccount {
    /// What token accounts get (cswap's shape).
    pub fn synthesized(email: &str) -> Self {
        Self(json!({
            "emailAddress": email, "accountUuid": "",
            "organizationUuid": null, "organizationName": null
        }))
    }

    fn text(&self, key: &str) -> String {
        self.0
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    pub fn email_address(&self) -> String {
        self.text("emailAddress")
    }

    pub fn account_uuid(&self) -> String {
        self.text("accountUuid")
    }

    pub fn organization_uuid(&self) -> String {
        self.text("organizationUuid")
    }

    pub fn organization_name(&self) -> String {
        self.text("organizationName")
    }

    /// `(email lowercased, organizationUuid)`; `None` without an email.
    pub fn identity(&self) -> Option<Identity> {
        let email = self.email_address();
        if email.is_empty() {
            return None;
        }
        Some(Identity::new(email.to_lowercase(), self.organization_uuid()))
    }
}

/// `credentials/<n>.json` for a Claude account: the login plus its identity.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotFile {
    pub credential: ClaudeCredential,
    pub oauth_account: OauthAccount,
}

impl SlotFile {
    /// Stores only the login part of `credential` (no siblings).
    pub fn new(credential: &ClaudeCredential, oauth_account: OauthAccount) -> Self {
        Self {
            credential: credential.oauth_only(),
            oauth_account,
        }
    }

    pub fn to_value(&self) -> Value {
        let mut value = self.credential.0.clone();
        if !value.is_object() {
            value = json!({});
        }
        value[OAUTH_ACCOUNT_KEY] = self.oauth_account.0.clone();
        value
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let mut map = value
            .as_object()
            .cloned()
            .ok_or_else(|| CswitchError::credential_read("stored Claude credentials are not a JSON object"))?;
        let account = map
            .remove(OAUTH_ACCOUNT_KEY)
            .filter(Value::is_object)
            .ok_or_else(|| CswitchError::credential_read("stored Claude credentials carry no oauthAccount"))?;
        Ok(Self {
            credential: ClaudeCredential(Value::Object(map)),
            oauth_account: OauthAccount(account),
        })
    }

    pub fn identity(&self) -> Option<Identity> {
        self.oauth_account.identity()
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib claude::credentials:: && cargo clippy --all-targets -- -D warnings`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add src/claude/credentials.rs
git commit -m "feat(claude): model the credential object, oauthAccount and the Claude slot file"
```

---

### Task 7: `claude::live` — the live login across Keychain and file, plus backups

Deviation from spec §14: the live read/write lives in `src/claude/live.rs` so `credentials.rs` stays a pure model.

**Files:**
- Create: `src/claude/live.rs` (replacing the stub)
- Test: `src/claude/live.rs`

**Interfaces:**
- Produces: `Backend { Keychain, File }`; `LiveLogin { credential: Option<ClaudeCredential>, oauth_account: Option<OauthAccount>, keychain_unavailable: bool }` with `identity()`; `ClaudeLive::new(&Paths, &dyn SecurityCli)` with `read() -> Result<LiveLogin>`, `write_oauth(&ClaudeCredential) -> Result<Backend>`, `write_managed_key(&str) -> Result<Backend>`, `read_global_config() -> Result<Option<Value>>`, `update_global_config(FnOnce(&mut Map))`; `backup_live(&Paths, &LiveLogin) -> Result<()>`; `KEYCHAIN_FOLLOWUP`, `FILE_FOLLOWUP`.
- Consumes: Task 4 paths, Task 5 keychain, Task 6 model, `fsutil::{atomic_write_private, write_json_private, read_json, io_error}`, `store::ensure_private_dir` (make it `pub(crate)` — it already is).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::keychain::CliOutput;
    use crate::store::temp_store;
    use serde_json::json;
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::io;

    /// A Keychain that stores items in memory and can be told to fail.
    #[derive(Default)]
    struct FakeKeychain {
        items: RefCell<BTreeMap<(String, String), String>>,
        failures: RefCell<VecDeque<()>>,
    }

    impl FakeKeychain {
        fn fail_next(&self) {
            self.failures.borrow_mut().push_back(());
        }
        fn item(&self, service: &str) -> Option<String> {
            self.items
                .borrow()
                .get(&(service.to_string(), crate::claude::keychain::account_name()))
                .cloned()
        }
    }

    impl SecurityCli for FakeKeychain {
        fn run(&self, args: &[String], stdin_line: Option<&str>) -> io::Result<CliOutput> {
            if self.failures.borrow_mut().pop_front().is_some() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "locked"));
            }
            let ok = |stdout: &str| Ok(CliOutput { status: 0, stdout: stdout.to_string() });
            let missing = || Ok(CliOutput { status: 44, stdout: String::new() });
            let mut items = self.items.borrow_mut();
            match args[0].as_str() {
                "find-generic-password" => {
                    let key = (args[args.len() - 1].clone(), args[2].clone());
                    match items.get(&key) {
                        Some(v) => ok(v),
                        None => missing(),
                    }
                }
                "delete-generic-password" => {
                    let key = (args[4].clone(), args[2].clone());
                    if items.remove(&key).is_some() { ok("") } else { missing() }
                }
                "-i" => {
                    let line = stdin_line.unwrap();
                    let parts: Vec<&str> = line.split(' ').collect();
                    let account = parts[3].trim_matches('"').to_string();
                    let service = parts[5].trim_matches('"').to_string();
                    let value = String::from_utf8(hex::decode(parts[7]).unwrap()).unwrap();
                    items.insert((service, account), value);
                    ok("")
                }
                _ => ok(""),
            }
        }
    }

    fn creds(access: &str, refresh: &str) -> ClaudeCredential {
        ClaudeCredential::from_value(json!({
            OAUTH_KEY: {"accessToken": access, "refreshToken": refresh, "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]},
            "mcpOAuth": {"srv": {"accessToken": "m"}}
        }))
    }

    fn write_file(paths: &crate::paths::Paths, value: &serde_json::Value) {
        std::fs::create_dir_all(&paths.claude_home).unwrap();
        std::fs::write(paths.claude_credentials_file(), value.to_string()).unwrap();
    }

    fn write_config(paths: &crate::paths::Paths, value: &serde_json::Value) {
        std::fs::create_dir_all(paths.claude_global_config_file().parent().unwrap()).unwrap();
        std::fs::write(paths.claude_global_config_file(), value.to_string()).unwrap();
    }

    #[test]
    fn file_backend_reads_credentials_then_managed_key_and_the_account() {
        let (_dir, store) = temp_store();
        let fake = FakeKeychain::default();
        let live = ClaudeLive::new(&store.paths, &fake);
        let empty = live.read().unwrap();
        assert!(empty.credential.is_none() && empty.oauth_account.is_none());
        assert!(!empty.keychain_unavailable);

        write_config(&store.paths, &json!({"oauthAccount": {"emailAddress": "A@x.io", "organizationUuid": "org"}, "projects": {"/p": {}}}));
        write_file(&store.paths, &creds("at", "rt").0);
        let login = live.read().unwrap();
        assert_eq!(login.credential.as_ref().unwrap().access_token(), Some("at"));
        assert_eq!(login.identity(), Some(crate::model::Identity::new("a@x.io", "org")));

        std::fs::remove_file(store.paths.claude_credentials_file()).unwrap();
        write_config(&store.paths, &json!({"primaryApiKey": "sk-ant-api03-k", "oauthAccount": {"emailAddress": "A@x.io"}}));
        let login = live.read().unwrap();
        assert_eq!(login.credential.as_ref().unwrap().api_key(), Some("sk-ant-api03-k"));
        assert!(fake.items.borrow().is_empty(), "the file backend never touches the Keychain");
    }

    #[test]
    fn keychain_backend_wins_over_the_file_and_degrades_stickily() {
        let (_dir, store) = temp_store();
        let mut paths = store.paths.clone();
        paths.keychain_enabled = true;
        let fake = FakeKeychain::default();
        write_file(&paths, &creds("file", "rt-file").0);
        let live = ClaudeLive::new(&paths, &fake);
        assert_eq!(live.read().unwrap().credential.unwrap().access_token(), Some("file"));

        live.write_oauth(&creds("kc", "rt-kc")).unwrap();
        assert_eq!(live.read().unwrap().credential.unwrap().access_token(), Some("kc"));
        assert!(fake.item(LIVE_SERVICE).unwrap().contains("rt-kc"));
        let file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.claude_credentials_file()).unwrap()).unwrap();
        assert_eq!(file[OAUTH_KEY]["refreshToken"], "rt-kc", "an existing file is rewritten");

        fake.fail_next();
        let degraded = live.read().unwrap();
        assert!(degraded.keychain_unavailable);
        assert_eq!(degraded.credential.unwrap().access_token(), Some("kc"), "the file still answers");
        assert_eq!(live.read().unwrap().credential.unwrap().access_token(), Some("kc"));
        assert!(!live.read().unwrap().keychain_unavailable, "sticky file mode after a failure: no second probe");
        let backend = live.write_oauth(&creds("after", "rt-after")).unwrap();
        assert_eq!(backend, Backend::File);
        assert_eq!(fake.item(LIVE_SERVICE), None, "a stale Keychain item is deleted");
    }

    #[test]
    fn keychain_write_never_creates_the_file_and_failure_falls_back() {
        let (_dir, store) = temp_store();
        let mut paths = store.paths.clone();
        paths.keychain_enabled = true;
        let fake = FakeKeychain::default();
        let live = ClaudeLive::new(&paths, &fake);
        assert_eq!(live.write_oauth(&creds("a", "rt")).unwrap(), Backend::Keychain);
        assert!(!paths.claude_credentials_file().exists());
        let fake2 = FakeKeychain::default();
        fake2.fail_next();
        let live2 = ClaudeLive::new(&paths, &fake2);
        assert_eq!(live2.write_oauth(&creds("b", "rt-b")).unwrap(), Backend::File);
        assert!(paths.claude_credentials_file().exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.claude_credentials_file()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let unavailable = live2.read().unwrap();
        assert!(unavailable.keychain_unavailable, "no probe after the pin, so the flag reports the pinned state");
    }

    #[test]
    fn managed_key_write_clears_oauth_and_vice_versa() {
        let (_dir, store) = temp_store();
        let fake = FakeKeychain::default();
        let live = ClaudeLive::new(&store.paths, &fake);
        write_config(&store.paths, &json!({"projects": {"/p": {"allowedTools": []}}}));
        live.write_oauth(&creds("a", "rt")).unwrap();
        assert_eq!(live.write_managed_key("sk-ant-api03-k").unwrap(), Backend::File);
        let config = live.read_global_config().unwrap().unwrap();
        assert_eq!(config["primaryApiKey"], "sk-ant-api03-k");
        assert_eq!(config["projects"]["/p"]["allowedTools"], json!([]), "other keys survive");
        let file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(store.paths.claude_credentials_file()).unwrap()).unwrap();
        assert!(file.get(OAUTH_KEY).is_none(), "the OAuth login is cleared");
        assert_eq!(file["mcpOAuth"]["srv"]["accessToken"], "m", "siblings stay");
        live.write_oauth(&creds("b", "rt-b")).unwrap();
        assert!(live.read_global_config().unwrap().unwrap().get("primaryApiKey").is_none());
        assert_eq!(live.read().unwrap().credential.unwrap().access_token(), Some("b"));
    }

    #[test]
    fn update_global_config_preserves_everything_else() {
        let (_dir, store) = temp_store();
        let fake = FakeKeychain::default();
        let live = ClaudeLive::new(&store.paths, &fake);
        live.update_global_config(|cfg| {
            cfg.insert("oauthAccount".into(), json!({"emailAddress": "a@x.io"}));
        })
        .unwrap();
        write_config(&store.paths, &json!({"oauthAccount": {"emailAddress": "old"}, "mcpServers": {"x": 1}, "numStartups": 7}));
        live.update_global_config(|cfg| {
            cfg.insert("oauthAccount".into(), json!({"emailAddress": "new"}));
        })
        .unwrap();
        let config = live.read_global_config().unwrap().unwrap();
        assert_eq!(config["oauthAccount"]["emailAddress"], "new");
        assert_eq!(config["mcpServers"]["x"], 1);
        assert_eq!(config["numStartups"], 7);
        std::fs::write(store.paths.claude_global_config_file(), "[1]").unwrap();
        assert_eq!(live.read_global_config().unwrap_err().type_name(), "ConfigError");
    }

    #[test]
    fn backups_keep_three() {
        let (_dir, store) = temp_store();
        let login = LiveLogin {
            credential: Some(creds("a", "rt")),
            oauth_account: Some(OauthAccount::synthesized("a@x.io")),
            keychain_unavailable: false,
        };
        for _ in 0..5 {
            backup_live(&store.paths, &login).unwrap();
        }
        let mut names: Vec<String> = std::fs::read_dir(store.paths.claude_backups_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 3);
        let newest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(store.paths.claude_backups_dir().join(names.last().unwrap())).unwrap()).unwrap();
        assert_eq!(newest["credentials"][OAUTH_KEY]["refreshToken"], "rt");
        assert_eq!(newest["oauthAccount"]["emailAddress"], "a@x.io");
        let empty = LiveLogin { credential: None, oauth_account: None, keychain_unavailable: false };
        backup_live(&store.paths, &empty).unwrap();
        assert_eq!(std::fs::read_dir(store.paths.claude_backups_dir()).unwrap().count(), 3, "nothing to back up");
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib claude::live::`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Claude Code's live login: read and write across the macOS Keychain and the
//! plaintext file exactly as Claude Code does (spec §4, §7), the global
//! config splice, and the pre-switch backup.

use std::cell::Cell;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::errors::{CswitchError, Result};
use crate::fsutil;
use crate::model::Identity;
use crate::paths::Paths;
use crate::store::ensure_private_dir;

use super::credentials::{ClaudeCredential, CredentialKind, MANAGED_KEY, OAUTH_ACCOUNT_KEY, OAUTH_KEY, OauthAccount};
use super::keychain::{Keychain, LIVE_SERVICE, MANAGED_SERVICE, SecurityCli, account_name};

/// The line printed after a switch, keyed to where the live write landed.
pub const KEYCHAIN_FOLLOWUP: &str = "Restart Claude Code to apply immediately — otherwise the session can take up to ~30 seconds to pick up the new account.";
pub const FILE_FOLLOWUP: &str = "New account is active on your next message — no restart needed.";
const MAX_BACKUPS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Keychain,
    File,
}

impl Backend {
    pub fn followup(self) -> &'static str {
        match self {
            Self::Keychain => KEYCHAIN_FOLLOWUP,
            Self::File => FILE_FOLLOWUP,
        }
    }
}

/// What Claude Code is logged in as right now.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveLogin {
    pub credential: Option<ClaudeCredential>,
    pub oauth_account: Option<OauthAccount>,
    /// The Keychain could not be read and nothing else covered the login.
    pub keychain_unavailable: bool,
}

impl LiveLogin {
    pub fn identity(&self) -> Option<Identity> {
        self.oauth_account.as_ref()?.identity()
    }
}

pub struct ClaudeLive<'a> {
    paths: &'a Paths,
    keychain: Keychain<'a>,
    /// Sticky for the process: one Keychain failure routes every later call
    /// to the file so a command never splits between backends.
    file_mode: Cell<bool>,
    account: String,
}

fn read_text_if_present(path: &std::path::Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(fsutil::io_error(CswitchError::CredentialRead, path, &err)),
    }
}

impl<'a> ClaudeLive<'a> {
    pub fn new(paths: &'a Paths, cli: &'a dyn SecurityCli) -> Self {
        Self {
            paths,
            keychain: Keychain::new(cli),
            file_mode: Cell::new(!paths.keychain_enabled),
            account: account_name(),
        }
    }

    fn use_keychain(&self) -> bool {
        !self.file_mode.get()
    }

    fn pin_file_mode(&self) {
        self.file_mode.set(true);
    }

    /// Keychain (when usable) → `.credentials.json` → managed key (Keychain
    /// `Claude Code`, then `primaryApiKey`), plus `oauthAccount`.
    pub fn read(&self) -> Result<LiveLogin> {
        let mut keychain_unavailable = false;
        let mut credential = None;
        if self.use_keychain() {
            match self.keychain.get_password(LIVE_SERVICE, &self.account) {
                Ok(Some(text)) => credential = Some(ClaudeCredential::parse(&text)?),
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!("Claude Keychain read failed, using the file backend: {err}");
                    keychain_unavailable = true;
                    self.pin_file_mode();
                }
            }
        } else if self.paths.keychain_enabled {
            // Pinned after an earlier failure: an empty slot below is the
            // Keychain's fault, not a logged-out machine.
            keychain_unavailable = true;
        }
        if credential.is_none()
            && let Some(text) = read_text_if_present(&self.paths.claude_credentials_file())?
            && !text.trim().is_empty()
        {
            credential = Some(ClaudeCredential::parse(&text)?);
        }
        if credential.is_none()
            && let Some(key) = self.read_managed_key()?
        {
            credential = Some(ClaudeCredential::managed_key(&key));
        }
        let oauth_account = self
            .read_global_config()?
            .and_then(|config| config.get(OAUTH_ACCOUNT_KEY).filter(|v| v.is_object()).cloned())
            .map(OauthAccount);
        Ok(LiveLogin {
            keychain_unavailable: keychain_unavailable && credential.is_none(),
            credential,
            oauth_account,
        })
    }

    fn read_managed_key(&self) -> Result<Option<String>> {
        if self.use_keychain() {
            match self.keychain.get_password(MANAGED_SERVICE, &self.account) {
                Ok(Some(key)) if !key.trim().is_empty() => return Ok(Some(key)),
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!("Claude Keychain read failed, using the file backend: {err}");
                    self.pin_file_mode();
                }
            }
        }
        Ok(self.read_global_config()?.and_then(|config| {
            config
                .get(MANAGED_KEY)
                .and_then(Value::as_str)
                .filter(|k| !k.trim().is_empty())
                .map(str::to_string)
        }))
    }

    /// `~/.claude.json` (or the legacy file) as an object; `None` when absent.
    pub fn read_global_config(&self) -> Result<Option<Value>> {
        let path = self.paths.claude_global_config_file();
        let value = fsutil::read_json(&path).map_err(|err| {
            CswitchError::config(format!("{} could not be read ({err})", path.display()))
        })?;
        match value {
            Some(value) if !value.is_object() => Err(CswitchError::config(format!(
                "{} does not hold a JSON object",
                path.display()
            ))),
            other => Ok(other),
        }
    }

    /// Read-modify-write of the global config preserving every other key;
    /// creates the file when absent. Atomic, 0600.
    pub fn update_global_config(&self, mutate: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
        let path = self.paths.claude_global_config_file();
        let mut value = self.read_global_config()?.unwrap_or_else(|| json!({}));
        mutate(value.as_object_mut().expect("object checked on read"));
        fsutil::write_json_private(&path, &value)
            .map_err(|err| fsutil::io_error(CswitchError::CredentialWrite, &path, &err))
    }

    /// Write the OAuth login where Claude Code reads it (spec §7.2c): the
    /// Keychain when usable (an already-present file is rewritten, never
    /// created), else the file (and a stale Keychain item is dropped).
    pub fn write_oauth(&self, live: &ClaudeCredential) -> Result<Backend> {
        let text = serde_json::to_string(&live.0)
            .map_err(|err| CswitchError::credential_write(format!("invalid credential: {err}")))?;
        let file = self.paths.claude_credentials_file();
        if self.use_keychain() {
            match self.keychain.set_password(LIVE_SERVICE, &self.account, &text) {
                Ok(()) => {
                    if file.exists()
                        && let Err(err) = fsutil::atomic_write_private(&file, text.as_bytes())
                    {
                        tracing::warn!("could not refresh {} after the Keychain write: {err}", file.display());
                    }
                    self.clear_managed_key()?;
                    return Ok(Backend::Keychain);
                }
                Err(err) => {
                    tracing::warn!("Claude Keychain write failed, falling back to the file: {err}");
                    self.pin_file_mode();
                }
            }
        }
        fsutil::atomic_write_private(&file, text.as_bytes())
            .map_err(|err| fsutil::io_error(CswitchError::CredentialWrite, &file, &err))?;
        if self.paths.keychain_enabled {
            let _ = self.keychain.delete_password(LIVE_SERVICE, &self.account);
        }
        self.clear_managed_key()?;
        Ok(Backend::File)
    }

    /// Write a managed API key (Keychain `Claude Code`, else `primaryApiKey`)
    /// and clear any OAuth login.
    pub fn write_managed_key(&self, key: &str) -> Result<Backend> {
        let backend = if self.use_keychain()
            && self.keychain.set_password(MANAGED_SERVICE, &self.account, key).is_ok()
        {
            Backend::Keychain
        } else {
            self.pin_file_mode();
            self.update_global_config(|config| {
                config.insert(MANAGED_KEY.to_string(), json!(key));
            })?;
            Backend::File
        };
        self.clear_oauth()?;
        Ok(backend)
    }

    fn clear_managed_key(&self) -> Result<()> {
        if self.paths.keychain_enabled {
            let _ = self.keychain.delete_password(MANAGED_SERVICE, &self.account);
        }
        if self
            .read_global_config()?
            .is_some_and(|config| config.get(MANAGED_KEY).is_some())
        {
            self.update_global_config(|config| {
                config.remove(MANAGED_KEY);
            })?;
        }
        Ok(())
    }

    fn clear_oauth(&self) -> Result<()> {
        if self.paths.keychain_enabled {
            let _ = self.keychain.delete_password(LIVE_SERVICE, &self.account);
        }
        let file = self.paths.claude_credentials_file();
        if let Some(text) = read_text_if_present(&file)?
            && let Ok(mut credential) = ClaudeCredential::parse(&text)
            && credential.0.get(OAUTH_KEY).is_some()
        {
            credential.0.as_object_mut().expect("object").remove(OAUTH_KEY);
            fsutil::write_json_private(&file, &credential.0)
                .map_err(|err| fsutil::io_error(CswitchError::CredentialWrite, &file, &err))?;
        }
        Ok(())
    }
}

/// `backups/claude/<unix_nanos>.json` = `{"credentials": …, "oauthAccount": …}`,
/// three kept. Nothing to back up is not an error.
pub fn backup_live(paths: &Paths, login: &LiveLogin) -> Result<()> {
    if login.credential.is_none() && login.oauth_account.is_none() {
        return Ok(());
    }
    let dir = paths.claude_backups_dir();
    ensure_private_dir(&dir)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CswitchError::credential_write("system clock is before the Unix epoch"))?
        .as_nanos();
    let path = (0..1000u16)
        .map(|n| if n == 0 { dir.join(format!("{nanos}.json")) } else { dir.join(format!("{nanos}-{n}.json")) })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| CswitchError::credential_write("could not allocate a Claude backup path"))?;
    let value = json!({
        "credentials": login.credential.as_ref().map(|c| c.0.clone()).unwrap_or(Value::Null),
        "oauthAccount": login.oauth_account.as_ref().map(|a| a.0.clone()).unwrap_or(Value::Null),
    });
    fsutil::write_json_private(&path, &value)
        .map_err(|err| fsutil::io_error(CswitchError::CredentialWrite, &path, &err))?;
    let mut backups: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    if backups.len() > MAX_BACKUPS {
        backups.sort();
        for old in &backups[..backups.len() - MAX_BACKUPS] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}
```

Check that `crate::fsutil::read_json` returns `io::Result<Option<Value>>` (it does, `fsutil.rs:47`) and that `ensure_private_dir` is `pub(crate)` in `src/store/mod.rs` (it is).

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib claude::live:: && cargo clippy --all-targets -- -D warnings`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add src/claude/live.rs
git commit -m "feat(claude): read and write the live Claude Code login across the Keychain and the file"
```

---

### Task 8: `claude::locks` — Claude Code's directory locks

**Files:**
- Create: `src/claude/locks.rs` (replacing the stub)
- Modify: `Cargo.toml` (add `filetime = "0.2"`)
- Test: `src/claude/locks.rs`

**Interfaces:**
- Produces: `LockSpec { path, stale }`, `ClaudeLocks` (RAII guard), `specs(&Paths) -> Vec<LockSpec>`, `acquire(&Paths) -> Result<ClaudeLocks>`, `acquire_with(specs, budget, touch_every) -> Result<ClaudeLocks>`, constants `CREDENTIALS_STALE`, `CONFIG_STALE`, `TOUCH_INTERVAL`, `WAIT_BUDGET`, `wait_budget()` (honours `CSWITCH_CLAUDE_LOCK_BUDGET_MS`, tests only).
- Consumes: Task 4 lock paths, `rand`, `filetime`.

- [ ] **Step 1: Add the dependency and write the failing tests**

`Cargo.toml` `[dependencies]`: `filetime = "0.2"`.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn spec(dir: &std::path::Path, name: &str, stale: Duration) -> LockSpec {
        LockSpec {
            path: dir.join(name),
            stale,
        }
    }

    #[test]
    fn acquire_creates_dirs_in_order_and_drop_removes_them() {
        let dir = tempfile::tempdir().unwrap();
        let specs = vec![
            spec(dir.path(), "a.lock", CREDENTIALS_STALE),
            spec(dir.path(), "nested/b.lock", CONFIG_STALE),
        ];
        let locks = acquire_with(specs.clone(), Duration::from_millis(50), TOUCH_INTERVAL).unwrap();
        assert!(dir.path().join("a.lock").is_dir());
        assert!(dir.path().join("nested/b.lock").is_dir());
        assert_eq!(locks.held().len(), 2);
        drop(locks);
        assert!(!dir.path().join("a.lock").exists());
        assert!(!dir.path().join("nested/b.lock").exists());
    }

    #[test]
    fn stale_lock_is_stolen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.lock");
        std::fs::create_dir(&path).unwrap();
        let old = filetime::FileTime::from_unix_time(filetime::FileTime::now().unix_seconds() - 120, 0);
        filetime::set_file_mtime(&path, old).unwrap();
        let locks = acquire_with(vec![spec(dir.path(), "a.lock", Duration::from_secs(60))], Duration::from_millis(50), TOUCH_INTERVAL).unwrap();
        assert_eq!(locks.held().len(), 1);
        let age = std::fs::metadata(&path).unwrap().modified().unwrap().elapsed().unwrap();
        assert!(age < Duration::from_secs(5), "recreated now");
    }

    #[test]
    fn fresh_lock_times_out_with_a_lock_error_and_releases_earlier_locks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("b.lock")).unwrap();
        let started = std::time::Instant::now();
        let err = acquire_with(
            vec![spec(dir.path(), "a.lock", CREDENTIALS_STALE), spec(dir.path(), "b.lock", CREDENTIALS_STALE)],
            Duration::from_millis(200),
            TOUCH_INTERVAL,
        )
        .unwrap_err();
        assert_eq!(err.type_name(), "LockError");
        assert!(err.to_string().starts_with("Claude Code is holding "), "{err}");
        assert!(err.to_string().ends_with("; retry in a moment"));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(!dir.path().join("a.lock").exists(), "the first lock is released on failure");
        assert!(dir.path().join("b.lock").exists(), "the foreign lock is left alone");
    }

    #[test]
    fn held_locks_are_touched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.lock");
        let locks = acquire_with(vec![spec(dir.path(), "a.lock", CREDENTIALS_STALE)], Duration::from_millis(50), Duration::from_millis(100)).unwrap();
        let old = filetime::FileTime::from_unix_time(filetime::FileTime::now().unix_seconds() - 120, 0);
        filetime::set_file_mtime(&path, old).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        let age = std::fs::metadata(&path).unwrap().modified().unwrap().elapsed().unwrap();
        assert!(age < Duration::from_secs(5), "touched while held: {age:?}");
        drop(locks);
    }

    #[test]
    fn specs_follow_the_paths_and_the_budget_env() {
        let (_dir, store) = crate::store::temp_store();
        let specs = specs(&store.paths);
        assert_eq!(specs[0].path, store.paths.claude_refresh_lock_dir());
        assert_eq!(specs[0].stale, CREDENTIALS_STALE);
        assert_eq!(specs[1].path, store.paths.claude_legacy_lock_dir());
        assert_eq!(specs[2].path, store.paths.claude_config_lock_dir());
        assert_eq!(specs[2].stale, CONFIG_STALE);
        assert_eq!(budget_from(None), WAIT_BUDGET);
        assert_eq!(budget_from(Some("250")), Duration::from_millis(250));
        assert_eq!(budget_from(Some("x")), WAIT_BUDGET);
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib claude::locks::`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Claude Code's advisory locks (spec §4): `proper-lockfile` directories whose
//! `mkdir` is the mutex, stale by mtime, touched while held. Holding them
//! around a credential swap means a concurrent Claude Code refresh either
//! finishes first or re-reads and aborts; a crashed holder's lock is taken
//! over once it goes stale.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use std::{fs, io};

use crate::errors::{CswitchError, Result};
use crate::fsutil::io_error;
use crate::paths::Paths;

/// Credential locks (`stale: 60000` in Claude Code).
pub const CREDENTIALS_STALE: Duration = Duration::from_secs(60);
/// The config lock keeps proper-lockfile's default.
pub const CONFIG_STALE: Duration = Duration::from_secs(10);
/// Claude Code touches every 5 s; a little faster for margin.
pub const TOUCH_INTERVAL: Duration = Duration::from_secs(3);
/// Per lock. Claude Code holds a credential lock for one token round trip.
pub const WAIT_BUDGET: Duration = Duration::from_secs(9);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockSpec {
    pub path: PathBuf,
    pub stale: Duration,
}

/// The three locks Claude Code takes around a refresh and a config write, in its order.
pub fn specs(paths: &Paths) -> Vec<LockSpec> {
    vec![
        LockSpec { path: paths.claude_refresh_lock_dir(), stale: CREDENTIALS_STALE },
        LockSpec { path: paths.claude_legacy_lock_dir(), stale: CREDENTIALS_STALE },
        LockSpec { path: paths.claude_config_lock_dir(), stale: CONFIG_STALE },
    ]
}

/// `CSWITCH_CLAUDE_LOCK_BUDGET_MS` shortens the wait (tests); anything else is the default.
pub fn wait_budget() -> Duration {
    budget_from(std::env::var("CSWITCH_CLAUDE_LOCK_BUDGET_MS").ok().as_deref())
}

fn budget_from(value: Option<&str>) -> Duration {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(WAIT_BUDGET, Duration::from_millis)
}

pub fn acquire(paths: &Paths) -> Result<ClaudeLocks> {
    acquire_with(specs(paths), wait_budget(), TOUCH_INTERVAL)
}

/// Holds the directories until dropped and keeps their mtime fresh meanwhile.
pub struct ClaudeLocks {
    held: Vec<PathBuf>,
    stop: Arc<AtomicBool>,
    toucher: Option<JoinHandle<()>>,
}

impl ClaudeLocks {
    pub fn held(&self) -> &[PathBuf] {
        &self.held
    }
}

pub fn acquire_with(specs: Vec<LockSpec>, budget: Duration, touch_every: Duration) -> Result<ClaudeLocks> {
    let mut locks = ClaudeLocks {
        held: Vec::new(),
        stop: Arc::new(AtomicBool::new(false)),
        toucher: None,
    };
    for spec in &specs {
        // On an error the guard drops here and releases what was taken.
        acquire_one(spec, budget)?;
        locks.held.push(spec.path.clone());
    }
    let paths = locks.held.clone();
    let stop = locks.stop.clone();
    locks.toucher = Some(thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let mut slept = Duration::ZERO;
            while slept < touch_every && !stop.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(50));
                slept += Duration::from_millis(50);
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            for path in &paths {
                let _ = filetime::set_file_mtime(path, filetime::FileTime::now());
            }
        }
    }));
    Ok(locks)
}

fn is_stale(path: &Path, stale: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > stale)
}

fn acquire_one(spec: &LockSpec, budget: Duration) -> Result<()> {
    let started = Instant::now();
    if let Some(parent) = spec.path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    loop {
        match fs::create_dir(&spec.path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                if is_stale(&spec.path, spec.stale) {
                    let _ = fs::remove_dir_all(&spec.path);
                    continue;
                }
                let elapsed = started.elapsed();
                if elapsed >= budget {
                    return Err(CswitchError::lock(format!(
                        "Claude Code is holding {}; retry in a moment",
                        spec.path.display()
                    )));
                }
                // 1–2 s jittered, as Claude Code's own retries; never past the budget.
                let jitter = Duration::from_millis(1000 + rand::random::<u64>() % 1000);
                thread::sleep(jitter.min(budget - elapsed));
            }
            Err(err) => return Err(io_error(CswitchError::Lock, &spec.path, &err)),
        }
    }
}

impl Drop for ClaudeLocks {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(toucher) = self.toucher.take() {
            let _ = toucher.join();
        }
        for path in self.held.iter().rev() {
            let _ = fs::remove_dir_all(path);
        }
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib claude::locks:: && cargo clippy --all-targets -- -D warnings`
Expected: 5 passed (the timeout test takes about 0.2 s).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/claude/locks.rs
git commit -m "feat(claude): take Claude Code's credential and config directory locks around a switch"
```

---

### Task 9: `claude::oauth` — token refresh

**Files:**
- Create: `src/claude/oauth.rs` (replacing the stub)
- Test: `src/claude/oauth.rs`

**Interfaces:**
- Produces: `TOKEN_URL`, `CLIENT_ID`, `token_url()`, `ClaudeTokens { access_token, expires_at_ms, refresh_token: Option<String>, scopes: Option<Vec<String>> }` with `apply_to(&mut ClaudeCredential)`, `async fn refresh(&reqwest::Client, refresh_token) -> Result<ClaudeTokens, RefreshError>`; re-exports `RefreshError` from `crate::codex::oauth`.
- Consumes: `codex::oauth::RefreshError` (fields are `pub`).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use serde_json::json;

    #[test]
    fn success_parses_tokens_and_optional_rotation() {
        let tokens = resolve(
            StatusCode::OK,
            r#"{"access_token":"at-new","expires_in":3600,"refresh_token":"rt-new","scope":"user:inference user:profile"}"#,
            1_000,
        )
        .unwrap();
        assert_eq!(
            tokens,
            ClaudeTokens {
                access_token: "at-new".into(),
                expires_at_ms: 1_000 + 3_600_000,
                refresh_token: Some("rt-new".into()),
                scopes: Some(vec!["user:inference".into(), "user:profile".into()]),
            }
        );
        let kept = resolve(StatusCode::OK, r#"{"access_token":"at","expires_in":60}"#, 0).unwrap();
        assert_eq!(kept.refresh_token, None);
        assert_eq!(kept.scopes, None);
        assert_eq!(
            resolve(StatusCode::OK, r#"{"expires_in":60}"#, 0),
            Err(RefreshError::Transient("response omitted access_token".into()))
        );
        assert_eq!(
            resolve(StatusCode::OK, r#"{"access_token":"at"}"#, 0),
            Err(RefreshError::Transient("response omitted expires_in".into()))
        );
    }

    #[test]
    fn verdicts() {
        let dead = resolve(StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant","error_description":"revoked"}"#, 0).unwrap_err();
        assert_eq!(
            dead,
            RefreshError::Terminal { code: "invalid_grant".into(), message: Some("revoked".into()), memorable: true }
        );
        let client = resolve(StatusCode::UNAUTHORIZED, r#"{"error":"invalid_client"}"#, 0).unwrap_err();
        assert!(client.is_terminal() && !client.is_memorable());
        let bare = resolve(StatusCode::FORBIDDEN, "{}", 0).unwrap_err();
        assert_eq!(bare, RefreshError::Terminal { code: "http_403".into(), message: None, memorable: false });
        assert_eq!(resolve(StatusCode::TOO_MANY_REQUESTS, r#"{"error":"invalid_grant"}"#, 0).unwrap_err(), RefreshError::Transient("invalid_grant: (HTTP 429 Too Many Requests)".into()));
        assert!(!resolve(StatusCode::REQUEST_TIMEOUT, "{}", 0).unwrap_err().is_terminal());
        assert!(!resolve(StatusCode::BAD_GATEWAY, "{}", 0).unwrap_err().is_terminal());
        let garbage = resolve(StatusCode::INTERNAL_SERVER_ERROR, "<html>secret</html>", 0).unwrap_err();
        assert!(!garbage.is_terminal());
        assert!(!garbage.to_string().contains("secret"));
    }

    #[test]
    fn apply_to_rewrites_the_oauth_block_only() {
        let mut credential = ClaudeCredential::from_value(json!({
            OAUTH_KEY: {"accessToken": "old", "refreshToken": "rt-old", "expiresAt": 1, "scopes": ["user:inference"], "subscriptionType": "max"},
            "mcpOAuth": {"keep": true}
        }));
        ClaudeTokens { access_token: "new".into(), expires_at_ms: 99, refresh_token: None, scopes: None }.apply_to(&mut credential);
        assert_eq!(credential.access_token(), Some("new"));
        assert_eq!(credential.refresh_token(), Some("rt-old"), "no rotation keeps the token");
        assert_eq!(credential.expires_at_ms(), Some(99));
        assert_eq!(credential.0[OAUTH_KEY]["subscriptionType"], "max");
        ClaudeTokens { access_token: "n2".into(), expires_at_ms: 100, refresh_token: Some("rt-new".into()), scopes: Some(vec!["a".into()]) }.apply_to(&mut credential);
        assert_eq!(credential.refresh_token(), Some("rt-new"));
        assert_eq!(credential.scopes(), vec!["a"]);
        assert_eq!(credential.0["mcpOAuth"]["keep"], true);
    }

    #[test]
    fn constants_and_override() {
        assert_eq!(TOKEN_URL, "https://platform.claude.com/v1/oauth/token");
        assert_eq!(CLIENT_ID, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
        assert_eq!(token_url_from(None), TOKEN_URL);
        assert_eq!(token_url_from(Some(" ")), TOKEN_URL);
        assert_eq!(token_url_from(Some("http://127.0.0.1:1/t")), "http://127.0.0.1:1/t");
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib claude::oauth::`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Token refresh against platform.claude.com (spec §4). The refresh token
//! may rotate; a response without one keeps the presented token.

use serde_json::{Value, json};
use tracing::debug;

pub use crate::codex::oauth::RefreshError;
use crate::model::now_unix;

use super::credentials::{ClaudeCredential, OAUTH_KEY};

pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Claude Code's OAuth client id.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// The token endpoint, or the `CSWITCH_CLAUDE_TOKEN_URL` override (tests).
pub fn token_url() -> String {
    token_url_from(std::env::var("CSWITCH_CLAUDE_TOKEN_URL").ok().as_deref())
}

fn token_url_from(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map_or_else(|| TOKEN_URL.to_string(), str::to_string)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeTokens {
    pub access_token: String,
    pub expires_at_ms: i64,
    pub refresh_token: Option<String>,
    pub scopes: Option<Vec<String>>,
}

impl ClaudeTokens {
    /// Rewrite `claudeAiOauth` in place; other keys of the block (and of the
    /// object) are kept.
    pub fn apply_to(&self, credential: &mut ClaudeCredential) {
        if !credential.0.is_object() {
            credential.0 = json!({});
        }
        let root = credential.0.as_object_mut().expect("object");
        let block = root.entry(OAUTH_KEY).or_insert_with(|| json!({}));
        if !block.is_object() {
            *block = json!({});
        }
        let block = block.as_object_mut().expect("object");
        block.insert("accessToken".into(), json!(self.access_token));
        block.insert("expiresAt".into(), json!(self.expires_at_ms));
        if let Some(refresh) = &self.refresh_token {
            block.insert("refreshToken".into(), json!(refresh));
        }
        if let Some(scopes) = &self.scopes {
            block.insert("scopes".into(), json!(scopes));
        }
    }
}

pub async fn refresh(client: &reqwest::Client, refresh_token: &str) -> Result<ClaudeTokens, RefreshError> {
    let url = token_url();
    debug!("sending Claude token refresh request to {url}");
    let response = client
        .post(&url)
        .json(&json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|err| RefreshError::Transient(format!("request failed: {err}")))?;
    let status = response.status();
    debug!("Claude token refresh response: HTTP {status}");
    let body = response.text().await.map_err(|err| {
        RefreshError::Transient(format!("unreadable response (HTTP {status}): {err}"))
    })?;
    resolve(status, &body, now_unix() * 1000)
}

/// Any 4xx but 429/408 rejected the grant; `invalid_grant` is worth remembering.
fn resolve(status: reqwest::StatusCode, body: &str, now_ms: i64) -> Result<ClaudeTokens, RefreshError> {
    let terminal = status.is_client_error()
        && !matches!(status, reqwest::StatusCode::TOO_MANY_REQUESTS | reqwest::StatusCode::REQUEST_TIMEOUT);
    let wire: Value = serde_json::from_str(body).map_err(|err| {
        RefreshError::Transient(format!("unparseable response (HTTP {status}): {err}"))
    })?;
    if let Some(code) = wire.get("error").and_then(Value::as_str) {
        let message = wire.get("error_description").and_then(Value::as_str).map(str::to_string);
        if terminal {
            return Err(RefreshError::Terminal {
                code: code.to_string(),
                memorable: code == "invalid_grant",
                message,
            });
        }
        return Err(RefreshError::Transient(format!(
            "{code}: {} (HTTP {status})",
            message.unwrap_or_default()
        )));
    }
    if !status.is_success() {
        let code = format!("http_{}", status.as_u16());
        if terminal {
            return Err(RefreshError::Terminal { code, message: None, memorable: false });
        }
        return Err(RefreshError::Transient(format!("HTTP {status}")));
    }
    let access_token = wire
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| RefreshError::Transient("response omitted access_token".into()))?;
    let expires_in = wire
        .get("expires_in")
        .and_then(Value::as_i64)
        .ok_or_else(|| RefreshError::Transient("response omitted expires_in".into()))?;
    Ok(ClaudeTokens {
        access_token: access_token.to_string(),
        expires_at_ms: now_ms + expires_in * 1000,
        refresh_token: wire.get("refresh_token").and_then(Value::as_str).map(str::to_string),
        scopes: wire
            .get("scope")
            .and_then(Value::as_str)
            .map(|s| s.split_whitespace().map(str::to_string).collect()),
    })
}
```

`RefreshError::Terminal`'s fields are public, so the struct literal compiles; `is_terminal()` / `is_memorable()` come with the re-export.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib claude::oauth:: && cargo clippy --all-targets -- -D warnings`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/claude/oauth.rs
git commit -m "feat(claude): refresh Claude OAuth tokens against platform.claude.com"
```

---

### Task 10: `claude::usage` — the usage client and normalization

**Files:**
- Create: `src/claude/usage.rs` (replacing the stub)
- Modify: `src/codex/usage.rs` (`build_client_with_agent`, `pub(crate) transport_error`)
- Test: `src/claude/usage.rs`, `src/codex/usage.rs`

**Interfaces:**
- Produces: `USAGE_URL`, `BETA_HEADER`, `usage_url()`, `user_agent()`, `build_client(proxy) -> Result<reqwest::Client>`, `async fn get_usage(&Client, bearer) -> Result<NormalizedUsage, FetchError>`, `parse_usage(&Value) -> Result<NormalizedUsage, String>`; `codex::usage::build_client_with_agent(proxy, agent)`.
- Consumes: `codex::usage::{FetchError, retry_after_hint, transport_error}`, `model::{NormalizedUsage, Spend, ScopedWindow, WindowUsage}`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> Value {
        json!({
            "five_hour": {"utilization": 22.0, "resets_at": "2026-10-08T15:00:00Z"},
            "seven_day": {"utilization": 61, "resets_at": null},
            "seven_day_opus": null,
            "extra_usage": {"is_enabled": true, "used_credits": 72900, "monthly_limit": 500000, "utilization": 14.58, "currency": "USD"},
            "limits": [
                {"kind": "weekly_scoped", "group": "weekly", "percent": 100, "resets_at": "2026-10-10T00:00:00Z", "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}, "is_active": true},
                {"kind": "session", "percent": 5, "scope": null},
                {"kind": "weekly_scoped", "percent": "7", "scope": {"model": {"display_name": "Opus"}}},
                {"kind": "weekly_scoped", "percent": 3, "scope": {"model": {"display_name": ""}}}
            ]
        })
    }

    #[test]
    fn full_body_normalizes() {
        let usage = parse_usage(&body()).unwrap();
        assert_eq!(usage.five_hour, Some(WindowUsage { pct: 22.0, resets_at: Some("2026-10-08T15:00:00Z".into()) }));
        assert_eq!(usage.seven_day, Some(WindowUsage { pct: 61.0, resets_at: None }));
        let spend = usage.spend.unwrap();
        assert_eq!((spend.used, spend.limit, spend.pct, spend.currency.as_str()), (729.0, 5000.0, 14.58, "USD"));
        assert_eq!(spend.resets_at, None);
        assert_eq!(usage.scoped, vec![ScopedWindow { name: "Fable".into(), pct: 100.0, resets_at: Some("2026-10-10T00:00:00Z".into()) }]);
        assert!(!usage.limited);
        assert_eq!(usage.credits, None);
        assert_eq!(usage.plan_type, None);
    }

    #[test]
    fn spend_needs_every_field_and_is_disabled_without_the_flag() {
        let mut b = body();
        b["extra_usage"]["is_enabled"] = json!(false);
        assert_eq!(parse_usage(&b).unwrap().spend, None);
        let mut b = body();
        b["extra_usage"]["monthly_limit"] = Value::Null;
        assert_eq!(parse_usage(&b).unwrap().spend, None);
        let mut b = body();
        b["extra_usage"]["currency"] = Value::Null;
        b["extra_usage"]["resets_at"] = json!("2026-11-01T00:00:00Z");
        let spend = parse_usage(&b).unwrap().spend.unwrap();
        assert_eq!(spend.currency, "USD");
        assert_eq!(spend.resets_at.as_deref(), Some("2026-11-01T00:00:00Z"));
    }

    #[test]
    fn bodies_without_windows_are_rejected() {
        assert_eq!(parse_usage(&json!({"five_hour": null, "seven_day": null})).unwrap_err(), "usage response missing recognized quota fields");
        assert!(parse_usage(&json!({"five_hour": {"utilization": "x"}})).is_err());
        assert!(parse_usage(&json!({"limits": [{"percent": 1, "scope": {"model": {"display_name": "Fable"}}}]})).is_ok());
    }

    #[test]
    fn client_and_urls() {
        assert_eq!(USAGE_URL, "https://api.anthropic.com/api/oauth/usage");
        assert_eq!(BETA_HEADER, "oauth-2025-04-20");
        assert_eq!(user_agent(), format!("cswitch/{}", crate::VERSION));
        assert!(build_client(None).is_ok());
        assert_eq!(usage_url_from(None), USAGE_URL);
        assert_eq!(usage_url_from(Some("http://127.0.0.1:1/u")), "http://127.0.0.1:1/u");
    }
}
```

In `src/codex/usage.rs` tests (`client_builder_and_helpers`) add `assert!(build_client_with_agent(None, "x/1").is_ok());`.

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib claude::usage:: codex::usage::`
Expected: compile errors.

- [ ] **Step 3: Implement**

`src/codex/usage.rs`: rename the body of `build_client` into

```rust
/// HTTP client with the given user agent (30 s connect / 60 s total, rustls
/// with the OS trust store plus bundled roots; proxy from the argument,
/// `CSWITCH_PROXY`, then reqwest's own environment handling).
pub fn build_client_with_agent(proxy: Option<&str>, agent: &str) -> Result<reqwest::Client> {
    // … the existing body, with `.user_agent(agent)` instead of `.user_agent(user_agent())`
}

/// The Codex client: Codex's own user agent.
pub fn build_client(proxy: Option<&str>) -> Result<reqwest::Client> {
    build_client_with_agent(proxy, &user_agent())
}
```

and change `fn transport_error` to `pub(crate) fn transport_error`.

`src/claude/usage.rs`:

```rust
//! The Anthropic usage API and its normalization (spec §4, §8). No refresh
//! happens here: the collector decides whether a Claude credential may be
//! refreshed (never the active one).

use serde_json::Value;
use tracing::debug;

pub use crate::codex::usage::FetchError;
use crate::codex::usage::{build_client_with_agent, retry_after_hint, transport_error};
use crate::errors::Result;
use crate::model::{NormalizedUsage, ScopedWindow, Spend, WindowUsage};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const BETA_HEADER: &str = "oauth-2025-04-20";

pub fn user_agent() -> String {
    format!("cswitch/{}", crate::VERSION)
}

/// The usage endpoint, or the `CSWITCH_CLAUDE_USAGE_URL` override (tests).
pub fn usage_url() -> String {
    usage_url_from(std::env::var("CSWITCH_CLAUDE_USAGE_URL").ok().as_deref())
}

fn usage_url_from(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map_or_else(|| USAGE_URL.to_string(), str::to_string)
}

pub fn build_client(proxy: Option<&str>) -> Result<reqwest::Client> {
    build_client_with_agent(proxy, &user_agent())
}

/// `GET` the usage of one access token.
pub async fn get_usage(client: &reqwest::Client, bearer: &str) -> std::result::Result<NormalizedUsage, FetchError> {
    let response = client
        .get(usage_url())
        .bearer_auth(bearer)
        .header("anthropic-beta", BETA_HEADER)
        .send()
        .await
        .map_err(transport_error)?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.map_err(transport_error)?;
    debug!("Claude usage API: HTTP {status}, {} bytes", body.len());
    if status.is_success() {
        let value: Value = serde_json::from_slice(&body)
            .map_err(|err| FetchError::BadResponse(format!("invalid JSON (HTTP {status}): {err}")))?;
        return parse_usage(&value).map_err(FetchError::BadResponse);
    }
    let retry_after = (status == reqwest::StatusCode::TOO_MANY_REQUESTS)
        .then(|| retry_after_hint(&headers, &body))
        .flatten();
    Err(FetchError::Http { status: status.as_u16(), retry_after })
}

fn window(value: Option<&Value>) -> std::result::Result<Option<WindowUsage>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(raw) = value.get("utilization") else {
        return Ok(None);
    };
    let pct = raw
        .as_f64()
        .ok_or_else(|| format!("utilization is not a number: {raw}"))?;
    Ok(Some(WindowUsage {
        pct,
        resets_at: value
            .get("resets_at")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }))
}

/// `extra_usage` → `spend` only when enabled and complete; credits are cents.
fn spend(value: Option<&Value>) -> Option<Spend> {
    let extra = value?.as_object()?;
    if !extra.get("is_enabled").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let used = extra.get("used_credits")?.as_f64()?;
    let limit = extra.get("monthly_limit")?.as_f64()?;
    let pct = extra.get("utilization")?.as_f64()?;
    Some(Spend {
        used: used / 100.0,
        limit: limit / 100.0,
        pct,
        currency: extra
            .get("currency")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .unwrap_or("USD")
            .to_string(),
        resets_at: extra
            .get("resets_at")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// `limits[]` entries with a model display name and a numeric percent.
fn scoped(value: Option<&Value>) -> Vec<ScopedWindow> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item.pointer("/scope/model/display_name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            let pct = item.get("percent")?.as_f64()?;
            Some(ScopedWindow {
                name: name.to_string(),
                pct,
                resets_at: item
                    .get("resets_at")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            })
        })
        .collect()
}

/// Normalize a usage body (spec §8). A body with no window at all is not a measurement.
pub fn parse_usage(body: &Value) -> std::result::Result<NormalizedUsage, String> {
    let usage = NormalizedUsage {
        five_hour: window(body.get("five_hour"))?,
        seven_day: window(body.get("seven_day"))?,
        scoped: scoped(body.get("limits")),
        credits: None,
        limited: false,
        plan_type: None,
        reset_credits: None,
        spend: spend(body.get("extra_usage")),
    };
    if usage.five_hour.is_none() && usage.seven_day.is_none() && usage.scoped.is_empty() {
        return Err("usage response missing recognized quota fields".to_string());
    }
    Ok(usage)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib claude::usage:: codex::usage:: && cargo clippy --all-targets -- -D warnings`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add src/claude/usage.rs src/codex/usage.rs
git commit -m "feat(claude): fetch and normalize Anthropic usage (5h, 7d, spend, per-model windows)"
```

---

### Task 11: The collector learns providers

**Files:**
- Modify: `src/store/credentials.rs` (`fingerprint`), `src/collect.rs`, `src/autoswitch.rs` (the `CollectOptions` literals only), `src/switcher.rs` (the `CollectOptions` literals only)
- Test: `src/store/credentials.rs`, `src/collect.rs`

**Interfaces:**
- Produces: `CollectOptions.actives: Vec<u32>` (replaces `active: Option<u32>`); `collect::live_login_for(&Store, &Roster, Provider) -> CurrentAccount`; `collect::refresh_slot` handles Claude slots; `store::credentials::fingerprint` reads `/claudeAiOauth/refreshToken` too.
- Consumes: Tasks 4–10.

- [ ] **Step 1: Write the failing tests**

`src/store/credentials.rs` tests, in `fingerprint_prefers_the_refresh_token`:

```rust
        let claude = json!({"claudeAiOauth": {"accessToken": "a", "refreshToken": "crt-1"}, "oauthAccount": {}});
        let expected = hex::encode(Sha256::digest(b"crt-1"));
        assert_eq!(fingerprint(&claude), Some(format!("sha256:{expected}")));
```

`src/collect.rs` tests (same module as the existing ones):

```rust
    fn claude_slot(store: &Store, slot: u32, email: &str, access: &str, refresh: Option<&str>, expires_at_ms: i64) {
        use crate::claude::credentials::{ClaudeCredential, OauthAccount, SlotFile};
        let mut inner = json!({"accessToken": access, "expiresAt": expires_at_ms, "scopes": ["user:inference"]});
        if let Some(refresh) = refresh {
            inner["refreshToken"] = json!(refresh);
        }
        let credential = ClaudeCredential::from_value(json!({"claudeAiOauth": inner}));
        let account = OauthAccount(json!({"emailAddress": email, "organizationUuid": "org", "organizationName": "Acme", "accountUuid": "u"}));
        credentials::write(store, slot, &SlotFile::new(&credential, account).to_value()).unwrap();
    }

    fn claude_record(slot_email: &str) -> AccountRecord {
        let mut record = AccountRecord::new(slot_email);
        record.provider = crate::provider::Provider::Claude;
        record.organization_uuid = "org".into();
        record
    }

    fn write_claude_live(store: &Store, email: &str, access: &str, refresh: &str) {
        let paths = &store.paths;
        std::fs::create_dir_all(&paths.claude_home).unwrap();
        std::fs::write(
            paths.claude_credentials_file(),
            json!({"claudeAiOauth": {"accessToken": access, "refreshToken": refresh, "expiresAt": FAR * 1000}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            paths.claude_global_config_file(),
            json!({"oauthAccount": {"emailAddress": email, "organizationUuid": "org"}}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn claude_live_login_resolves_by_oauth_account() {
        use crate::provider::Provider;
        let (_dir, store) = temp_store();
        let mut roster = roster_with(&[(1, "a1")]);
        roster.add_record(2, claude_record("c@example.com"));
        assert_eq!(live_login_for(&store, &roster, Provider::Claude), CurrentAccount::NoLogin);
        write_claude_live(&store, "C@example.com", "cat", "crt");
        assert_eq!(
            live_login_for(&store, &roster, Provider::Claude),
            CurrentAccount::Managed { slot: 2, email: "c@example.com".into(), api_key: false }
        );
        write_claude_live(&store, "other@example.com", "cat", "crt");
        assert_eq!(
            live_login_for(&store, &roster, Provider::Claude),
            CurrentAccount::Unmanaged { email: "other@example.com".into() }
        );
        assert_eq!(live_login_for(&store, &roster, Provider::Codex), CurrentAccount::NoLogin, "Codex is untouched");
    }

    #[test]
    fn claude_sentinels_in_a_store_only_pass() {
        let (_dir, store) = temp_store();
        let mut roster = roster_with(&[(1, "a1")]);
        roster.add_record(2, claude_record("c@example.com"));
        roster.add_record(3, claude_record("d@example.com"));
        roster.add_record(4, claude_record("e@example.com"));
        roster.record_mut(4).unwrap().kind = Some(AccountKind::ApiKey);
        claude_slot(&store, 2, "c@example.com", "cat", Some("crt"), FAR * 1000);
        // 3 has no slot file; 4 is a managed key.
        {
            use crate::claude::credentials::{ClaudeCredential, OauthAccount, SlotFile};
            let key = SlotFile::new(&ClaudeCredential::managed_key("sk-ant-api03-k"), OauthAccount::synthesized("e@example.com"));
            credentials::write(&store, 4, &key.to_value()).unwrap();
        }
        let collected = run_pass(&store, &roster, opts(CollectMode::StoreOnly, &[], &[2, 3, 4])).unwrap();
        assert_eq!(collected.entries[&2].sentinel, None);
        assert_eq!(collected.entries[&3].sentinel, Some(UsageSentinel::NoCredentials));
        assert_eq!(collected.entries[&4].sentinel, Some(UsageSentinel::ApiKey));

        // The active Claude slot reads the live login; an expired one idles.
        write_claude_live(&store, "c@example.com", "cat-live", "crt-live");
        let path = store.paths.claude_credentials_file();
        std::fs::write(&path, json!({"claudeAiOauth": {"accessToken": "cat-live", "refreshToken": "crt-live", "expiresAt": 1}}).to_string()).unwrap();
        let collected = run_pass(&store, &roster, opts(CollectMode::StoreOnly, &[2], &[])).unwrap();
        assert_eq!(collected.entries[&2].sentinel, Some(UsageSentinel::TokenExpired));
    }

    #[test]
    fn claude_rotation_is_persisted_into_the_slot_only() {
        use crate::claude::credentials::SlotFile;
        use crate::claude::oauth::ClaudeTokens;
        let (_dir, store) = temp_store();
        claude_slot(&store, 2, "c@example.com", "cat", Some("crt-old"), 1);
        let tokens = ClaudeTokens { access_token: "cat-new".into(), expires_at_ms: 9, refresh_token: Some("crt-new".into()), scopes: None };
        persist_claude_rotation(&store, 2, "crt-old", &tokens).unwrap();
        let slot = SlotFile::from_value(&credentials::read(&store, 2).unwrap().unwrap()).unwrap();
        assert_eq!(slot.credential.refresh_token(), Some("crt-new"));
        assert_eq!(slot.credential.access_token(), Some("cat-new"));
        assert_eq!(slot.oauth_account.organization_name(), "Acme", "oauthAccount survives");
        persist_claude_rotation(&store, 2, "crt-someone-else", &tokens).unwrap();
        let slot = SlotFile::from_value(&credentials::read(&store, 2).unwrap().unwrap()).unwrap();
        assert_eq!(slot.credential.refresh_token(), Some("crt-new"), "compare-and-swap");
        assert!(!store.paths.claude_credentials_file().exists(), "never touches the live login");
    }

    #[test]
    fn claude_pass_refreshes_inactive_slots_on_401_and_never_the_active_one() {
        let mock = mock();
        let (_dir, store) = temp_store();
        let (a, b) = (unique("cl"), unique("cl"));
        let mut roster = Roster::empty();
        roster.add_record(1, claude_record(&format!("{a}@example.com")));
        roster.add_record(2, claude_record(&format!("{b}@example.com")));
        write_claude_live(&store, &format!("{a}@example.com"), "cat-stale", "crt-live-a");
        claude_slot(&store, 1, &format!("{a}@example.com"), "cat-stale", Some("crt-live-a"), FAR * 1000);
        claude_slot(&store, 2, &format!("{b}@example.com"), "cat-stale", Some(&format!("crt-live-{b}")), FAR * 1000);
        let collected = run_pass(&store, &roster, opts(CollectMode::Escalation, &[1], &[2])).unwrap();
        assert_eq!(collected.entries[&1].last_good, None, "the active slot is never refreshed");
        assert_eq!(collected.entries[&1].last_error.as_deref(), Some("http-401"));
        assert_eq!(collected.entries[&2].last_good.as_ref().unwrap().five_hour.as_ref().unwrap().pct, 40.0);
        let slot = crate::claude::credentials::SlotFile::from_value(&credentials::read(&store, 2).unwrap().unwrap()).unwrap();
        assert_eq!(slot.credential.refresh_token(), Some("crt-next"), "the rotation landed in the slot file");
        assert_eq!(mock.claude_token_calls(), 1);
        assert_eq!(collected.entries[&2].poll_interval_s, Some(300.0));
    }
```

Change the existing `opts` helper to `fn opts<'a>(mode, actives: &[u32], candidates: &'a [u32]) -> CollectOptions<'a>` with `actives: actives.to_vec()`, and update its existing callers (`Some(1)` → `&[1]`, `None` → `&[]`).

Extend the in-module mock: routes `/claude/usage` (GET; requires the `anthropic-beta` header; bearer `cat-good` → `{"five_hour": {"utilization": 40, "resets_at": "…"}, "seven_day": {"utilization": 55}}`, `cat-stale` → 401) and `/claude/token` (POST; `client_id` must be `9d1c250a-e61b-44d9-88ed-5944d1962f5e`; `crt-live-<x>` → `{"access_token": "cat-good", "expires_in": 3600, "refresh_token": "crt-next"}`; else 400 `{"error": "invalid_grant"}`); record them under paths `/claude/usage` and `/claude/token`; set `CSWITCH_CLAUDE_USAGE_URL` / `CSWITCH_CLAUDE_TOKEN_URL` in `mock()` next to the Codex overrides; add `fn claude_token_calls(&self) -> usize` counting `/claude/token` records.

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib collect:: store::credentials::`
Expected: compile errors.

- [ ] **Step 3: Implement**

`src/store/credentials.rs` `fingerprint`: try both pointers:

```rust
    for pointer in ["/tokens/refresh_token", "/claudeAiOauth/refreshToken"] {
        if let Some(token) = value.pointer(pointer).and_then(Value::as_str).filter(|t| !t.is_empty()) {
            return Some(format!("sha256:{}", hex::encode(Sha256::digest(token.as_bytes()))));
        }
    }
```

`src/collect.rs`:

1. Imports:

```rust
use crate::claude::credentials::{ClaudeCredential, CredentialKind, SlotFile};
use crate::claude::keychain::SystemSecurity;
use crate::claude::live::ClaudeLive;
use crate::claude::oauth::ClaudeTokens;
use crate::provider::Provider;
```

2. `CollectOptions`: replace `pub active: Option<u32>` with

```rust
    /// The slots whose credentials are a live login (at most one per provider).
    pub actives: Vec<u32>,
```

and add `impl CollectOptions<'_> { fn is_active(&self, slot: u32) -> bool { self.actives.contains(&slot) } }`. Replace every `opts.active == Some(slot)` / `opts.active == Some(*slot)` with `opts.is_active(slot)`; in `run_pass` build `slots` from `opts.actives.iter().copied()` then the candidates; in `reserve` iterate `for active in opts.actives.iter().copied()` for the active reservation and filter candidates with `!opts.is_active(*slot)`.

3. The presented credential:

```rust
/// The credential a pass presents for a slot, by provider.
#[derive(Debug, Clone)]
enum Presented {
    Codex(AuthJson),
    Claude(ClaudeCredential),
}

impl Presented {
    fn fingerprint(&self) -> Option<String> {
        match self {
            Self::Codex(auth) => credentials::fingerprint(&auth.0),
            Self::Claude(cred) => cred.fingerprint(),
        }
    }

    fn refresh_token(&self) -> Option<&str> {
        match self {
            Self::Codex(auth) => auth.refresh_token(),
            Self::Claude(cred) => cred.refresh_token(),
        }
    }
}

struct SlotView {
    provider: Provider,
    identity: Option<Identity>,
    api_key_record: bool,
    stored: Option<Presented>,
    live: Option<Presented>,
    /// The active Claude slot's live read failed on the Keychain.
    keychain_unavailable: bool,
}
```

`SlotView::load`:

```rust
    fn load(store: &Store, roster: &Roster, slot: u32, is_active: bool) -> Self {
        let record = roster.record(slot);
        let provider = record.map(|r| r.provider).unwrap_or_default();
        let identity = record.map(|r| r.identity());
        let api_key_record = record.is_some_and(|r| r.is_api_key());
        let raw = credentials::read(store, slot).ok().flatten();
        let (stored, live, keychain_unavailable) = match provider {
            Provider::Codex => (
                raw.map(|v| Presented::Codex(AuthJson::from_value(v))),
                is_active
                    .then(|| AuthJson::read(&store.paths.live_auth_file()).ok().flatten())
                    .flatten()
                    .map(Presented::Codex),
                false,
            ),
            Provider::Claude => {
                let stored = raw
                    .and_then(|v| SlotFile::from_value(&v).ok())
                    .map(|s| Presented::Claude(s.credential));
                let (live, unavailable) = if is_active {
                    match ClaudeLive::new(&store.paths, &SystemSecurity).read() {
                        Ok(login) => (login.credential.map(Presented::Claude), login.keychain_unavailable),
                        Err(_) => (None, false),
                    }
                } else {
                    (None, false)
                };
                (stored, live, unavailable)
            }
        };
        Self { provider, identity, api_key_record, stored, live, keychain_unavailable }
    }

    fn presented(&self) -> Option<&Presented> {
        self.live.as_ref().or(self.stored.as_ref())
    }

    /// Something a usage request can be made with.
    fn fetchable(&self) -> bool {
        if self.api_key_record || self.identity.is_none() {
            return false;
        }
        match self.presented() {
            Some(Presented::Codex(auth)) => auth.kind() == AuthKind::ChatGpt,
            Some(Presented::Claude(cred)) => matches!(cred.kind(), CredentialKind::OAuth | CredentialKind::SetupToken),
            None => false,
        }
    }

    fn fingerprint(&self) -> Option<String> {
        self.presented().and_then(Presented::fingerprint)
    }

    /// The access token is past its expiry (or absent).
    fn access_expired(&self, now: f64) -> bool {
        match self.presented() {
            Some(Presented::Codex(auth)) => match auth.access_token() {
                None => true,
                Some(token) => token_expires_at(token).is_some_and(|exp| exp as f64 <= now),
            },
            Some(Presented::Claude(cred)) => cred.access_token().is_none() || cred.is_expired((now * 1000.0) as i64),
            None => true,
        }
    }
```

`derive_sentinel`:

```rust
    let Some(presented) = view.presented() else {
        if view.keychain_unavailable {
            return Some(UsageSentinel::KeychainUnavailable);
        }
        return Some(UsageSentinel::NoCredentials);
    };
    let api_key = match presented {
        Presented::Codex(auth) => auth.kind() == AuthKind::ApiKey,
        Presented::Claude(cred) => cred.kind() == CredentialKind::ApiKey,
    };
    if view.api_key_record || api_key {
        return Some(UsageSentinel::ApiKey);
    }
    if !view.fetchable() {
        return Some(UsageSentinel::NoCredentials);
    }
    // … the dead-token and TokenExpired checks stay as they are …
```

4. Live-login resolution:

```rust
/// The slot whose credentials are `provider`'s live login.
pub fn live_login_for(store: &Store, roster: &Roster, provider: Provider) -> CurrentAccount {
    match provider {
        Provider::Codex => live_login(store, roster),
        Provider::Claude => claude_live_login(store, roster),
    }
}

fn claude_live_login(store: &Store, roster: &Roster) -> CurrentAccount {
    let Ok(login) = ClaudeLive::new(&store.paths, &SystemSecurity).read() else {
        return CurrentAccount::NoLogin;
    };
    let Some(credential) = &login.credential else {
        return CurrentAccount::NoLogin;
    };
    let email_of = |slot: u32| roster.record(slot).map(|r| r.email.clone()).unwrap_or_default();
    match credential.kind() {
        CredentialKind::ApiKey => {
            let key = credential.api_key();
            let matched = roster.slots_of(Provider::Claude).into_iter().find(|slot| {
                roster.record(*slot).is_some_and(|r| r.is_api_key())
                    && credentials::read(store, *slot)
                        .ok()
                        .flatten()
                        .and_then(|v| SlotFile::from_value(&v).ok())
                        .is_some_and(|s| s.credential.api_key() == key)
            });
            match matched {
                Some(slot) => CurrentAccount::Managed { slot, email: email_of(slot), api_key: true },
                None => CurrentAccount::Unmanaged { email: String::new() },
            }
        }
        CredentialKind::OAuth | CredentialKind::SetupToken => match login.identity() {
            Some(identity) => match roster.find_slot(Provider::Claude, &identity) {
                Some(slot) => CurrentAccount::Managed { slot, email: email_of(slot), api_key: false },
                None => CurrentAccount::Unmanaged { email: identity.email },
            },
            None => CurrentAccount::Unmanaged {
                email: login.oauth_account.as_ref().map(|a| a.email_address().to_lowercase()).unwrap_or_default(),
            },
        },
        CredentialKind::Unknown => CurrentAccount::NoLogin,
    }
}
```

5. Fetching. Replace the Codex-only `FetchOutcome` plumbing with a provider-neutral outcome:

```rust
enum Rotation {
    Codex(RefreshedTokens),
    Claude(ClaudeTokens),
}

struct Outcome {
    rotation: Option<Rotation>,
    result: std::result::Result<NormalizedUsage, FetchError>,
}

type Fetched = (u32, Presented, Outcome);
```

`fetch_all_blocking` builds two clients (`build_client(None)?` for Codex and `crate::claude::usage::build_client(None)?` for Claude) and spawns per job:

```rust
            let outcome = match &presented {
                Presented::Codex(auth) => {
                    let out = fetch_usage(&codex_client, auth).await;
                    Outcome { rotation: out.refreshed.map(Rotation::Codex), result: out.result }
                }
                Presented::Claude(cred) => fetch_claude(&claude_client, cred, is_active).await,
            };
```

(jobs carry `(slot, Presented, is_active)`; `run_pass` passes `opts.is_active(slot)`.)

```rust
/// Spec §8: refresh only an inactive credential (proactively within the
/// 5-minute buffer, reactively on 401/403); the active one belongs to
/// Claude Code and a 401 is reported as such.
async fn fetch_claude(client: &reqwest::Client, cred: &ClaudeCredential, is_active: bool) -> Outcome {
    use crate::claude::oauth::refresh;
    use crate::claude::usage::get_usage;
    let now_ms = now_unix() * 1000;
    let refreshable = (!is_active).then(|| cred.refresh_token()).flatten();
    if let Some(rt) = refreshable
        && cred.is_expiring(now_ms)
    {
        match refresh(client, rt).await {
            Ok(tokens) => {
                let result = get_usage(client, &tokens.access_token).await;
                return Outcome { rotation: Some(Rotation::Claude(tokens)), result };
            }
            Err(err) if err.is_terminal() => {
                return Outcome { rotation: None, result: Err(FetchError::Auth(err)) };
            }
            Err(err) => tracing::warn!("proactive Claude token refresh failed: {err}"),
        }
    }
    let Some(bearer) = cred.access_token() else {
        return Outcome { rotation: None, result: Err(FetchError::NoAccessToken) };
    };
    let first = get_usage(client, bearer).await;
    let (Err(FetchError::Http { status: 401 | 403, .. }), Some(rt)) = (&first, refreshable) else {
        return Outcome { rotation: None, result: first };
    };
    match refresh(client, rt).await {
        Ok(tokens) => {
            let result = get_usage(client, &tokens.access_token).await;
            Outcome { rotation: Some(Rotation::Claude(tokens)), result }
        }
        Err(err) => Outcome { rotation: None, result: Err(FetchError::Auth(err)) },
    }
}
```

In `run_pass`'s result loop, persist by rotation kind:

```rust
            if let Some(rotation) = &outcome.rotation
                && let Some(rt) = presented.refresh_token()
            {
                let saved = match rotation {
                    Rotation::Codex(tokens) => persist_rotation(store, slot, is_active, rt, tokens),
                    Rotation::Claude(tokens) => persist_claude_rotation(store, slot, rt, tokens),
                };
                if let Err(err) = saved {
                    failures.push(format!("[Account-{slot}] token refresh succeeded but the rotated credentials could not be saved: {err}"));
                }
            }
```

and use `outcome.result` where `outcome.result` was used before; the failure branch's `struck_fp` becomes `permanent_auth.then(|| presented.fingerprint()).flatten()`.

```rust
/// Persist a Claude rotation into the slot file with compare-and-swap on the
/// presented refresh token. The live login is Claude Code's and never touched.
fn persist_claude_rotation(store: &Store, slot: u32, presented_refresh_token: &str, tokens: &ClaudeTokens) -> Result<()> {
    let _lock = store.lock()?;
    let Some(value) = credentials::read(store, slot)? else {
        return Ok(());
    };
    let mut file = SlotFile::from_value(&value)?;
    if file.credential.refresh_token() != Some(presented_refresh_token) {
        return Ok(());
    }
    tokens.apply_to(&mut file.credential);
    credentials::write(store, slot, &file.to_value())
}
```

6. `refresh_slot`: at the top, after `record` is found, branch on the provider:

```rust
    if record.provider == Provider::Claude {
        return refresh_claude_slot(store, roster, slot, force);
    }
```

```rust
fn refresh_claude_slot(store: &Store, roster: &Roster, slot: u32, force: bool) -> RefreshStatus {
    let record = roster.record(slot).expect("checked by the caller");
    if record.is_api_key() {
        return RefreshStatus::NotNeeded;
    }
    let file = match credentials::read(store, slot) {
        Ok(Some(value)) => match SlotFile::from_value(&value) {
            Ok(file) => file,
            Err(err) => return RefreshStatus::Transient(err.to_string()),
        },
        Ok(None) => return RefreshStatus::Transient(format!("Account-{slot} has no stored credentials")),
        Err(err) => return RefreshStatus::Transient(err.to_string()),
    };
    match file.credential.kind() {
        CredentialKind::ApiKey | CredentialKind::SetupToken => return RefreshStatus::NotNeeded,
        CredentialKind::Unknown => return RefreshStatus::NoRefreshToken,
        CredentialKind::OAuth => {}
    }
    if claude_live_login(store, roster).slot() == Some(slot) {
        // Claude Code owns the active login; spec §8.
        return RefreshStatus::NotNeeded;
    }
    if !force && !file.credential.is_expiring(now_unix() * 1000) {
        return RefreshStatus::NotNeeded;
    }
    let Some(refresh_token) = file.credential.refresh_token().map(str::to_string) else {
        return RefreshStatus::NoRefreshToken;
    };
    let result = run_blocking(|| async {
        let client = crate::claude::usage::build_client(None).map_err(|err| RefreshError::Transient(err.to_string()))?;
        crate::claude::oauth::refresh(&client, &refresh_token).await
    });
    match result {
        Ok(tokens) => {
            if let Err(err) = persist_claude_rotation(store, slot, &refresh_token, &tokens) {
                return RefreshStatus::Transient(format!("token refresh succeeded but the rotated credentials could not be saved: {err}"));
            }
            let _ = UsageStore::new(&store.paths).clear_dead_token(&[slot]);
            RefreshStatus::Ok
        }
        Err(RefreshError::Terminal { code, memorable, .. }) => {
            let strike = FetchRecord::Failure {
                error: "auth".to_string(),
                retry_after: None,
                permanent_auth: true,
                struck_fp: file.credential.fingerprint(),
            };
            if let Err(err) = UsageStore::new(&store.paths).record(slot, &record.identity(), strike, now_unix() as f64) {
                tracing::warn!("could not record the dead-token strike for account {slot}: {err}");
            }
            RefreshStatus::Terminal { code, memorable }
        }
        Err(RefreshError::Transient(detail)) => RefreshStatus::Transient(detail),
    }
}

/// Run a future on a private runtime; from inside another runtime the work
/// moves to a helper thread (the same rule as `run_refresh`).
fn run_blocking<T: Send + 'static, F>(make: F) -> T
where
    F: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>> + Send + 'static,
{
    let work = move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        runtime.block_on(make())
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::scope(|scope| scope.spawn(work).join().unwrap_or_else(|p| std::panic::resume_unwind(p)));
    }
    work()
}
```

(Write the call as `run_blocking(move || Box::pin(async move { … }))`.)

7. The `CollectOptions` literals: `grep -n "CollectOptions {" src/switcher.rs src/autoswitch.rs` — in `src/switcher.rs` (`collect_for_switch`, `list_snapshot`, `status`) and in `src/autoswitch.rs` change `active: Some(x)` → `actives: vec![x]` and `active,` → `actives: active.into_iter().collect(),`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt`
Expected: green; the four new Claude tests pass against the extended mock.

- [ ] **Step 5: Commit**

```bash
git add src/store/credentials.rs src/collect.rs src/autoswitch.rs src/switcher.rs
git commit -m "feat(collect): fetch, refresh and persist Claude accounts alongside Codex in one usage pass"
```

---

### Task 12: `add` captures both logins; `add-token` routes by prefix

**Files:**
- Modify: `src/switcher.rs`, `src/cli/accounts.rs`, `src/cli/list.rs` (`first_run`), `src/tui/worker.rs` (`AddCurrent`)
- Test: `src/switcher.rs`

**Interfaces:**
- Produces: `Switcher::add_accounts(provider: Option<Provider>, slot, alias) -> Result<Vec<(Provider, AddOutcome)>>`; `Switcher::add_account(provider: Provider, slot, alias) -> Result<AddOutcome>` (the single-provider form; callers that passed `(slot, alias)` now pass `(Provider::Codex, slot, alias)`); `Switcher::current_account_for(Provider) -> Result<CurrentAccount>`; private `claude_slot_of_live`, `claude_slot_of_key`; `announce_placement(…, suffix: Option<&str>, what)`.
- Consumes: Tasks 6, 7, 11.

- [ ] **Step 1: Write the failing tests**

Extend the `Fixture` in `src/switcher.rs` tests:

```rust
        fn write_claude_live(&self, email: &str, org: &str, name: &str, refresh: &str) {
            let paths = &self.switcher.store.paths;
            fs::create_dir_all(&paths.claude_home).unwrap();
            fs::write(
                paths.claude_credentials_file(),
                json!({
                    "claudeAiOauth": {"accessToken": format!("cat-{refresh}"), "refreshToken": refresh, "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]},
                    "mcpOAuth": {"srv": {"accessToken": "m"}}
                })
                .to_string(),
            )
            .unwrap();
            fs::write(
                paths.claude_global_config_file(),
                json!({"oauthAccount": {"emailAddress": email, "organizationUuid": org, "organizationName": name, "accountUuid": "u"}, "projects": {"/p": {}}})
                    .to_string(),
            )
            .unwrap();
        }
        fn claude_credentials(&self) -> Value {
            serde_json::from_str(&fs::read_to_string(self.switcher.store.paths.claude_credentials_file()).unwrap()).unwrap()
        }
        fn claude_config(&self) -> Value {
            serde_json::from_str(&fs::read_to_string(self.switcher.store.paths.claude_global_config_file()).unwrap()).unwrap()
        }
        fn credential(&self, slot: u32) -> Value {
            credentials::read(&self.switcher.store, slot).unwrap().unwrap()
        }
```

and the tests:

```rust
    #[test]
    fn add_accounts_captures_both_logins_even_with_the_same_email() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("me@example.com", "acct-1", "rt-codex"));
        fx.write_claude_live("Me@Example.com", "org-1", "Acme", "crt-1");
        let outcomes = fx.switcher.add_accounts(None, None, None).unwrap();
        assert_eq!(
            outcomes,
            vec![(Provider::Codex, AddOutcome::Added { slot: 1 }), (Provider::Claude, AddOutcome::Added { slot: 2 })]
        );
        assert_eq!(
            fx.lines(),
            ["Added Account 1: me@example.com [Plus]", "Added Account 2: me@example.com [Acme]"]
        );
        let roster = fx.roster();
        assert_eq!(roster.record(1).unwrap().provider, Provider::Codex);
        let claude = roster.record(2).unwrap();
        assert_eq!(claude.provider, Provider::Claude);
        assert_eq!(claude.organization_uuid, "org-1");
        assert_eq!(claude.uuid, "u");
        assert_eq!(roster.active_for(Provider::Codex), Some(1));
        assert_eq!(roster.active_for(Provider::Claude), Some(2));
        let stored = fx.credential(2);
        assert_eq!(stored["claudeAiOauth"]["refreshToken"], "crt-1");
        assert_eq!(stored["oauthAccount"]["emailAddress"], "Me@Example.com");
        assert!(stored.get("mcpOAuth").is_none());
    }

    #[test]
    fn add_accounts_reports_when_both_logins_are_already_managed() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        fx.write_claude_live("c@example.com", "org", "", "crt-1");
        fx.switcher.add_accounts(None, None, None).unwrap();
        fx.lines.borrow_mut().clear();
        let outcomes = fx.switcher.add_accounts(None, None, None).unwrap();
        assert_eq!(
            outcomes,
            vec![(Provider::Codex, AddOutcome::Updated { slot: 1 }), (Provider::Claude, AddOutcome::Updated { slot: 2 })]
        );
        assert_eq!(
            fx.lines(),
            [
                "Updated credentials for Account 1 (a@example.com [Plus]).",
                "Updated credentials for Account 2 (c@example.com [personal]).",
                "Both current logins were already managed: Account-1 (codex), Account-2 (claude) — nothing new was added.",
            ]
        );
    }

    #[test]
    fn add_accounts_selector_missing_login_and_flags() {
        let mut fx = fixture();
        let err = fx.switcher.add_accounts(None, None, None).unwrap_err();
        assert_eq!(err.to_string(), "No active Codex or Claude login found. Log in first.");
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        let err = fx.switcher.add_accounts(Some(Provider::Claude), None, None).unwrap_err();
        assert_eq!(err.to_string(), "No active Claude account found. Please log in first.");
        assert_eq!(fx.switcher.add_account(Provider::Codex, Some(3), Some("Work")).unwrap(), AddOutcome::Added { slot: 3 });
        fx.write_claude_live("c@example.com", "org", "", "crt-1");
        let err = fx.switcher.add_accounts(None, None, Some("x")).unwrap_err();
        assert_eq!(err.type_name(), "ValidationError");
        assert!(err.to_string().starts_with("--slot/--alias need a single login"), "{err}");
        assert_eq!(fx.switcher.add_accounts(None, None, None).unwrap().len(), 2);
    }

    #[test]
    fn add_token_routes_by_prefix() {
        let mut fx = fixture();
        assert_eq!(fx.switcher.add_token("sk-ant-api03-key", None, None).unwrap(), AddOutcome::Added { slot: 1 });
        assert_eq!(fx.switcher.add_token("sk-ant-oat01-tok", Some("me@example.com"), None).unwrap(), AddOutcome::Added { slot: 2 });
        assert_eq!(fx.switcher.add_token("sk-openai", None, None).unwrap(), AddOutcome::Added { slot: 3 });
        assert_eq!(
            fx.lines(),
            [
                "Added Account 1: api-key-1@token.local [personal] (from API key)",
                "Added Account 2: me@example.com [personal] (from token)",
                "Added Account 3: api-key-3@token.local [personal] (from API key)",
            ]
        );
        let roster = fx.roster();
        assert_eq!(roster.record(1).unwrap().provider, Provider::Claude);
        assert!(roster.record(1).unwrap().is_api_key());
        assert_eq!(roster.record(2).unwrap().provider, Provider::Claude);
        assert!(!roster.record(2).unwrap().is_api_key(), "a setup-token is an OAuth-shaped login");
        assert_eq!(roster.record(3).unwrap().provider, Provider::Codex);
        assert_eq!(fx.credential(1)["primaryApiKey"], "sk-ant-api03-key");
        assert_eq!(fx.credential(1)["oauthAccount"]["emailAddress"], "api-key-1@token.local");
        assert_eq!(fx.credential(2)["claudeAiOauth"]["accessToken"], "sk-ant-oat01-tok");
        assert_eq!(fx.credential(2)["claudeAiOauth"]["scopes"], json!(["user:inference"]));
        assert_eq!(fx.credential(3)["OPENAI_API_KEY"], "sk-openai");
        fx.lines.borrow_mut().clear();
        assert_eq!(fx.switcher.add_token("sk-ant-oat01-tok2", Some("me@example.com"), None).unwrap(), AddOutcome::Updated { slot: 2 });
        assert_eq!(fx.lines(), ["Updated token for Account 2 (me@example.com [personal])."]);
    }
```

(`use crate::provider::Provider;` in the tests module.)

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib switcher::tests::add_`
Expected: compile errors.

- [ ] **Step 3: Implement**

Imports in `src/switcher.rs`:

```rust
use crate::claude::credentials::{ClaudeCredential, CredentialKind, OAUTH_ACCOUNT_KEY, OauthAccount, SlotFile, looks_like_api_key, looks_like_setup_token};
use crate::claude::keychain::SystemSecurity;
use crate::claude::live::{ClaudeLive, LiveLogin, backup_live as backup_claude_live};
use crate::provider::Provider;
```

Replace `add_account` with:

```rust
/// What `add` found logged in for one provider.
enum Capture {
    Codex(AuthJson),
    Claude(LiveLogin),
}

fn no_login_error(provider: Option<Provider>) -> CswitchError {
    match provider {
        Some(provider) => CswitchError::config(format!(
            "No active {} account found. Please log in first.",
            provider.title()
        )),
        None => CswitchError::config("No active Codex or Claude login found. Log in first."),
    }
}

impl Switcher {
    /// The live login of `provider`, resolved against the roster.
    pub fn current_account_for(&self, provider: Provider) -> Result<CurrentAccount> {
        let roster = roster::read_or_empty(&self.store.paths)?;
        Ok(collect::live_login_for(&self.store, &roster, provider))
    }

    fn capture_live(&self, provider: Provider) -> Result<Option<Capture>> {
        match provider {
            Provider::Codex => Ok(AuthJson::read(&self.store.paths.live_auth_file())?.map(Capture::Codex)),
            Provider::Claude => {
                let login = ClaudeLive::new(&self.store.paths, &SystemSecurity).read()?;
                Ok(login.credential.is_some().then_some(Capture::Claude(login)))
            }
        }
    }

    /// Snapshot the live login(s) into the store: both providers without a
    /// selector, each new login added and each managed one refreshed in place.
    pub fn add_accounts(
        &mut self,
        provider: Option<Provider>,
        slot: Option<i64>,
        alias: Option<&str>,
    ) -> Result<Vec<(Provider, AddOutcome)>> {
        let requested = slot_arg(slot)?;
        let alias = alias
            .map(|a| normalize_alias(a).map_err(|e| CswitchError::validation(e.to_string())))
            .transpose()?;
        let providers: Vec<Provider> = provider.map_or_else(|| Provider::ALL.to_vec(), |p| vec![p]);
        let mut captures = Vec::new();
        for candidate in providers {
            if let Some(capture) = self.capture_live(candidate)? {
                captures.push((candidate, capture));
            }
        }
        if captures.is_empty() {
            return Err(no_login_error(provider));
        }
        if captures.len() > 1 && (requested.is_some() || alias.is_some()) {
            return Err(CswitchError::validation(
                "--slot/--alias need a single login; both a Codex and a Claude login were found. Say which: cswitch add codex … or cswitch add claude …",
            ));
        }
        let _lock = self.store.lock()?;
        let mut outcomes = Vec::new();
        for (candidate, capture) in captures {
            let outcome = match capture {
                Capture::Codex(auth) => self.add_auth(&auth, requested, alias.clone())?,
                Capture::Claude(login) => self.add_claude(&login, requested, alias.clone())?,
            };
            outcomes.push((candidate, outcome));
        }
        if provider.is_none()
            && outcomes.len() == 2
            && outcomes.iter().all(|(_, o)| matches!(o, AddOutcome::Updated { .. }))
        {
            let slot_of = |wanted: Provider| {
                outcomes
                    .iter()
                    .find_map(|(p, o)| match o {
                        AddOutcome::Updated { slot } if *p == wanted => Some(*slot),
                        _ => None,
                    })
                    .unwrap_or_default()
            };
            self.say(Line::dimmed(format!(
                "Both current logins were already managed: Account-{} (codex), Account-{} (claude) — nothing new was added.",
                slot_of(Provider::Codex),
                slot_of(Provider::Claude)
            )));
        }
        Ok(outcomes)
    }

    /// `add` for one provider.
    pub fn add_account(&mut self, provider: Provider, slot: Option<i64>, alias: Option<&str>) -> Result<AddOutcome> {
        Ok(self
            .add_accounts(Some(provider), slot, alias)?
            .into_iter()
            .next()
            .map_or(AddOutcome::Cancelled, |(_, outcome)| outcome))
    }

    /// The managed Claude slot holding this live login.
    fn claude_slot_of_live(&self, roster: &Roster, live: &LiveLogin) -> Option<u32> {
        let credential = live.credential.as_ref()?;
        match credential.kind() {
            CredentialKind::ApiKey => self.claude_slot_of_key(roster, credential.api_key()),
            CredentialKind::OAuth | CredentialKind::SetupToken => {
                live.identity().and_then(|id| roster.find_slot(Provider::Claude, &id))
            }
            CredentialKind::Unknown => None,
        }
    }

    fn claude_slot_of_key(&self, roster: &Roster, key: Option<&str>) -> Option<u32> {
        let key = key?;
        roster.slots_of(Provider::Claude).into_iter().find(|slot| {
            roster.record(*slot).is_some_and(|r| r.is_api_key())
                && credentials::read(&self.store, *slot)
                    .ok()
                    .flatten()
                    .and_then(|v| SlotFile::from_value(&v).ok())
                    .is_some_and(|s| s.credential.api_key() == Some(key))
        })
    }

    /// The caller holds the store lock.
    fn add_claude(&mut self, login: &LiveLogin, requested: Option<u32>, alias: Option<String>) -> Result<AddOutcome> {
        let mut roster = roster::init_if_absent(&self.store.paths)?;
        let credential = login
            .credential
            .as_ref()
            .ok_or_else(|| no_login_error(Some(Provider::Claude)))?;
        let (record, existing, suffix) = match credential.kind() {
            CredentialKind::OAuth | CredentialKind::SetupToken => {
                let account = login.oauth_account.as_ref().ok_or_else(|| {
                    CswitchError::credential_read(
                        "the Claude Code login carries no oauthAccount; log in with Claude Code first",
                    )
                })?;
                let identity = account.identity().ok_or_else(|| {
                    CswitchError::credential_read("the Claude Code login carries no email address")
                })?;
                let mut record = AccountRecord::new(identity.email.clone());
                record.provider = Provider::Claude;
                record.uuid = account.account_uuid();
                record.organization_uuid = identity.account_id.clone();
                record.organization_name = account.organization_name();
                let existing = roster.find_slot(Provider::Claude, &identity);
                (record, existing, None)
            }
            CredentialKind::ApiKey => {
                let existing = self.claude_slot_of_key(&roster, credential.api_key());
                let slot = existing.or(requested).unwrap_or_else(|| roster.next_free_slot());
                let mut record = AccountRecord::new(format!("api-key-{slot}@token.local"));
                record.provider = Provider::Claude;
                record.kind = Some(AccountKind::ApiKey);
                (record, existing, Some("from API key"))
            }
            CredentialKind::Unknown => {
                return Err(CswitchError::credential_read(
                    "the Claude Code login holds neither an OAuth credential nor an API key",
                ));
            }
        };
        let oauth_account = match credential.kind() {
            CredentialKind::ApiKey => OauthAccount::synthesized(&record.email),
            _ => login.oauth_account.clone().expect("checked above"),
        };
        let value = SlotFile::new(credential, oauth_account).to_value();
        let placement = self.place(&mut roster, record, existing, requested, alias, &value)?;
        Ok(self.announce_placement(&roster, placement, suffix, "credentials"))
    }
}
```

`add_auth` keeps its body; its last line becomes `Ok(self.announce_placement(&roster, placement, from_api_key.then_some("from API key"), "credentials"))`. In `place`, capture `let provider = record.provider;` before `roster.add_record(target, record);` and replace `roster.set_active(Some(target));` with `roster.set_active_for(provider, Some(target));`. In `refresh_in_place` replace `roster.set_active(Some(slot));` with:

```rust
        let provider = roster.record(slot).map(|r| r.provider).unwrap_or_default();
        roster.set_active_for(provider, Some(slot));
```

`announce_placement(&mut self, roster, placement, suffix: Option<&str>, what: &str)`: replace the `if from_api_key { … " (from API key)" }` with `if let Some(suffix) = suffix { line = line.push(Style::Plain, format!(" ({suffix})")); }`.

`add_token`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    OpenAiKey,
    ClaudeApiKey,
    SetupToken,
}

impl TokenKind {
    fn detect(token: &str) -> Self {
        if looks_like_api_key(token) {
            Self::ClaudeApiKey
        } else if looks_like_setup_token(token) {
            Self::SetupToken
        } else {
            Self::OpenAiKey
        }
    }
    fn provider(self) -> Provider {
        match self {
            Self::OpenAiKey => Provider::Codex,
            _ => Provider::Claude,
        }
    }
    fn is_api_key(self) -> bool {
        self != Self::SetupToken
    }
    fn email_prefix(self) -> &'static str {
        match self {
            Self::SetupToken => "setup-token",
            _ => "api-key",
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            Self::SetupToken => "from token",
            _ => "from API key",
        }
    }
    fn what(self) -> &'static str {
        match self {
            Self::SetupToken => "token",
            _ => "API key",
        }
    }
}
```

and the body after the email validation:

```rust
        let kind = TokenKind::detect(token);
        let _lock = self.store.lock()?;
        let mut roster = roster::init_if_absent(&self.store.paths)?;
        let email = match email {
            Some(email) => email.to_string(),
            None => format!(
                "{}-{}@token.local",
                kind.email_prefix(),
                requested.unwrap_or_else(|| roster.next_free_slot())
            ),
        };
        let identity = Identity::new(email.clone(), "");
        let existing = roster.find_slot(kind.provider(), &identity);
        if let Some(slot) = existing
            && roster.record(slot).is_some_and(|r| r.is_api_key() != kind.is_api_key())
        {
            return Err(CswitchError::validation(format!(
                "'{email}' already exists as an OAuth account (slot {slot}); cannot add it as an API-key account. Pass a distinct --email."
            )));
        }
        let mut record = AccountRecord::new(email.clone());
        record.provider = kind.provider();
        if kind.is_api_key() {
            record.kind = Some(AccountKind::ApiKey);
        }
        let creds = match kind {
            TokenKind::OpenAiKey => AuthJson::api_key_auth(token).0,
            TokenKind::ClaudeApiKey => {
                SlotFile::new(&ClaudeCredential::managed_key(token), OauthAccount::synthesized(&email)).to_value()
            }
            TokenKind::SetupToken => {
                SlotFile::new(&ClaudeCredential::wrap_setup_token(token), OauthAccount::synthesized(&email)).to_value()
            }
        };
        let placement = self.place(&mut roster, record, existing, requested, None, &creds)?;
        Ok(self.announce_placement(&roster, placement, Some(kind.suffix()), kind.what()))
```

The `starts_with('{')` message becomes `Token must be an API key or setup-token, not a JSON object`; update the assertion in `tests/cli_accounts.rs` if it checks that text (`grep -n "not a JSON object" tests/`).

Callers: `src/cli/accounts.rs` `add(switcher, slot, alias)` → `switcher.add_accounts(None, slot, alias)?` (Task 15 threads the selector through); `src/cli/list.rs` `first_run` → `switcher.add_accounts(None, None, None)?`; `src/switcher.rs` `switch()` unmanaged path → `self.add_account(Provider::Codex, None, None)?` (Task 14 makes it the resolved provider); `src/tui/worker.rs` `Action::AddCurrent` → `switcher.add_accounts(None, None, None)?`; `add_browser_account` is unchanged.

- [ ] **Step 4: Run the tests**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt`
Expected: green (the existing `add_first_second_and_refresh_in_place` integration test keeps its wording because a Codex-only machine has no Claude login).

- [ ] **Step 5: Commit**

```bash
git add src/switcher.rs src/cli/accounts.rs src/cli/list.rs src/tui/worker.rs tests/cli_accounts.rs
git commit -m "feat(add): capture the Codex and Claude logins in one add; route add-token by token prefix"
```

---

### Task 13: Switching to a Claude account

**Files:**
- Modify: `src/switcher.rs`
- Test: `src/switcher.rs`

**Interfaces:**
- Produces: `perform_switch` dispatches on `record.provider`; private `perform_claude_switch`; `SwitchOutcome.provider` set from the record.
- Consumes: Tasks 7, 8, 12.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn switch_to_a_claude_slot_rewrites_the_live_login_and_keeps_siblings() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-codex"));
        fx.write_claude_live("one@example.com", "org-1", "", "crt-1");
        fx.switcher.add_accounts(None, None, None).unwrap();
        fx.write_claude_live("two@example.com", "org-2", "Acme", "crt-2");
        fx.switcher.add_account(Provider::Claude, None, None).unwrap();
        let codex_before = fx.live();
        fx.lines.borrow_mut().clear();

        let report = fx.switcher.switch_to("2", false, true).unwrap().unwrap();
        assert!(report.outcome.switched);
        assert_eq!(report.outcome.provider, Provider::Claude);
        assert_eq!(report.outcome.from.as_ref().unwrap().number, Some(3));
        assert_eq!(report.outcome.to.as_ref().unwrap().email, "one@example.com");
        assert_eq!(report.followup.as_deref(), Some(crate::claude::live::FILE_FOLLOWUP));
        assert!(report.show_list);
        assert_eq!(fx.lines(), ["Switched to Account-2 (one@example.com)"]);

        let creds = fx.claude_credentials();
        assert_eq!(creds["claudeAiOauth"]["refreshToken"], "crt-1");
        assert_eq!(creds["mcpOAuth"]["srv"]["accessToken"], "m", "siblings survive");
        let config = fx.claude_config();
        assert_eq!(config["oauthAccount"]["emailAddress"], "one@example.com");
        assert_eq!(config["oauthAccount"]["organizationUuid"], "org-1");
        assert_eq!(config["projects"]["/p"], json!({}), "other config keys survive");
        assert_eq!(fx.live(), codex_before, "the Codex login is untouched");
        assert_eq!(fx.roster().active_for(Provider::Claude), Some(2));
        assert_eq!(fx.roster().active_for(Provider::Codex), Some(1));
        let backups = fs::read_dir(fx.switcher.store.paths.claude_backups_dir()).unwrap().count();
        assert_eq!(backups, 1);
        let paths = &fx.switcher.store.paths;
        assert!(!paths.claude_refresh_lock_dir().exists() && !paths.claude_legacy_lock_dir().exists() && !paths.claude_config_lock_dir().exists());

        fx.lines.borrow_mut().clear();
        let again = fx.switcher.switch_to("2", false, true).unwrap().unwrap();
        assert_eq!(again.outcome.reason, "already-active");
        assert_eq!(fx.lines()[0], "Already on Account-2 (one@example.com)");
    }

    #[test]
    fn switch_folds_a_rotated_live_claude_login_back_first() {
        let mut fx = fixture();
        fx.write_claude_live("one@example.com", "org-1", "", "crt-1");
        fx.switcher.add_account(Provider::Claude, None, None).unwrap();
        fx.write_claude_live("two@example.com", "org-2", "", "crt-2");
        fx.switcher.add_account(Provider::Claude, None, None).unwrap();
        // Claude Code rotated account two's token while it was live.
        let paths = fx.switcher.store.paths.clone();
        fs::write(
            paths.claude_credentials_file(),
            json!({"claudeAiOauth": {"accessToken": "cat-rotated", "refreshToken": "crt-2b", "expiresAt": 4_102_444_801_000i64}}).to_string(),
        )
        .unwrap();
        fx.switcher.switch_to("1", false, true).unwrap();
        assert_eq!(fx.credential(2)["claudeAiOauth"]["refreshToken"], "crt-2b", "folded back before leaving");
        assert_eq!(fx.claude_credentials()["claudeAiOauth"]["refreshToken"], "crt-1");
        let err = fx.switcher.switch_to("9", false, true).unwrap_err();
        assert_eq!(err.to_string(), "Account-9 does not exist");
    }
```

Add `fn live(&self) -> Value` to the fixture (reads `live_auth_file`).

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib switcher::tests::switch_`
Expected: the first test fails because `perform_switch` reads the Codex store for a Claude slot (`has no stored credentials` / a Codex write).

- [ ] **Step 3: Implement**

At the top of `perform_switch`, after `record` is read:

```rust
        if record.provider == Provider::Claude {
            return self.perform_claude_switch(roster, target, record, strategy, force);
        }
```

Set `provider: Provider::Codex` in both `SwitchOutcome` literals of `perform_switch` (they already carry a `Codex` literal from Task 3; leave them).

```rust
    /// The Claude switch body (spec §7): Claude Code's locks, fold-back, backup,
    /// write across backends, `oauthAccount` splice, follow-up by backend.
    fn perform_claude_switch(
        &mut self,
        mut roster: Roster,
        target: u32,
        record: AccountRecord,
        strategy: &str,
        force: bool,
    ) -> Result<SwitchReport> {
        let stored = credentials::read(&self.store, target)?.ok_or_else(|| {
            CswitchError::switch(format!(
                "Account-{target} has no stored credentials. Re-add with: cswitch add claude --slot {target}"
            ))
        })?;
        let stored = SlotFile::from_value(&stored).map_err(|err| {
            CswitchError::switch(format!(
                "Account-{target}'s stored credentials are unusable ({err}). Re-add with: cswitch add claude --slot {target}"
            ))
        })?;
        let to = AccountRef {
            number: Some(target),
            email: record.email.clone(),
        };
        let mut warnings = Vec::new();
        let live_api = ClaudeLive::new(&self.store.paths, &SystemSecurity);

        let store_lock = self.store.lock()?;
        let claude_locks = crate::claude::locks::acquire(&self.store.paths)?;
        let live = live_api.read()?;
        let live_slot = self.claude_slot_of_live(&roster, &live);
        let from = live.credential.as_ref().map(|_| match live_slot {
            Some(slot) => AccountRef {
                number: Some(slot),
                email: roster
                    .record(slot)
                    .map(|r| r.email.clone())
                    .unwrap_or_else(|| claude_live_email(&live)),
            },
            None => AccountRef {
                number: None,
                email: claude_live_email(&live),
            },
        });
        let same_slot = live_slot == Some(target);
        let live_login_only = live.credential.as_ref().map(ClaudeCredential::oauth_only);
        if same_slot && !force && live_login_only.as_ref() == Some(&stored.credential) {
            return Ok(SwitchReport {
                outcome: SwitchOutcome {
                    switched: false,
                    provider: Provider::Claude,
                    from: from.clone(),
                    to: Some(to),
                    strategy: strategy.to_string(),
                    reason: "already-active".to_string(),
                    message: format!("Already on {}", account_label(target, &record.email)),
                    warnings,
                },
                followup: None,
                show_list: false,
            });
        }
        let mut target_credential = stored.credential.clone();
        if !force && let Some(live_credential) = &live.credential {
            match live_slot {
                Some(slot) => {
                    let kept = credentials::read(&self.store, slot)?.and_then(|v| SlotFile::from_value(&v).ok());
                    let incoming = live_credential.oauth_only();
                    if kept.as_ref().is_none_or(|kept| incoming.is_newer_than(&kept.credential)) {
                        let account = live
                            .oauth_account
                            .clone()
                            .or_else(|| kept.as_ref().map(|k| k.oauth_account.clone()))
                            .unwrap_or_else(|| OauthAccount::synthesized(&roster.record(slot).map(|r| r.email.clone()).unwrap_or_default()));
                        let folded = SlotFile::new(live_credential, account);
                        credentials::write(&self.store, slot, &folded.to_value())?;
                        tracing::info!("folded the live Claude login back into slot {slot}");
                        if slot == target {
                            target_credential = folded.credential;
                        }
                    }
                }
                None => warnings.push(
                    "The live login does not match a managed account; it was left in place.".to_string(),
                ),
            }
        }
        backup_claude_live(&self.store.paths, &live)?;
        let backend = match target_credential.kind() {
            CredentialKind::ApiKey => live_api.write_managed_key(target_credential.api_key().unwrap_or_default())?,
            _ => {
                let mut object = live
                    .credential
                    .clone()
                    .filter(|c| c.kind() != CredentialKind::ApiKey)
                    .unwrap_or_else(|| ClaudeCredential::from_value(serde_json::json!({})));
                object.replace_oauth_from(&target_credential);
                live_api.write_oauth(&object)?
            }
        };
        let account = stored.oauth_account.0.clone();
        live_api.update_global_config(|config| {
            config.insert(OAUTH_ACCOUNT_KEY.to_string(), account);
        })?;
        roster.set_active_for(Provider::Claude, Some(target));
        self.write_roster(&roster)?;
        drop(claude_locks);
        drop(store_lock);

        let followup = Some(backend.followup().to_string());
        self.replan_active(target, &record);
        tracing::info!(
            "Switched from account {} to {target} (claude)",
            from.as_ref()
                .and_then(|f| f.number)
                .map_or_else(|| "none".to_string(), |n| n.to_string())
        );
        let switched = from.as_ref().and_then(|f| f.number) != Some(target);
        let label = account_label(target, &record.email);
        let (reason, verb, message, show_list) = match (switched, live_slot.is_some()) {
            (true, true) => ("switched", "Switched to", format!("Switched to {label}"), true),
            (true, false) => ("switched", "Activated", format!("Activated {label}"), false),
            (false, _) => ("activated", "Activated", format!("Activated {label} from stored backup"), false),
        };
        for warning in &warnings {
            self.say(Line::warning(warning));
        }
        self.say(
            Line::new()
                .push(Style::Accent, verb)
                .push(Style::Plain, message[verb.len()..].to_string()),
        );
        Ok(SwitchReport {
            outcome: SwitchOutcome {
                switched,
                provider: Provider::Claude,
                from,
                to: Some(to),
                strategy: strategy.to_string(),
                reason: reason.to_string(),
                message,
                warnings,
            },
            followup,
            show_list,
        })
    }
```

and the helper next to `live_email`:

```rust
/// The email shown for a live Claude login: `oauthAccount`'s, else a placeholder.
fn claude_live_email(live: &LiveLogin) -> String {
    let email = live
        .oauth_account
        .as_ref()
        .map(|a| a.email_address().to_lowercase())
        .unwrap_or_default();
    if !email.is_empty() {
        return email;
    }
    match live.credential.as_ref().map(ClaudeCredential::kind) {
        Some(CredentialKind::ApiKey) => "api-key".to_string(),
        _ => "unknown".to_string(),
    }
}
```

`perform_switch` now takes the already-read `record` only for the Claude branch; keep the Codex branch's own `record` variable as is (pass `record.clone()` to the Claude branch before the Codex code uses it).

- [ ] **Step 4: Run the tests**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add src/switcher.rs
git commit -m "feat(switch): switch Claude accounts under Claude Code's locks with backup, fold-back and the backend follow-up"
```

---

### Task 14: Provider-scoped rotation and strategies; `list` / `status` for both providers

**Files:**
- Modify: `src/switcher.rs`, `src/cli/switch.rs`, `src/tui/worker.rs` (call sites)
- Test: `src/switcher.rs`

**Interfaces:**
- Produces: `Switcher::switch(provider: Option<Provider>, strategy, models, interactive)`; private `resolve_provider`; `ListSnapshot.actives` filled for both providers and `is_active` per provider; `Switcher::status()` returns both providers.
- Consumes: Task 3 types, Task 11 `live_login_for`, Task 12 `current_account_for`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn rotation_needs_a_selector_when_both_providers_have_accounts() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        fx.switcher.add_account(Provider::Codex, None, None).unwrap();
        fx.write_live(&chatgpt("b@example.com", "acct-2", "rt-2"));
        fx.switcher.add_account(Provider::Codex, None, None).unwrap();
        let report = fx.switcher.switch(None, Strategy::Rotation, &[], false).unwrap();
        assert_eq!(report.outcome.to.as_ref().unwrap().number, Some(1), "one provider: rotation as before");
        fx.write_claude_live("c@example.com", "org", "", "crt-1");
        fx.switcher.add_account(Provider::Claude, None, None).unwrap();
        let err = fx.switcher.switch(None, Strategy::Rotation, &[], false).unwrap_err();
        assert_eq!(err.type_name(), "ConfigError");
        assert_eq!(
            err.to_string(),
            "Both Codex and Claude accounts are managed — say which: cswitch switch codex | cswitch switch claude"
        );
        fx.lines.borrow_mut().clear();
        let report = fx.switcher.switch(Some(Provider::Claude), Strategy::Rotation, &[], false).unwrap();
        assert_eq!(report.outcome.reason, "only-one-account");
        assert_eq!(report.outcome.provider, Provider::Claude);
        let report = fx.switcher.switch(Some(Provider::Codex), Strategy::Rotation, &[], false).unwrap();
        assert_eq!(report.outcome.to.as_ref().unwrap().number, Some(2));
        assert_eq!(report.outcome.provider, Provider::Codex);
    }

    #[test]
    fn rotation_stays_inside_the_selected_provider() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        fx.switcher.add_account(Provider::Codex, None, None).unwrap();
        fx.write_claude_live("one@example.com", "org-1", "", "crt-1");
        fx.switcher.add_account(Provider::Claude, None, None).unwrap();
        fx.write_claude_live("two@example.com", "org-2", "", "crt-2");
        fx.switcher.add_account(Provider::Claude, None, None).unwrap();
        let report = fx.switcher.switch(Some(Provider::Claude), Strategy::Rotation, &[], false).unwrap();
        assert_eq!(report.outcome.from.as_ref().unwrap().number, Some(3));
        assert_eq!(report.outcome.to.as_ref().unwrap().number, Some(2), "slot 1 (Codex) is never a Claude target");
        let report = fx.switcher.switch(Some(Provider::Claude), Strategy::Rotation, &[], false).unwrap();
        assert_eq!(report.outcome.to.as_ref().unwrap().number, Some(3));
        assert_eq!(fx.roster().active_for(Provider::Codex), Some(1), "the Codex marker never moved");
    }

    #[test]
    fn list_and_status_report_one_active_per_provider() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        fx.write_claude_live("c@example.com", "org", "Acme", "crt-1");
        fx.switcher.add_accounts(None, None, None).unwrap();
        let list = fx.switcher.list_snapshot(false).unwrap().unwrap();
        assert_eq!(list.actives, ActiveSlots { codex: Some(1), claude: Some(2) });
        assert!(list.rows.iter().all(|row| row.is_active));
        assert_eq!(list.rows[1].record.provider, Provider::Claude);
        let status = fx.switcher.status().unwrap();
        assert_eq!(status.total, 2);
        assert_eq!(status.providers.len(), 2);
        assert_eq!(status.providers[0].provider, Provider::Codex);
        assert_eq!(status.providers[0].current.slot(), Some(1));
        assert_eq!(status.providers[1].provider, Provider::Claude);
        assert_eq!(status.providers[1].current.slot(), Some(2));
        assert_eq!(status.providers[1].row.as_ref().unwrap().record.email, "c@example.com");
        fx.switcher.remove("2", false).unwrap_or_default();
    }
```

(`use crate::model::ActiveSlots;` in the tests module.)

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib switcher::tests::rotation_ switcher::tests::list_and_status`
Expected: compile errors (`switch` takes 3 arguments).

- [ ] **Step 3: Implement**

```rust
    /// The provider a bare verb acts on: the selector, else the only provider
    /// with accounts, else the user has to say (spec §6.1).
    fn resolve_provider(&self, roster: &Roster, explicit: Option<Provider>, verb: &str) -> Result<Provider> {
        if let Some(provider) = explicit {
            return Ok(provider);
        }
        let present: Vec<Provider> = Provider::ALL
            .into_iter()
            .filter(|p| !roster.slots_of(*p).is_empty())
            .collect();
        match present.as_slice() {
            [] => Ok(Provider::Codex),
            [only] => Ok(*only),
            _ => Err(CswitchError::config(format!(
                "Both Codex and Claude accounts are managed — say which: cswitch {verb} codex | cswitch {verb} claude"
            ))),
        }
    }
```

`switch`:

```rust
    pub fn switch(
        &mut self,
        provider: Option<Provider>,
        strategy: Strategy,
        models: &[String],
        interactive: bool,
    ) -> Result<SwitchReport> {
        let roster = self.roster()?;
        let provider = self.resolve_provider(&roster, provider, "switch")?;
        let name = strategy.name();
        let noop = |from: Option<AccountRef>, reason: &str, message: String| SwitchReport {
            outcome: SwitchOutcome {
                switched: false,
                provider,
                from: from.clone(),
                to: from,
                strategy: name.to_string(),
                reason: reason.to_string(),
                message,
                warnings: Vec::new(),
            },
            followup: None,
            show_list: false,
        };
        let current = self.current_account_for(provider)?;
        let live_slot = match &current {
            CurrentAccount::NoLogin => return self.activate_fresh(roster, provider, name),
            CurrentAccount::Unmanaged { email } => {
                // … unchanged, except `self.add_account(None, None)` becomes
                //    `self.add_account(provider, None, None)` …
            }
            CurrentAccount::Managed { slot, .. } => *slot,
        };
        let current_ref = AccountRef { number: Some(live_slot), email: current.email().unwrap_or_default().to_string() };
        if roster.slots_of(provider).len() <= 1 {
            let message = "Only one account is managed. Add more accounts to switch between.";
            self.say(Line::dimmed(message));
            return Ok(noop(Some(current_ref), "only-one-account", message.to_string()));
        }
        match strategy {
            Strategy::Rotation => self.rotate(roster, provider, live_slot, current_ref, None, models),
            Strategy::NextAvailable => {
                let entries = self.collect_for_switch(&roster, provider, live_slot, models)?;
                self.rotate(roster, provider, live_slot, current_ref, Some(&entries), models)
            }
            Strategy::Best => {
                let entries = self.collect_for_switch(&roster, provider, live_slot, models)?;
                self.best(roster, provider, live_slot, current_ref, &entries, models)
            }
        }
    }
```

`activate_fresh(roster, provider, strategy)`: `preferred = roster.active_for(provider).filter(…)`; the order iterates `roster.slots_of(provider)` instead of `roster.sequence`. `collect_for_switch(roster, provider, live_slot, models)`: `candidates` = `roster.switchable_slots(…)` filtered with `roster.record(*s).is_some_and(|r| r.provider == provider)`; `CollectOptions { actives: vec![live_slot], … }`. `rotate(roster, provider, live_slot, current_ref, entries, models)`: `anchor` uses `roster.active_for(provider)` and `sequence = roster.slots_of(provider)`; the `noop` closure sets `provider`. `best(roster, provider, …)`: `candidates` filtered by provider; `noop` sets `provider`. In `remove` and `set_disabled` replace `self.current_account()?.slot() == Some(slot)` with `self.current_account_for(record.provider)?.slot() == Some(slot)` (in `set_disabled` read the provider from the record first).

`list_snapshot`:

```rust
        let mut actives = ActiveSlots::default();
        for provider in Provider::ALL {
            actives.set(provider, collect::live_login_for(&self.store, &roster, provider).slot());
        }
        // …
        let mut collected = collect::run_pass(&self.store, &roster, CollectOptions {
            mode,
            actives: actives.iter().filter_map(|(_, slot)| slot).collect(),
            candidates: &slots,
            threshold: self.settings.autoswitch.threshold,
            models: &self.settings.autoswitch.model_names(),
        })?;
        let rows = slots.into_iter().map(|slot| {
            let record = roster.record(slot).cloned().expect("filtered above");
            let is_active = actives.get(record.provider) == Some(slot);
            AccountRow { slot, record, usage: …, is_active }
        }).collect();
        Ok(Some(ListSnapshot { actives, rows, warnings: collected.token_persist_failures }))
```

`status`:

```rust
    pub fn status(&self) -> Result<StatusSnapshot> {
        let roster = self.roster_opt()?.unwrap_or_else(Roster::empty);
        let total = roster.sorted_slots().len();
        let mut providers = Vec::new();
        for provider in Provider::ALL {
            let current = collect::live_login_for(&self.store, &roster, provider);
            let row = match (current.slot(), current.slot().and_then(|s| roster.record(s))) {
                (Some(slot), Some(record)) => {
                    let mut collected = collect::run_pass(&self.store, &roster, CollectOptions {
                        mode: CollectMode::OnDemand,
                        actives: vec![slot],
                        candidates: &[],
                        threshold: self.settings.autoswitch.threshold,
                        models: &self.settings.autoswitch.model_names(),
                    })?;
                    Some(AccountRow {
                        slot,
                        record: record.clone(),
                        usage: collected.entries.remove(&slot).unwrap_or_else(|| self.fallback_entry(slot)),
                        is_active: true,
                    })
                }
                _ => None,
            };
            providers.push(ProviderStatus { provider, current, row });
        }
        Ok(StatusSnapshot { providers, total })
    }
```

Call sites: `src/cli/switch.rs` `rotate_cmd` → `switcher.switch(None, strategy, &models, !json)` (Task 15 passes the selector); `src/tui/worker.rs` `Action::SwitchBest` → `switcher.switch(None, Strategy::Best, &models, false)` (Task 17 passes the provider).

- [ ] **Step 4: Run the tests**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt`
Expected: green; `tests/cli_switch.rs` keeps passing because a Codex-only roster resolves to Codex.

- [ ] **Step 5: Commit**

```bash
git add src/switcher.rs src/cli/switch.rs src/tui/worker.rs
git commit -m "feat(switch): scope rotation and strategies to one provider; list and status report both"
```

---

### Task 15: The CLI selector, two-block `list`, per-provider `status`, help text

**Files:**
- Modify: `src/cli/legacy.rs`, `src/cli/mod.rs`, `src/cli/accounts.rs`, `src/cli/list.rs`, `src/cli/switch.rs`
- Test: `src/cli/legacy.rs`, `src/cli/list.rs`

**Interfaces:**
- Produces: `legacy::Options.provider: Option<Provider>`; `--provider codex|claude` (hidden flag the selector word translates to); `list::list_cmd(switcher, json, token_status, provider)`, `list::status_cmd(switcher, json, provider)`, `list::list_lines(switcher, &ListSnapshot, token_status)` with provider blocks; `switch::rotate_cmd(switcher, provider, strategy, model, json)`; `accounts::add(switcher, provider, slot, alias)`.
- Consumes: Tasks 12–14.

- [ ] **Step 1: Write the failing tests**

`src/cli/legacy.rs` tests — extend `translation_worked_examples` with:

```rust
            (&["switch", "claude"], &["--switch", "--provider", "claude"]),
            (&["switch", "Codex", "--strategy", "best"], &["--switch", "--provider", "codex", "--strategy", "best"]),
            (&["add", "claude", "--slot", "3"], &["--add-account", "--provider", "claude", "--slot", "3"]),
            (&["list", "codex", "--json"], &["--list", "--provider", "codex", "--json"]),
            (&["ls", "claude"], &["--list", "--provider", "claude"]),
            (&["status", "claude"], &["--status", "--provider", "claude"]),
            (&["switch", "dev"], &["--switch-to", "dev"]),
            (&["remove", "claude"], &["--remove-account", "claude"]),
```

`parse_selects_commands_and_options`:

```rust
        let opts = parse(&argv(&["--switch", "--provider", "claude"])).unwrap();
        assert_eq!(opts.provider, Some(Provider::Claude));
        assert_eq!(
            parse(&argv(&["--list", "--provider", "gemini"])).unwrap_err(),
            "argument --provider: invalid choice: 'gemini' (choose from 'codex', 'claude')"
        );
```

`cross_flag_validation_in_order`:

```rust
        assert_eq!(
            check(&["--remove-account", "2", "--provider", "codex"]),
            "--provider can only be used with 'switch', 'add', 'list', or 'status'"
        );
```

and in the `ok` list add `&["--switch", "--provider", "claude", "--strategy", "best"]` and `&["--status", "--provider", "codex", "--json"]`. `help_and_version`: replace `assert!(help.contains("Multi-Account Switcher for OpenAI Codex"))` with `"Multi-Account Switcher for OpenAI Codex and Claude Code"`, drop `assert!(!help.contains("Claude"))`, add `assert!(help.contains("cswitch switch [codex|claude]"))` and `assert!(help.contains("sk-ant-"))`.

`src/cli/list.rs` tests:

```rust
    #[test]
    fn list_lines_split_into_provider_blocks_only_for_a_mixed_roster() {
        let (_dir, store) = crate::store::temp_store();
        let switcher = Switcher::open(store);
        let mut claude = AccountRecord::new("c@x.io");
        claude.provider = crate::provider::Provider::Claude;
        let row = |slot: u32, record: AccountRecord| AccountRow { slot, record, usage: entry(), is_active: false };
        let mixed = ListSnapshot {
            actives: crate::model::ActiveSlots::default(),
            rows: vec![row(1, AccountRecord::new("a@x.io")), row(2, claude.clone())],
            warnings: Vec::new(),
        };
        assert_eq!(
            texts(&list_lines(&switcher, &mixed, false)),
            [
                "Codex accounts:",
                "  1: a@x.io [personal]",
                "     usage unavailable",
                "",
                "Claude accounts:",
                "  2: c@x.io [personal]",
                "     usage unavailable",
            ]
        );
        let single = ListSnapshot {
            actives: crate::model::ActiveSlots::default(),
            rows: vec![row(2, claude)],
            warnings: Vec::new(),
        };
        assert_eq!(texts(&list_lines(&switcher, &single, false))[0], "Accounts:");
    }
```

(and extend `status_lines_variants` with a both-managed case expecting `Codex status: Account-1 (a@x.io [personal])` and `Claude status: Account-2 (…)` blocks separated by a blank line.)

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib cli::`
Expected: compile errors / assertion failures.

- [ ] **Step 3: Implement**

`src/cli/legacy.rs`:

- `use crate::provider::Provider;` and `pub provider: Option<Provider>` in `Options`.
- `translate`: for `switch`, check the selector first:

```rust
    if first == "switch" {
        let rest = &argv[1..];
        return match rest.first() {
            Some(word) if Provider::parse_selector(word).is_some() => {
                let mut out = vec!["--switch".to_string(), "--provider".to_string(), word.to_ascii_lowercase()];
                out.extend(rest[1..].iter().cloned());
                out
            }
            Some(target) if !target.starts_with('-') => { /* --switch-to as today */ }
            _ => { /* --switch as today */ }
        };
    }
```

and after the `VERB_FLAGS` lookup, for `add`, `list`, `ls`, `status` only:

```rust
        Some((verb, flag)) => {
            let mut out = vec![flag.to_string()];
            let mut rest = argv[1..].iter();
            if matches!(*verb, "add" | "list" | "ls" | "status")
                && let Some(word) = argv.get(1)
                && Provider::parse_selector(word).is_some()
            {
                out.push("--provider".to_string());
                out.push(word.to_ascii_lowercase());
                rest.next();
            }
            out.extend(rest.cloned());
            out
        }
```

- `parse`: `"--provider" => { let value = take_value(&mut i, false)?; opts.provider = Some(Provider::parse_selector(&value).ok_or_else(|| format!("argument --provider: invalid choice: '{value}' (choose from 'codex', 'claude')"))?); }`.
- `validate`, as the last check: `if opts.provider.is_some() && !is(|c| matches!(c, Switch | AddAccount | List | Status)) { return Err("--provider can only be used with 'switch', 'add', 'list', or 'status'".into()); }`.
- `help_text`: header `Multi-Account Switcher for OpenAI Codex and Claude Code`; commands:

```
  cswitch list [codex|claude]        list managed accounts (both providers by default)
  cswitch status [codex|claude]      show the active account of each provider
  cswitch switch [codex|claude]      rotate to the next account of one provider
  cswitch switch <num|email>         switch to a specific account (Codex or Claude)
  cswitch add [codex|claude]         add the current login(s)
  cswitch add-token [TOKEN|-]        register an OpenAI API key, or an Anthropic API key / setup-token (sk-ant-…)
```

(the remaining lines unchanged); options add after `--full`:

```
  --provider {{codex,claude}}
                        Act on one provider; the same as the word after
                        'switch', 'add', 'list' or 'status'
```

and `--email` reads `… defaults to api-key-{{slot}}@token.local (setup-token-{{slot}}@token.local for a Claude setup-token) …`; examples add `cswitch switch claude                     # rotate among the Claude accounts` and `cswitch add-token sk-ant-oat01-... --email me@example.com`.

`src/cli/mod.rs` dispatch:

```rust
        Command::AddAccount => accounts::add(switcher, opts.provider, opts.slot, opts.alias.as_deref()),
        Command::List => list::list_cmd(switcher, json, opts.token_status, opts.provider),
        Command::Status => list::status_cmd(switcher, json, opts.provider),
        Command::Switch => switch::rotate_cmd(switcher, opts.provider, opts.strategy.as_deref(), opts.model.as_deref(), json),
```

`src/cli/accounts.rs`: `pub fn add(switcher, provider: Option<Provider>, slot, alias) -> Result<i32> { switcher.add_accounts(provider, slot, alias)?; Ok(0) }`.

`src/cli/switch.rs`: `rotate_cmd(switcher, provider: Option<Provider>, strategy, model, json)` → `switcher.switch(provider, strategy, &models, !json)`.

`src/cli/list.rs`:

```rust
/// The `Accounts:` block, or one `<Provider> accounts:` block per provider
/// when the rows span both.
pub fn list_lines(switcher: &Switcher, snapshot: &ListSnapshot, token_status: bool) -> Vec<Line> {
    let now = now_unix();
    let providers: Vec<Provider> = Provider::ALL
        .into_iter()
        .filter(|p| snapshot.rows.iter().any(|r| r.record.provider == *p))
        .collect();
    let mut lines = Vec::new();
    for (block, provider) in providers.iter().enumerate() {
        if block > 0 {
            lines.push(Line::new());
        }
        let title = if providers.len() > 1 {
            format!("{} accounts:", provider.title())
        } else {
            "Accounts:".to_string()
        };
        lines.push(Line::new().push(Style::Bold, title));
        let rows: Vec<&AccountRow> = snapshot.rows.iter().filter(|r| r.record.provider == *provider).collect();
        let count = rows.len();
        for (i, row) in rows.into_iter().enumerate() {
            lines.push(account_line(row));
            lines.extend(usage_lines(&row.usage, now, "     "));
            if token_status {
                lines.extend(token_status_lines(switcher, row, now));
            }
            if i + 1 < count {
                lines.push(Line::new());
            }
        }
    }
    if providers.is_empty() {
        lines.push(Line::new().push(Style::Bold, "Accounts:"));
    }
    // … warnings as before …
    lines
}
```

`token_status_lines` returns `Vec::new()` for Claude rows (`row.record.provider == Provider::Claude`) in phase 1. `list_cmd(switcher, json, token_status, provider)`: after the snapshot, `if let Some(p) = provider { snapshot.rows.retain(|r| r.record.provider == p); }` (bind `let mut snapshot`). `status_cmd(switcher, json, provider)`: `if let Some(p) = provider { snapshot.providers.retain(|s| s.provider == p); }`. `first_run`:

```rust
    print_lines(&[Line::dimmed("No accounts are managed yet.")]);
    let mut found = Vec::new();
    for provider in Provider::ALL {
        if let CurrentAccount::Unmanaged { email } | CurrentAccount::Managed { email, .. } = switcher.current_account_for(provider)? {
            found.push(if email.is_empty() { "API key".to_string() } else { email });
        }
    }
    if found.is_empty() {
        print_lines(&[Line::dimmed("No active Codex or Claude login found. Log in first.")]);
        return Ok(0);
    }
    print!("No managed accounts found. Add current account ({}) to managed list? [Y/n] ", found.join(" and "));
    // … the prompt handling as today …
    switcher.add_accounts(None, None, None)?;
    Ok(0)
```

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt`
Expected: green. If `tests/cli_list.rs` asserts the first-run prompt text, it keeps passing (a single Codex login prints one email).

- [ ] **Step 5: Commit**

```bash
git add src/cli
git commit -m "feat(cli): accept a provider selector on switch, add, list and status; two-block list; help text"
```

---

### Task 16: Integration harness and end-to-end tests for the mixed roster

**Files:**
- Modify: `tests/support/mod.rs`, `tests/support/usage_mock.rs`
- Create: `tests/cli_claude.rs`

**Interfaces:**
- Produces: `Cli.claude_home`, `Cli::write_claude_live(email, org_uuid, org_name, refresh)`, `write_claude_live_with(creds, config)`, `remove_claude_live()`, `claude_credentials()`, `claude_config()`, `claude_backups()`, `add_claude(email, org_uuid, org_name, refresh) -> Run`, `claude_calls()`; `support::claude_creds(email_hint, refresh, access)`, `support::claude_config(email, org_uuid, org_name)`; `UsageMock.claude_usage_url`, `claude_token_url`, `claude_token_calls()`; bearer constants `CLAUDE_OK`, `CLAUDE_LIMIT_7D`, `CLAUDE_STALE`, `CLAUDE_REFRESHED`, `CLAUDE_ROTATED_REFRESH`, `CLAUDE_POOL_NAME`, `claude_live_refresh_token(email)`.
- Consumes: the binary built by Tasks 1–15.

- [ ] **Step 1: Extend the harness**

`tests/support/mod.rs`:

- `Cli` gains `pub claude_home: PathBuf` (`root/claude`, created in `new`), a fake `claude` next to the fake `codex` (`#!/bin/sh\nprintf '%s\n' "$*" >> "$CS_CLAUDE_LOG"\nexit 0\n`, plus a `claude.cmd` twin on Windows), `claude_log: PathBuf`, `claude_usage_url: Option<String>`, `claude_token_url: Option<String>`.
- `command()` adds: `.env("CLAUDE_CONFIG_DIR", &self.claude_home)`, `.env("CSWITCH_KEYCHAIN", "off")`, `.env("USER", "tester")`, `.env("CSWITCH_CLAUDE_USAGE_URL", self.claude_usage_url.clone().unwrap_or_else(|| format!("{}/claude-usage", self.dead_url)))`, `.env("CSWITCH_CLAUDE_TOKEN_URL", …"/claude-token")`, `.env("CS_CLAUDE_LOG", &self.claude_log)`, `.env("CSWITCH_CLAUDE_LOCK_BUDGET_MS", "300")`.
- `with_mock` also copies `mock.claude_usage_url` / `mock.claude_token_url`.
- Helpers:

```rust
    pub fn claude_credentials_path(&self) -> PathBuf {
        self.claude_home.join(".credentials.json")
    }

    pub fn claude_config_path(&self) -> PathBuf {
        self.claude_home.join(".claude.json")
    }

    pub fn write_claude_live_with(&self, creds: &Value, config: &Value) {
        fs::write(self.claude_credentials_path(), serde_json::to_string_pretty(creds).unwrap()).unwrap();
        fs::write(self.claude_config_path(), serde_json::to_string_pretty(config).unwrap()).unwrap();
    }

    pub fn write_claude_live(&self, email: &str, org_uuid: &str, org_name: &str, refresh: &str) {
        self.write_claude_live_with(
            &claude_creds(refresh, &format!("cat-{refresh}")),
            &claude_config(email, org_uuid, org_name),
        );
    }

    pub fn remove_claude_live(&self) {
        let _ = fs::remove_file(self.claude_credentials_path());
        let _ = fs::remove_file(self.claude_config_path());
    }

    pub fn claude_credentials(&self) -> Value {
        read_json(&self.claude_credentials_path())
    }

    pub fn claude_config(&self) -> Value {
        read_json(&self.claude_config_path())
    }

    pub fn claude_backups(&self) -> Vec<String> {
        let dir = self.cswitch_home.join("backups").join("claude");
        let mut names: Vec<String> = fs::read_dir(&dir)
            .map(|entries| entries.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn claude_calls(&self) -> Vec<String> {
        fs::read_to_string(&self.claude_log).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// `add claude` the given login and return the run.
    pub fn add_claude(&self, email: &str, org_uuid: &str, org_name: &str, refresh: &str) -> Run {
        self.write_claude_live(email, org_uuid, org_name, refresh);
        let run = self.run(&["add", "claude"]);
        assert_eq!(run.status, 0, "add claude failed: {}{}", run.stdout, run.stderr);
        run
    }
```

and module-level builders:

```rust
pub fn claude_creds(refresh: &str, access: &str) -> Value {
    json!({
        "claudeAiOauth": {
            "accessToken": access,
            "refreshToken": refresh,
            "expiresAt": 4_102_444_800_000i64,
            "scopes": ["user:inference", "user:profile"],
            "subscriptionType": "max"
        },
        "mcpOAuth": {"srv": {"accessToken": "mcp-token"}}
    })
}

pub fn claude_config(email: &str, org_uuid: &str, org_name: &str) -> Value {
    json!({
        "numStartups": 7,
        "oauthAccount": {
            "accountUuid": format!("uuid-{}", email.to_lowercase()),
            "emailAddress": email,
            "organizationUuid": org_uuid,
            "organizationName": org_name,
            "billingType": "stripe"
        },
        "projects": {"/tmp/p": {"allowedTools": []}}
    })
}
```

`tests/support/usage_mock.rs`: constants

```rust
/// Claude: 5h 40 %, 7d 55 %, spend $7.29 / $50.00, `Fable` weekly window at 62 %.
pub const CLAUDE_OK: &str = "cat-ok";
/// Claude: 5h 10 %, 7d 100 % (resets tomorrow).
pub const CLAUDE_LIMIT_7D: &str = "cat-limit-7d";
/// Claude: HTTP 401.
pub const CLAUDE_STALE: &str = "cat-stale";
/// The bearer the Claude token endpoint issues: 5h 30 %, 7d 35 %.
pub const CLAUDE_REFRESHED: &str = "cat-refreshed";
pub const CLAUDE_ROTATED_REFRESH: &str = "crt-next";
pub const CLAUDE_POOL_NAME: &str = "Fable";

pub fn claude_live_refresh_token(email: &str) -> String {
    format!("crt-live|{email}")
}
```

routes `.route("/api/oauth/usage", get(claude_usage))` and `.route("/v1/oauth/token", post(claude_token))`, fields `claude_usage_url: format!("http://{addr}/api/oauth/usage")`, `claude_token_url: format!("http://{addr}/v1/oauth/token")`, recorded paths `/api/oauth/usage` and `/v1/oauth/token`, `trail()` maps them to `claude-usage:<bearer>` / `claude-token:<refresh>`, `claude_token_calls()` counts the token records.

```rust
fn claude_body(five: f64, seven: f64, seven_reset: &str) -> Value {
    json!({
        "five_hour": {"utilization": five, "resets_at": "2099-01-01T10:00:00Z"},
        "seven_day": {"utilization": seven, "resets_at": seven_reset},
        "seven_day_opus": null
    })
}

async fn claude_usage(State(log): State<Arc<Log>>, headers: HeaderMap) -> Response {
    let bearer = /* as `usage` */;
    log.requests.lock().unwrap().push(Recorded { path: "/api/oauth/usage".into(), bearer: bearer.clone(), refresh_token: None });
    if headers.get("anthropic-beta").and_then(|v| v.to_str().ok()) != Some("oauth-2025-04-20") {
        return (StatusCode::BAD_REQUEST, "missing anthropic-beta").into_response();
    }
    match bearer.as_deref().unwrap_or("") {
        CLAUDE_OK => {
            let mut b = claude_body(40.0, 55.0, "2099-01-03T10:00:00Z");
            b["extra_usage"] = json!({"is_enabled": true, "used_credits": 729, "monthly_limit": 5000, "utilization": 14.58, "currency": "USD"});
            b["limits"] = json!([{"kind": "weekly_scoped", "percent": 62, "resets_at": "2099-01-03T10:00:00Z", "scope": {"model": {"display_name": CLAUDE_POOL_NAME}}}]);
            Json(b).into_response()
        }
        CLAUDE_LIMIT_7D => Json(claude_body(10.0, 100.0, "2099-01-02T10:00:00Z")).into_response(),
        CLAUDE_REFRESHED => Json(claude_body(30.0, 35.0, "2099-01-03T10:00:00Z")).into_response(),
        _ => (StatusCode::UNAUTHORIZED, Json(json!({"error": {"type": "authentication_error"}}))).into_response(),
    }
}

async fn claude_token(State(log): State<Arc<Log>>, Json(body): Json<Value>) -> Response {
    let refresh = body["refresh_token"].as_str().unwrap_or("").to_string();
    log.requests.lock().unwrap().push(Recorded { path: "/v1/oauth/token".into(), bearer: None, refresh_token: Some(refresh.clone()) });
    if body["grant_type"] != "refresh_token" || body["client_id"] != "9d1c250a-e61b-44d9-88ed-5944d1962f5e" {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_request"}))).into_response();
    }
    if refresh.starts_with("crt-live|") {
        return Json(json!({"access_token": CLAUDE_REFRESHED, "expires_in": 3600, "refresh_token": CLAUDE_ROTATED_REFRESH, "scope": "user:inference user:profile"})).into_response();
    }
    (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "error_description": "unknown refresh token"}))).into_response()
}
```

- [ ] **Step 2: Write the end-to-end tests**

`tests/cli_claude.rs`:

```rust
//! The mixed roster through the built binary: both logins captured by `add`,
//! Claude switches under the lock protocol, two-block `list`, `status`,
//! JSON v2, and the provider selector.

mod support;

use serde_json::{Value, json};
use support::usage_mock::{self, CLAUDE_LIMIT_7D, CLAUDE_OK, CLAUDE_POOL_NAME, CLAUDE_REFRESHED, CLAUDE_ROTATED_REFRESH, CLAUDE_STALE, UsageMock};
use support::{Cli, api_key_auth, chatgpt_auth, claude_config, claude_creds};

fn mixed() -> Cli {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    cli.add_claude("One@example.com", "org-1", "", "crt-1");
    cli.add_claude("two@example.com", "org-2", "Acme", "crt-2");
    cli
}

#[test]
fn add_captures_both_logins_even_with_the_same_email() {
    let cli = Cli::new();
    cli.write_live(&chatgpt_auth("Me@Example.com", "acct-me", "rt-me"));
    cli.write_claude_live("Me@Example.com", "org-me", "Acme", "crt-me");
    let run = cli.run(&["add"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "Added Account 1: me@example.com [Plus]\nAdded Account 2: me@example.com [Acme]\n"
    );
    let roster = cli.roster();
    assert_eq!(roster["accounts"]["1"]["provider"], "codex");
    assert_eq!(roster["accounts"]["2"]["provider"], "claude");
    assert_eq!(roster["accounts"]["2"]["organizationUuid"], "org-me");
    assert_eq!(roster["accounts"]["2"]["uuid"], "uuid-me@example.com");
    assert_eq!(roster["activeByProvider"], json!({"claude": 2, "codex": 1}));
    assert_eq!(roster["activeAccountNumber"], 1);
    let stored = cli.credential(2);
    assert_eq!(stored["claudeAiOauth"]["refreshToken"], "crt-me");
    assert_eq!(stored["oauthAccount"]["billingType"], "stripe");
    assert!(stored.get("mcpOAuth").is_none());

    let run = cli.run(&["add"]);
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Updated credentials for Account 1 (me@example.com [Plus]).\n\
         Updated credentials for Account 2 (me@example.com [Acme]).\n\
         Both current logins were already managed: Account-1 (codex), Account-2 (claude) — nothing new was added.\n"
    );
}

#[test]
fn add_selector_missing_login_and_flags() {
    let cli = Cli::new();
    let run = cli.run(&["add"]);
    assert_eq!(run.status, 1);
    assert_eq!(run.stderr.trim(), "Error: No active Codex or Claude login found. Log in first.");
    cli.write_live(&chatgpt_auth("alice@example.com", "acct-alice", "rt-a"));
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 1);
    assert_eq!(run.stderr.trim(), "Error: No active Claude account found. Please log in first.");
    let run = cli.run(&["add", "codex", "--alias", "work"]);
    assert_eq!(run.stdout, "Added Account 1: alice@example.com [Plus]\n");
    cli.write_claude_live("c@example.com", "org", "", "crt-c");
    let run = cli.run(&["add", "--slot", "5"]);
    assert_eq!(run.status, 1);
    assert!(run.stderr.starts_with("Error: --slot/--alias need a single login"), "{}", run.stderr);
    let run = cli.run(&["add", "claude", "--slot", "5"]);
    assert_eq!(run.stdout, "Added Account 5: c@example.com [personal]\n");
    let run = cli.run(&["alias", "5", "claude"]);
    assert_eq!(run.status, 1);
    assert_eq!(run.stderr.trim(), "Error: alias 'claude' is reserved for the provider selector");
}

#[test]
fn add_token_routes_by_prefix() {
    let cli = Cli::new();
    let run = cli.run(&["add-token", "sk-ant-api03-key"]);
    assert_eq!(run.stdout, "Added Account 1: api-key-1@token.local [personal] (from API key)\n");
    assert_eq!(cli.roster()["accounts"]["1"]["provider"], "claude");
    assert_eq!(cli.roster()["accounts"]["1"]["kind"], "api_key");
    assert_eq!(cli.credential(1)["primaryApiKey"], "sk-ant-api03-key");
    let run = cli.run(&["add-token", "sk-ant-oat01-tok", "--email", "me@example.com"]);
    assert_eq!(run.stdout, "Added Account 2: me@example.com [personal] (from token)\n");
    assert_eq!(cli.roster()["accounts"]["2"]["provider"], "claude");
    assert!(cli.roster()["accounts"]["2"].get("kind").is_none());
    assert_eq!(cli.credential(2)["claudeAiOauth"]["accessToken"], "sk-ant-oat01-tok");
    let run = cli.run(&["add-token", "sk-openai"]);
    assert_eq!(run.stdout, "Added Account 3: api-key-3@token.local [personal] (from API key)\n");
    assert_eq!(cli.credential(3), api_key_auth("sk-openai"));
}

#[test]
fn switching_a_claude_slot_leaves_codex_alone() {
    let cli = mixed();
    let codex_before = cli.live();
    let run = cli.run(&["switch", "2"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines[0], "Switched to Account-2 (one@example.com)");
    assert_eq!(lines[1], "Codex accounts:");
    assert!(lines.contains(&"Claude accounts:"), "{lines:?}");
    assert!(
        run.stdout.trim_end().ends_with("New account is active on your next message — no restart needed."),
        "{}",
        run.stdout
    );
    assert_eq!(cli.claude_credentials()["claudeAiOauth"]["refreshToken"], "crt-1");
    assert_eq!(cli.claude_config()["oauthAccount"]["emailAddress"], "One@example.com");
    assert_eq!(cli.claude_config()["oauthAccount"]["organizationUuid"], "org-1");
    assert_eq!(cli.live(), codex_before);
    assert!(cli.live_backups().is_empty(), "no Codex backup for a Claude switch");
    assert_eq!(cli.claude_backups().len(), 1);
    assert!(cli.codex_calls().is_empty(), "the Codex daemon is never probed");
    assert_eq!(cli.roster()["activeByProvider"], json!({"claude": 2, "codex": 1}));
    assert!(!cli.claude_home.join(".oauth_refresh.lock").exists());
    assert!(!cli.root.path().join("claude.lock").exists());
    assert!(!cli.claude_home.join(".claude.json.lock").exists());

    let run = cli.run(&["switch", "2", "--json"]);
    let payload = run.json();
    assert_eq!(payload["schemaVersion"], 2);
    assert_eq!(payload["switched"], false);
    assert_eq!(payload["reason"], "already-active");
    assert_eq!(payload["provider"], "claude");
    assert_eq!(payload["to"]["provider"], "claude");
}

#[test]
fn switch_claude_keeps_sibling_keys() {
    let cli = mixed();
    let mut creds = claude_creds("crt-2", "cat-crt-2");
    creds["mcpOAuth"]["other"] = json!({"accessToken": "keep-me"});
    cli.write_claude_live_with(&creds, &claude_config("two@example.com", "org-2", "Acme"));
    let run = cli.run(&["switch", "2"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let after = cli.claude_credentials();
    assert_eq!(after["claudeAiOauth"]["refreshToken"], "crt-1");
    assert_eq!(after["mcpOAuth"]["other"]["accessToken"], "keep-me");
    assert_eq!(after["mcpOAuth"]["srv"]["accessToken"], "mcp-token");
    assert_eq!(cli.claude_config()["projects"]["/tmp/p"]["allowedTools"], json!([]));
    assert_eq!(cli.claude_config()["numStartups"], 7);
    let backup = support::read_json(&cli.cswitch_home.join("backups").join("claude").join(&cli.claude_backups()[0]));
    assert_eq!(backup["credentials"]["claudeAiOauth"]["refreshToken"], "crt-2");
    assert_eq!(backup["oauthAccount"]["emailAddress"], "two@example.com");
}

#[test]
fn bare_switch_needs_a_selector_when_both_providers_exist() {
    let cli = mixed();
    let run = cli.run(&["switch"]);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr.trim(),
        "Error: Both Codex and Claude accounts are managed — say which: cswitch switch codex | cswitch switch claude"
    );
    let run = cli.run(&["switch", "claude", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let payload = run.json();
    assert_eq!(payload["switched"], true);
    assert_eq!(payload["from"]["number"], 3);
    assert_eq!(payload["to"]["number"], 2);
    assert_eq!(payload["provider"], "claude");
    let run = cli.run(&["switch", "codex"]);
    assert_eq!(run.status, 0);
    assert!(run.stdout.contains("Only one account is managed"), "{}", run.stdout);
    assert_eq!(cli.live()["tokens"]["refresh_token"], "rt-a");
    let run = cli.run(&["--switch"]);
    assert_eq!(run.status, 1, "the legacy flag follows the same rule");
}

#[test]
fn list_prints_two_blocks_and_json_v2() {
    let cli = mixed();
    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines[0], "Codex accounts:");
    assert_eq!(lines[1], "  1: alice@example.com [Plus] (active)");
    let claude_at = lines.iter().position(|l| *l == "Claude accounts:").unwrap();
    assert_eq!(lines[claude_at - 1], "");
    assert_eq!(lines[claude_at + 1], "  2: one@example.com [personal]");
    assert_eq!(lines[claude_at + 3], "", "a blank line between accounts");
    assert!(lines[claude_at + 4].starts_with("  3: two@example.com [Acme] (active)"), "{}", lines[claude_at + 4]);
    let run = cli.run(&["list", "claude"]);
    assert_eq!(run.lines()[0], "Claude accounts:");
    assert!(!run.stdout.contains("alice@example.com"));

    let run = cli.run(&["list", "--json"]);
    let payload = run.json();
    assert_eq!(payload["schemaVersion"], 2);
    assert_eq!(payload["activeAccountNumber"], 1);
    assert_eq!(payload["active"], json!({"codex": 1, "claude": 3}));
    let rows = payload["accounts"].as_array().unwrap();
    assert_eq!(rows[0]["provider"], "codex");
    assert_eq!(rows[2]["provider"], "claude");
    assert_eq!(rows[2]["active"], true);
    assert_eq!(rows[1]["active"], false);
    assert_eq!(run.stderr, "");
}

#[test]
fn status_shows_both_providers() {
    let cli = mixed();
    let run = cli.run(&["status"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines[0], "Codex status: Account-1 (alice@example.com [Plus])");
    assert_eq!(lines[1], "  Total managed accounts: 3");
    let claude_at = lines.iter().position(|l| l.starts_with("Claude status:")).unwrap();
    assert_eq!(lines[claude_at], "Claude status: Account-3 (two@example.com [Acme])");
    let run = cli.run(&["status", "codex"]);
    assert_eq!(run.lines()[0], "Status: Account-1 (alice@example.com [Plus])");
    let run = cli.run(&["status", "--json"]);
    let payload = run.json();
    assert_eq!(payload["active"]["codex"]["number"], 1);
    assert_eq!(payload["active"]["claude"]["number"], 3);
    assert_eq!(payload["active"]["claude"]["provider"], "claude");
    assert_eq!(payload["active"]["claude"]["managed"], true);
    cli.remove_claude_live();
    let run = cli.run(&["status", "--json"]);
    assert!(run.json()["active"]["claude"].is_null());
}

#[test]
fn v1_roster_is_read_as_codex() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    let mut roster = cli.roster();
    roster["accounts"]["1"].as_object_mut().unwrap().remove("provider");
    roster.as_object_mut().unwrap().remove("activeByProvider");
    std::fs::write(cli.cswitch_home.join("sequence.json"), roster.to_string()).unwrap();
    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.lines()[0], "Accounts:");
    cli.remove_live();
    let run = cli.run(&["switch", "1", "--force"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(cli.live()["tokens"]["refresh_token"], "rt-a");
    assert_eq!(cli.roster()["accounts"]["1"]["provider"], "codex");
    assert_eq!(cli.roster()["activeByProvider"], json!({"codex": 1}));
}

#[test]
fn claude_usage_rows_refresh_and_the_active_login_is_never_refreshed() {
    let mock = UsageMock::start();
    let cli = Cli::new().with_mock(&mock);
    cli.write_claude_live_with(
        &claude_creds(&usage_mock::claude_live_refresh_token("one@example.com"), CLAUDE_OK),
        &claude_config("one@example.com", "org-1", ""),
    );
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    cli.write_claude_live_with(
        &claude_creds(&usage_mock::claude_live_refresh_token("two@example.com"), CLAUDE_STALE),
        &claude_config("two@example.com", "org-2", ""),
    );
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    // Two is live and stale: a 401 is reported, never refreshed. One is inactive and fine.
    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines[0], "Accounts:");
    assert!(lines[1].starts_with("  1: one@example.com"));
    // Labels are padded to the widest one (`Fable:`), so match the parts, not the columns.
    assert!(lines[2].starts_with("     ├ $$:") && lines[2].contains(" 15%"), "{}", lines[2]);
    assert!(lines[2].ends_with("  $7.29 / $50.00"), "{}", lines[2]);
    assert!(lines[3].starts_with("     ├ 5h:") && lines[3].contains(" 40%"), "{}", lines[3]);
    assert!(lines[4].starts_with("     ├ 7d:") && lines[4].contains(" 55%"), "{}", lines[4]);
    assert!(lines[5].starts_with(&format!("     └ {CLAUDE_POOL_NAME}:")), "{}", lines[5]);
    assert!(lines[7].starts_with("  2: two@example.com [personal] (active)"));
    assert_eq!(lines[8], "     usage unavailable (http-401)");
    assert_eq!(mock.claude_token_calls(), 0, "the active login belongs to Claude Code");

    // Make one the live login (fresh) and leave two inactive with a dead bearer: 401 → refresh → retry.
    cli.write_claude_live_with(
        &claude_creds(&usage_mock::claude_live_refresh_token("one@example.com"), CLAUDE_OK),
        &claude_config("one@example.com", "org-1", ""),
    );
    let run = cli.run(&["list", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let payload = run.json();
    let two = payload["accounts"].as_array().unwrap().iter().find(|r| r["number"] == 2).unwrap();
    assert_eq!(two["usageStatus"], "ok");
    assert_eq!(two["usage"]["fiveHour"]["pct"], 30.0);
    assert_eq!(cli.credential(2)["claudeAiOauth"]["refreshToken"], CLAUDE_ROTATED_REFRESH);
    assert_eq!(cli.credential(2)["claudeAiOauth"]["accessToken"], CLAUDE_REFRESHED);
    assert_eq!(mock.claude_token_calls(), 1);
    let one = payload["accounts"].as_array().unwrap().iter().find(|r| r["number"] == 1).unwrap();
    assert_eq!(one["usage"]["spend"], json!({"used": 7.29, "limit": 50.0, "pct": 14.58, "currency": "USD"}));
    assert_eq!(one["usage"]["scoped"][0]["name"], CLAUDE_POOL_NAME);
    assert_eq!(one["usage"]["scoped"][0]["pct"], 62.0);
    let _ = CLAUDE_LIMIT_7D;
}

#[test]
fn stale_claude_lock_is_taken_over_and_a_fresh_one_fails() {
    let cli = mixed();
    let stale = cli.claude_home.join(".oauth_refresh.lock");
    std::fs::create_dir(&stale).unwrap();
    let old = filetime::FileTime::from_unix_time(filetime::FileTime::now().unix_seconds() - 120, 0);
    filetime::set_file_mtime(&stale, old).unwrap();
    let run = cli.run(&["switch", "2"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!stale.exists());

    std::fs::create_dir(&stale).unwrap();
    let run = cli.run(&["switch", "3"]);
    assert_eq!(run.status, 1);
    assert!(run.stderr.starts_with("Error: Claude Code is holding "), "{}", run.stderr);
    assert!(run.stderr.trim().ends_with("; retry in a moment"));
    assert_eq!(cli.claude_config()["oauthAccount"]["emailAddress"], "One@example.com", "nothing changed");
    let run = cli.run(&["switch", "3", "--json"]);
    assert_eq!(run.json()["error"]["type"], "LockError");
    std::fs::remove_dir(&stale).unwrap();
}
```

Add `filetime` to `[dev-dependencies]` (it is already a dependency; listing it again under dev-dependencies is not needed — integration tests can use any regular dependency, so leave `Cargo.toml` as it is).

- [ ] **Step 3: Run the new tests**

Run: `cargo test --test cli_claude`
Expected: 11 passed. Any failure here is a real behaviour gap in Tasks 11–15: fix the implementation, not the test, unless the test contradicts the spec.

- [ ] **Step 4: Run the whole suite and the gate**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add tests/support tests/cli_claude.rs
git commit -m "test: drive the mixed Codex/Claude roster end to end through the binary"
```

---

### Task 17: The TUI groups by provider

**Files:**
- Modify: `src/tui/snapshot.rs`, `src/tui/test_support.rs`, `src/tui/widgets.rs`, `src/tui/switch.rs`, `src/tui/dashboard.rs`, `src/tui/modals.rs`, `src/tui/app.rs`, `src/tui/worker.rs`, `src/tui/watch.rs` (one call site)
- Test: unit tests in `snapshot.rs`, `widgets.rs`, `switch.rs`, `dashboard.rs`, `app.rs`

**Interfaces:**
- Produces: `AccountSnapshot.provider: Provider`; `AccountsSnapshot::is_mixed()`, `AccountsSnapshot::grouped() -> Vec<(Provider, Vec<&AccountSnapshot>)>`; `widgets::section_header(Provider, &Palette) -> Line`; `Action::SwitchBest(Provider)`; `MenuId::Header`; `SwitchScreen::handle_key(key, snapshot)`; `test_support::claude_account(number, email, is_active, usage)`.
- Consumes: Task 14 (`ListSnapshot.actives`), Task 12 (`add_accounts`).

- [ ] **Step 1: Write the failing tests**

`src/tui/test_support.rs`: `account()` sets `provider: Provider::Codex`; add

```rust
pub(crate) fn claude_account(number: u32, email: &str, is_active: bool, usage: UsageEntry) -> AccountSnapshot {
    let mut account = account(number, email, is_active, usage);
    account.provider = crate::provider::Provider::Claude;
    account
}
```

`src/tui/snapshot.rs` tests:

```rust
    #[test]
    fn grouping_and_mixed_detection() {
        use crate::provider::Provider;
        use crate::tui::test_support::{account, claude_account, entry};
        let single = crate::tui::test_support::snapshot(vec![account(1, "a@x.y", true, entry(None, None))], 1.0);
        assert!(!single.is_mixed());
        assert_eq!(single.grouped().len(), 1);
        let mixed = crate::tui::test_support::snapshot(
            vec![
                account(1, "a@x.y", true, entry(None, None)),
                claude_account(2, "c@x.y", true, entry(None, None)),
                account(3, "b@x.y", false, entry(None, None)),
            ],
            1.0,
        );
        assert!(mixed.is_mixed());
        let groups = mixed.grouped();
        assert_eq!(groups[0].0, Provider::Codex);
        assert_eq!(groups[0].1.iter().map(|a| a.number).collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!(groups[1].0, Provider::Claude);
        assert_eq!(groups[1].1[0].number, 2);
        assert_eq!(mixed.active_number, Some(1), "the lowest active slot");
    }
```

and in `from_list_maps_rows` set `record.provider = Provider::Claude` and assert `account.provider == Provider::Claude`.

`src/tui/widgets.rs` tests (add a `tests` module if there is none):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;
    use crate::tui::test_support::{account, claude_account, entry, snapshot};
    use crate::tui::theme::DARK;

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn mixed_panel_gets_a_header_per_provider_and_single_does_not() {
        let mixed = snapshot(
            vec![
                account(1, "a@x.y", true, entry(Some(900.0), Some(10.0))),
                claude_account(2, "c@x.y", true, entry(Some(900.0), Some(20.0))),
                claude_account(3, "d@x.y", false, entry(Some(900.0), Some(30.0))),
            ],
            1000.0,
        );
        let lines = texts(&accounts_panel(Some(&mixed), 90, Some(90.0), true, 1000.0, &DARK));
        assert_eq!(lines[0], "codex");
        assert!(lines[1].starts_with(" 1  a@x.y"), "{}", lines[1]);
        let claude_at = lines.iter().position(|l| l == "claude").unwrap();
        assert_eq!(lines[claude_at - 1], "", "blank line between sections");
        assert!(lines[claude_at + 1].starts_with(" 2  c@x.y"), "the card follows its header directly");
        assert!(lines[claude_at + 2].starts_with("    5h"));
        assert_eq!(lines[claude_at + 3], "");
        assert!(lines[claude_at + 4].starts_with(" 3  d@x.y"), "{}", lines[claude_at + 4]);
        assert_eq!(section_header(Provider::Claude, &DARK).spans[0].content, "claude");

        let single = snapshot(vec![account(1, "a@x.y", true, entry(Some(900.0), Some(10.0)))], 1000.0);
        let lines = texts(&accounts_panel(Some(&single), 90, None, true, 1000.0, &DARK));
        assert!(lines[0].starts_with(" 1  a@x.y"), "no header for one provider: {}", lines[0]);
    }

    #[test]
    fn empty_state_mentions_both_tools() {
        let empty = crate::tui::snapshot::AccountsSnapshot::empty(1.0);
        let lines = texts(&accounts_panel(Some(&empty), 90, None, true, 1.0, &DARK));
        assert_eq!(lines[1], "Use the menu below: Add account — from your current Codex or Claude Code login, or from a token.");
    }
}
```

`src/tui/switch.rs` tests: update the existing calls to `screen.handle_key(key(…))` to pass `Some(&snap)`; add:

```rust
    #[test]
    fn best_pick_targets_the_provider_under_the_cursor_and_headers_are_skipped() {
        let mut screen = SwitchScreen::new();
        let snap = snapshot(
            vec![
                account(1, "a@x.y", true, entry(Some(900.0), Some(10.0))),
                claude_account(2, "c@x.y", true, entry(Some(900.0), Some(20.0))),
            ],
            1000.0,
        );
        screen.sync(&snap, 1000.0);
        assert_eq!(screen.list.cursor(), Some(0));
        assert_eq!(
            screen.handle_key(key(KeyCode::Char('b')), Some(&snap)),
            vec![Effect::Action(Action::SwitchBest(Provider::Codex))]
        );
        screen.handle_key(key(KeyCode::Char('j')), Some(&snap));
        assert_eq!(
            screen.handle_key(key(KeyCode::Char('b')), Some(&snap)),
            vec![Effect::Action(Action::SwitchBest(Provider::Claude))]
        );
        let lines = screen.list.render_lines(Some(&snap), 80, 20, 1000.0, &crate::tui::theme::DARK);
        let text: Vec<String> = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert_eq!(text[0], "  codex");
        assert!(text.iter().any(|t| t == "  claude"), "{text:?}");
        assert!(text.iter().any(|t| t.starts_with("▌  2  c@x.y")), "{text:?}");
    }
```

(`use crate::provider::Provider; use crate::tui::test_support::claude_account;` in that module.)

`src/tui/dashboard.rs` tests: in `sample()` make slot 3 a `claude_account`; in `disable_toggles_without_confirm_and_returns_to_root` expect the labels

```rust
            [
                "codex",
                "1  a@x.y   → disable",
                "2  b@x.y   → disable",
                "claude",
                "3  c@x.y  (disabled)   → enable",
                "← back"
            ]
```

with the cursor starting at index 1 (`assert_eq!(dash.cursor(), 1)` after the push), moving `j` twice landing on index 4 (the header at 3 is skipped), and `dash.cursor = 4` before the Enter. In `submenus_breadcrumb_and_back`, the add menu reads `["Add new account", "From current logins", "From a token…", "← back"]`, and the remove submenu's first row is now `"codex"` followed by `"1  a@x.y  [personal]"` (cursor at 1).

`src/tui/app.rs` tests: in `actions_are_single_flight_and_end_with_a_refresh` the `b` key still returns the busy toast; add to `no_switch_failure_and_add_results` an `Action::SwitchBest(Provider::Codex)` label check: `assert_eq!(Action::SwitchBest(Provider::Claude).label(), "Switch (best, claude)")`.

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib tui::`
Expected: compile errors.

- [ ] **Step 3: Implement**

`src/tui/snapshot.rs`:

```rust
use crate::provider::Provider;

pub struct AccountSnapshot {
    pub number: u32,
    pub provider: Provider,
    // … the rest unchanged …
}

impl AccountsSnapshot {
    pub fn is_mixed(&self) -> bool {
        Provider::ALL.iter().filter(|p| self.accounts.iter().any(|a| a.provider == **p)).count() > 1
    }

    /// The accounts of each present provider, in `Provider::ALL` order.
    pub fn grouped(&self) -> Vec<(Provider, Vec<&AccountSnapshot>)> {
        Provider::ALL
            .into_iter()
            .filter_map(|provider| {
                let group: Vec<&AccountSnapshot> = self.accounts.iter().filter(|a| a.provider == provider).collect();
                (!group.is_empty()).then_some((provider, group))
            })
            .collect()
    }
}
```

`from_list` sets `provider: row.record.provider` and `active_number: list.actives.lowest()`; `same_account` also compares `provider`. Every `AccountSnapshot { … }` literal (`test_support.rs`, the two in `snapshot.rs` tests, `tests/tui_render.rs`, `examples/tui_screenshot.rs`) gains `provider: Provider::Codex`.

`src/tui/widgets.rs`:

```rust
/// `codex` / `claude` above a provider's accounts when the roster is mixed.
pub fn section_header(provider: Provider, p: &Palette) -> Line<'static> {
    Line::from(Span::styled(provider.as_str().to_string(), p.muted_style()))
}
```

`accounts_panel`: replace the account loop with

```rust
    let mixed = snapshot.is_mixed();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (provider, group) in snapshot.grouped() {
        let mut headed = false;
        if mixed {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(section_header(provider, p));
            headed = true;
        }
        let mut last_was_card = false;
        for acc in group {
            if acc.is_active {
                let card = account_card(acc, width, threshold, now, p);
                if !lines.is_empty() && !headed {
                    lines.push(Line::default());
                }
                lines.extend(card);
                last_was_card = true;
            } else if show_minis {
                if last_was_card {
                    lines.push(Line::default());
                }
                lines.push(mini_line(acc, now, p));
                last_was_card = false;
            }
            headed = false;
        }
    }
```

and the empty-state text becomes `Use the menu below: Add account — from your current Codex or Claude Code login, or from a token.`.

`src/tui/switch.rs` `CardList::render_lines`: inside the `if let Some(snapshot)` block, before the per-account loop compute `let mixed = snapshot.is_mixed(); let mut last_provider = None;` and at the top of each iteration:

```rust
                if mixed && last_provider != Some(acc.provider) {
                    if !lines.is_empty() {
                        lines.push(Line::default());
                    }
                    lines.push(indent_header(section_header(acc.provider, p)));
                    last_provider = Some(acc.provider);
                }
```

where `fn indent_header(line: Line<'static>) -> Line<'static>` prefixes two raw spaces (so the header aligns with the number column of the cards, whose first two cells are the cursor marker and a space). `SwitchScreen::handle_key(&mut self, key, snapshot: Option<&AccountsSnapshot>)`: the `b` arm becomes

```rust
            KeyCode::Char('b') => {
                let provider = self
                    .list
                    .selected()
                    .and_then(|n| snapshot.and_then(|s| s.account(n)))
                    .map(|a| a.provider)
                    .unwrap_or_default();
                vec![Effect::Action(Action::SwitchBest(provider))]
            }
```

`src/tui/app.rs`: `Action::SwitchBest(Provider)` with label `format!("Switch (best, {provider})")`; `Screen::Switch(s) => s.handle_key(key, snapshot.as_ref())`.

`src/tui/dashboard.rs`: add `Header` to `MenuId`; `account_rows` becomes

```rust
    fn account_rows(snapshot: Option<&AccountsSnapshot>, make: impl Fn(&AccountSnapshot) -> MenuEntry) -> Vec<MenuEntry> {
        let Some(snapshot) = snapshot else {
            return Vec::new();
        };
        let mixed = snapshot.is_mixed();
        let mut rows = Vec::new();
        for (provider, group) in snapshot.grouped() {
            if mixed {
                rows.push(entry(provider.as_str(), MenuId::Header));
            }
            rows.extend(group.into_iter().map(&make));
        }
        rows
    }
```

`push` sets the cursor to the first non-header entry (`self.cursor = level.entries.iter().position(|e| e.id != MenuId::Header).unwrap_or(0)`); `move_cursor` steps past headers in the direction of travel:

```rust
    fn move_cursor(&mut self, delta: i64) {
        let entries = self.entries();
        let len = entries.len() as i64;
        if len == 0 {
            return;
        }
        let step = delta.signum();
        let mut next = (self.cursor as i64 + delta).clamp(0, len - 1);
        while entries[next as usize].id == MenuId::Header {
            let candidate = next + step;
            if candidate < 0 || candidate >= len {
                return;
            }
            next = candidate;
        }
        self.cursor = next as usize;
    }
```

`Home` / `End` land on the first / last non-header entry; `activate` adds `MenuId::Header => Vec::new()`; `menu_lines` renders a header entry as `Span::styled(format!("  {}", entry.label), p.muted_style())` with no highlight. The add submenu entries read `Add new account` (unchanged), `From current logins` (`MenuId::AddLogin`), `From a token…` (`MenuId::AddToken`).

`src/tui/modals.rs`: `ConfirmModal::add_current` message `Back up the current Codex and Claude Code logins as managed accounts?\n\nA login that is already managed has its stored credentials refreshed in place.`; the token modal title `Add account from a token`, body `OpenAI API key (sk-…), or an Anthropic setup-token / API key (sk-ant-…); API-key accounts have no usage quota.`, first field label `token (required)`; update the `confirm_keys` assertion (`starts_with("Back up the current Codex and Claude Code logins")`).

`src/tui/worker.rs` `perform`: `Action::SwitchBest(provider) => switcher.switch(Some(*provider), Strategy::Best, &models, false)`; `Action::AddCurrent => { switcher.add_accounts(None, None, None)?; Ok(None) }` (already from Task 12). `src/tui/watch.rs` needs no change (it does not call `SwitchScreen::handle_key`); check `grep -n "handle_key(key)" src/tui` for any other caller.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib tui:: && cargo test --test tui_render` — the render test will fail on the renamed labels and the empty-state text; those assertions are updated in Task 18. Then `cargo clippy --all-targets -- -D warnings && cargo fmt`.

- [ ] **Step 5: Commit**

```bash
git add src/tui examples/tui_screenshot.rs tests/tui_render.rs
git commit -m "feat(tui): a section per provider on the dashboard, switch and watch screens; provider-aware menus and best pick"
```

(Include the minimal `provider: Provider::Codex` additions to `examples/tui_screenshot.rs` and `tests/tui_render.rs` so the crate compiles; their content changes land in Task 18.)

---

### Task 18: Render tests, README screenshots, README and CHANGELOG

**Files:**
- Modify: `tests/tui_render.rs`, `examples/tui_screenshot.rs`, `docs/tui-dashboard.png`, `docs/tui-watch.png`, `README.md`, `CHANGELOG.md`, `Cargo.toml` (description, keywords)

**Interfaces:**
- Consumes: Task 17.

- [ ] **Step 1: Update and extend the render tests**

In `tests/tui_render.rs`: `dashboard_menu_navigation_and_breadcrumb` expects `rows[y + 3] == "   From current logins"` and `rows[y + 4] == "   From a token…"`; `dashboard_loading_and_empty_states` expects `rows_empty[2].contains("from your current Codex or Claude Code login, or from a token")`. Add:

```rust
fn mixed_fixture() -> AccountsSnapshot {
    let mut snapshot = fixture();
    let mut bob = account(
        6,
        "bob@gmail.com",
        "Personal",
        entry(
            10.0,
            Some(NormalizedUsage {
                five_hour: Some(window(40.0, 70 * 60)),
                seven_day: Some(window(100.0, 2 * 86_400 + 4 * 3600)),
                scoped: vec![ScopedWindow { name: "Fable".into(), pct: 100.0, resets_at: Some(format_iso(NOW as i64 + 2 * 86_400 + 4 * 3600)) }],
                spend: Some(cswitch::model::Spend { used: 12.5, limit: 50.0, pct: 25.0, currency: "USD".into(), resets_at: None }),
                ..NormalizedUsage::default()
            }),
        ),
    );
    bob.provider = Provider::Claude;
    bob.is_active = true;
    let mut work = account(7, "bob@work.com", "Work", entry(10.0, Some(NormalizedUsage { five_hour: Some(window(3.0, 3600)), seven_day: Some(window(22.0, 86_400)), ..NormalizedUsage::default() })));
    work.provider = Provider::Claude;
    snapshot.accounts.push(bob);
    snapshot.accounts.push(work);
    snapshot
}

#[test]
fn mixed_roster_shows_a_section_per_provider() {
    let mut app = App::new(TuiStart::Dashboard, ThemeName::Dark, 90.0, None);
    app.apply_snapshot(mixed_fixture(), 1, NOW);
    let buf = render(&mut app, 100, 44, NOW);
    let rows = screen_rows(&buf);
    let (codex_y, codex) = find_row(&rows, "   codex");
    assert_eq!(codex, "   codex");
    assert_eq!(fg(&buf, 3, codex_y), DARK.muted);
    let (alice_y, _) = find_row(&rows, "alice@corp.io");
    assert_eq!(alice_y, codex_y + 1, "the first account follows its header");
    let (claude_y, claude) = find_row(&rows, "   claude");
    assert_eq!(claude, "   claude");
    assert_eq!(rows[claude_y - 1], "");
    let (bob_y, bob) = find_row(&rows, "bob@gmail.com");
    assert_eq!(bob_y, claude_y + 1);
    assert!(bob.starts_with("    6  bob@gmail.com  [Personal]   ● active"), "{bob}");
    let (_, spend) = find_row(&rows, "$$    ");
    assert!(spend.contains("  25%  "), "{spend}");
    assert!(spend.ends_with("$12.50 / $50.00"), "{spend}");
    let (_, work) = find_row(&rows, "bob@work.com");
    assert_eq!(work, "    7  bob@work.com  [Work]   5h 3% · 7d 22%");

    app.handle_key(key(KeyCode::Char('s')), NOW);
    let rows = screen_rows(&render(&mut app, 100, 50, NOW));
    find_row(&rows, "   codex");
    find_row(&rows, "   claude");
    let (active_y, active) = find_row(&rows, "john.doe@gmail.com");
    assert!(active.starts_with(" ▌  2  "), "cursor on the lowest active: {active}");
    for _ in 0..4 {
        app.handle_key(key(KeyCode::Char('j')), NOW);
    }
    let rows = screen_rows(&render(&mut app, 100, 50, NOW));
    let (_, bob) = find_row(&rows, "bob@gmail.com");
    assert!(bob.starts_with(" ▌  6  "), "{bob}");
    assert_eq!(
        app.handle_key(key(KeyCode::Char('b')), NOW),
        vec![Command::Action(Action::SwitchBest(Provider::Claude))]
    );
    let _ = active_y;
}
```

(`use cswitch::provider::Provider;` at the top.) Add a mixed-roster check to the auto screen test only if `draw_auto` changed; it did not.

- [ ] **Step 2: Run the render tests**

Run: `cargo test --test tui_render`
Expected: all pass.

- [ ] **Step 3: Extend the screenshot fixture and regenerate the PNGs**

In `examples/tui_screenshot.rs` give `account()` a `provider: Provider` parameter (the three Codex accounts pass `Provider::Codex`) and append two Claude accounts to `fixture()`:

```rust
    let mut claude_personal = account(
        4,
        "bob@gmail.com",
        "Personal",
        None,
        Provider::Claude,
        entry(
            30.0,
            NormalizedUsage {
                five_hour: Some(window(40.0, 70 * 60)),
                seven_day: Some(window(100.0, 2 * 86_400 + 4 * 3600)),
                scoped: vec![ScopedWindow {
                    name: "Fable".into(),
                    pct: 100.0,
                    resets_at: Some(format_iso(NOW as i64 + 2 * 86_400 + 4 * 3600)),
                }],
                spend: Some(Spend { used: 12.5, limit: 50.0, pct: 25.0, currency: "USD".into(), resets_at: None }),
                ..NormalizedUsage::default()
            },
        ),
    );
    claude_personal.is_active = true;
    let claude_work = account(
        5,
        "bob@work.com",
        "Work",
        None,
        Provider::Claude,
        entry(
            12.0,
            NormalizedUsage {
                five_hour: Some(window(3.0, 3600)),
                seven_day: Some(window(22.0, 86_400)),
                ..NormalizedUsage::default()
            },
        ),
    );
```

(`active_number` stays `Some(2)`, `accounts: vec![dev, personal, work, claude_personal, claude_work]`), and raise `ROWS` to 36 so both sections fit. Then:

```bash
cargo run --example tui_screenshot -- target/shots
for page in dashboard watch; do
  profile=$(mktemp -d)
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --window-size=980,740 \
    --force-device-scale-factor=2 --user-data-dir="$profile" \
    --screenshot="docs/tui-$page.png" "file://$PWD/target/shots/$page.html" &
  until [ -s "docs/tui-$page.png" ]; do sleep 0.5; done
  pkill -f "$profile"; rm -rf "$profile"
done
cp docs/tui-dashboard.png docs/tui-watch.png ~/showme/
```

(Chrome's `--headless=new` writes the PNG and never exits; the loop polls for the file and kills that profile's process.) Open both images and check that the `codex` and `claude` headers, the `$$` row and the `(!)` marker are visible; adjust `ROWS` if the menu is cut off.

- [ ] **Step 4: README, CHANGELOG, Cargo metadata**

`README.md`:

- First paragraph: `Multi-account switcher for the OpenAI Codex CLI and Claude Code. Keep several Codex and Claude logins on one machine in one roster, switch any of them without logging in again, …`.
- `## Install`: add after the `cli_auth_credentials_store` paragraph: `Claude Code's login is read from the macOS Keychain (service "Claude Code-credentials") or from ~/.claude/.credentials.json, and the account identity from ~/.claude.json; CLAUDE_CONFIG_DIR is honoured. Set CSWITCH_KEYCHAIN=off to use the file backend only.`
- `### Add your first account`: `cswitch add` captures the current Codex login and the current Claude Code login; `cswitch add claude` / `cswitch add codex` for one.
- `### Add more accounts`: the Claude note: log in with `claude` (`/login`), then `cswitch add claude`; never `/logout` first.
- `### Switch accounts`: `cswitch switch 5   # by slot — slots are one list across Codex and Claude`, `cswitch switch claude   # rotate among the Claude accounts`, `cswitch switch claude --strategy best`; the follow-up note for Claude (Keychain ~30 s / file immediately).
- `### See every account's usage`: `cswitch list` prints a Codex block and a Claude block; Claude rows add `$$` (extra-usage spend) and per-model windows (`Fable: 62%`); `cswitch list claude`.
- `### JSON output for scripting`: `schemaVersion: 2`; rows carry `provider`; `active` is `{"codex": n, "claude": n}` on `list` and a per-provider object on `status`; `switch` carries `provider`.
- `### Other commands`: `cswitch add-token sk-ant-oat01-...   # Anthropic setup-token`, `cswitch add-token sk-ant-api03-...   # Anthropic API key`.
- `## Data locations`: `backups/claude/` (the outgoing Claude login, three kept), the Claude lock directories are created and removed inside `~/.claude` during a switch, `CLAUDE_CONFIG_DIR`, `CSWITCH_KEYCHAIN`.
- `## Development`: `CSWITCH_CLAUDE_USAGE_URL` / `CSWITCH_CLAUDE_TOKEN_URL`, `CSWITCH_KEYCHAIN=off` in tests, `CSWITCH_CLAUDE_LOCK_BUDGET_MS`.
- Design line: add `docs/specs/2026-10-07-cswitch-claude-provider-design.md` and this plan.

`CHANGELOG.md`: a new `## Unreleased` section above `v0.2.0` with `### Added` (Claude accounts: `add` captures both logins, `add claude|codex`, `add-token` recognises `sk-ant-oat…` / `sk-ant-api…`, `switch <slot>` across providers, `switch claude|codex`, two-block `list`, per-provider `status`, the TUI sections, `$$` spend row, `backups/claude/`, `CSWITCH_KEYCHAIN`) and `### Changed` (`--json` is `schemaVersion: 2`: `provider` on rows and switch refs, `active` maps on `list` and `status`; `codex` / `claude` are reserved alias names; `sequence.json` records carry `provider` and the roster `activeByProvider`).

`Cargo.toml`: `description = "Multi-account switcher for the OpenAI Codex CLI and Claude Code (cswap-compatible interface)"`, keywords add `"claude"`.

- [ ] **Step 5: Final gate and commit**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: green.

```bash
git add tests/tui_render.rs examples/tui_screenshot.rs docs/tui-dashboard.png docs/tui-watch.png README.md CHANGELOG.md Cargo.toml Cargo.lock
git commit -m "docs: mixed-roster screenshots, README and changelog for the Claude provider (phase 1)"
```

---

## Deviations from the spec recorded by this plan

- `src/claude/paths.rs` is folded into `src/paths.rs` (Task 4); `src/claude/live.rs` holds the live read/write (Task 7). `src/claude/session.rs` arrives with phase 3.
- `CollectOptions.active: Option<u32>` becomes `actives: Vec<u32>` so one pass serves both live logins (Task 11); the auto-switch engine passes a one-element vector until phase 2.
- `add --slot` / `--alias` with two live logins is refused outright (even when only one of them is new), with a message naming the selector (Task 12). This is stricter than §6.2 and avoids placing two records into one requested slot.
- The `add-token` cross-kind collision keeps v0.1's wording for every token kind (Task 12).
- `list <provider>` on a mixed roster prints the titled block (`Claude accounts:`); `Accounts:` is printed only when the roster itself has one provider (Task 15).
- `CSWITCH_CLAUDE_LOCK_BUDGET_MS` (tests only) shortens the lock wait (Task 8).

## Self-review notes

- Spec coverage: §3 (Task 1), §4 (Tasks 5–10), §5 (Tasks 1, 4, 7), §6.1–6.3 (Tasks 3, 12–15), §7 (Task 13), §8 (Tasks 10–11), §12 (Task 17), §13 (Task 8's `LockError`, Task 3's sentinel), §14 (file map above), §15 (Tasks 11, 16, 18). §9 auto-switch, §10 session mode and §11 export/import are phases 2–4 by design; `purge` needs no change because `backups/` lives under the backup root it already removes.
- Type consistency: `Provider` (Task 1) is used by every later task; `ActiveSlots` (Task 3) by Tasks 14, 15, 17; `SlotFile` / `ClaudeCredential` (Task 6) by Tasks 7, 11–13, 16; `LiveLogin` / `ClaudeLive` (Task 7) by Tasks 11–13; `ClaudeTokens` (Task 9) by Task 11; `CollectOptions.actives` (Task 11) by Task 14; `add_accounts` (Task 12) by Tasks 15, 17; `switch(provider, …)` (Task 14) by Tasks 15, 17; `Action::SwitchBest(Provider)` (Task 17) by Task 18.
