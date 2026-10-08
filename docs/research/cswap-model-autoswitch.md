# cswap (claude-swap) — data model, settings, usage handling, auto-switch: spec notes for the `ccsw` (Codex) Rust port

Sources read (all verbatim, exhaustive):

- Go port design docs: `cswap-go-docs/01-switcher-accounts.md`, `02-switcher-switch-list.md` (§1–13), `03-credentials-locks.md`, `04-oauth-usage.md`, `05-autoswitch.md`, `06-session-mappings.md`, `07-transfer-migrations.md`, `DESIGN.md` (§2.11, §2.12, A15, A17), `reference.md` (auto/config/FILES/SETTINGS/ENVIRONMENT/EXIT/JSON).
- Python reference (newer, dated 2026-08-12): `models.py`, `paths.py`, `settings.py`, `switcher.py` (skimmed by section), `autoswitch.py` (full), `poll_policy.py` (full), `pace.py` (full), `usage_store.py` (full), `cache.py`, `mappings.py`, `session.py` (constants + setup path), `locking.py`, `transfer.py` (full), `json_output.py`, `oauth.py` (§ fetch orchestration), `exceptions.py`, `logging_config.py`, `snapshot_source.py`, `fsutil.py`, `cli.py` (auto flags).

**Precedence rule used in these notes:** where the Go docs and the Python source disagree, the **Python source wins** (it is the newer reference). Every such divergence is flagged with **[PY-NEWER]**. Go-side-only extensions (no Python counterpart) are flagged **[GO-ONLY]**.

Headline divergences [PY-NEWER] (details in the sections below):

| Topic | Go docs say | Python (current) says |
|---|---|---|
| `autoswitch.strategy` choices | `best` only (docs 05); `best`/`soonest-reset` (DESIGN A17, Go-only) | `best` / `consume-first` |
| Usage-store claim window | `CLAIM_TTL_S = 10` via `lastAttemptAt` | `CLAIM_TTL_S = 90` via fenced `claimId`/`claimUntil`; `LEGACY_CLAIM_TTL_S = 10` fallback |
| Retry-After honoring | `min(retry_after, 900)` floor | `retry_after + 900 margin` when `> 600`, capped at `4500` (429 only); non-429 capped at `3600` |
| Permanent auth errors | `{"invalid_grant"}` | `{"invalid_grant", "no_refresh_token"}` |
| Dead-token strike binding | slot-scoped | bound to `struckFingerprint` of the POSTed generation |
| Trust ceiling after 429 | 3600 s | `min(earliest future relevant reset, fetchedAt + 7200)` |
| Exhausted accounts | park until reset | keep polling at `EXHAUSTED_INTERVAL_S = 600`, capped at reset+60 |
| Post-429 cadence | floor 360 | AIMD: `max(base*1.5, 360)` up to `1800` |
| `MAX_SLEEP_S` (auto) | 21600 (6h) | `= EXHAUSTED_INTERVAL_S = 600` |
| Auto state file | `lastSwitchAt`, `lastSwitchTo`, `quarantine` | + `lastSwitchFrom`, `leftHeadroom`, `leftRecoveryAt`, `leftTrigger` |
| No-switch reasons | 12 reasons | + `reset-unknown`, `already-consuming-soonest`, `stale-usage` |
| Refresh error tokens | `invalid_grant`/`no_refresh_token`/`transient` | + `store-unmirrored`, `invalid_client`, `consume-busy`, `stash-unreadable` (deterministic, systemic) |
| Stale session marker | child `<profile>/.cswap-stale-credentials` | sibling `<backup>/sessions/.<profile-dirname>.cswap-stale-credentials` (child still honored on read) |
| Session bootstrap refresh | `refresh_oauth_credentials` under bootstrap lock | `consume_backup_grant` gate **before** the lock |
| Roster read failure | corrupt `sequence.json` → `None` (treated empty) | strict: corrupt/unreadable/non-object → `ConfigError` refusing to overwrite; absent → `None` |
| Import of existing quarantined slot | skip + hint | auto-**replace** (no `--force`) when the slot's identity-matched row is dead; counted as `replaced` |
| Export default | slim config only | slim config **and** slim credentials (`{"claudeAiOauth": ...}` only) unless `--full` |
| Pace (`expectedPct` etc.) | not documented | `pace.py` + JSON fields on weekly windows |

---

## 1. Backup store layout

### 1.1 Backup root (`paths.get_backup_root`)

| Platform | Root |
|---|---|
| Linux / WSL | `$XDG_DATA_HOME/claude-swap` if `XDG_DATA_HOME` is set, non-empty and (after `~` expansion) absolute; else `~/.local/share/claude-swap` |
| macOS / Windows / UNKNOWN | `~/.claude-swap-backup` (legacy layout, `LEGACY_BACKUP_DIRNAME`) |

- Platform detection: `sys.platform` → `darwin`=MACOS, `win32`=WINDOWS, `linux*`= WSL if env `WSL_DISTRO_NAME` non-empty else LINUX, else UNKNOWN. No `/proc/version` sniffing.
- On Linux/WSL, `migrate_legacy_backup_dir(target)` runs first thing in the switcher constructor (before logging/dirs): moves `~/.claude-swap-backup` → target once, guarded by flag file `<target.parent>/.<target.name>.migrating`. State machine:
  - `legacy.resolve() == target.resolve()` → no-op (macOS/Windows).
  - legacy absent → `flag.unlink(missing_ok=True)`, no-op.
  - flag present + legacy present → interrupted run: `rmtree(target)` if it exists, redo move.
  - no flag + target exists with **meaningful data** → `MigrationError("Both legacy ({legacy}) and new ({target}) backup paths exist. Refusing to merge or overwrite — inspect both and remove the stale one manually before re-running.")`. Meaningful = any entry whose name is not in `{"cache"}` and does not start with `claude-swap.log`.
  - no flag + target holds only throwaway artifacts (`cache/`, `claude-swap.log*`) → wipe them, `target.rmdir()`, then migrate.
  - move = `shutil.move` (rename same-FS, copy+unlink cross-FS; must preserve 0600 files / 0700 dirs), bracketed by `flag.touch()` / `flag.unlink()`. Any `OSError` → `MigrationError("Migration of {legacy} → {target} failed: {exc}")`.
  - Returns True only when a move ran → stderr `claude-swap: migrated data from {legacy} to {backup_dir}`.
- Construction order (parity-critical): home/platform → backup root → legacy-dir migration (may abort with `MigrationError`) → derive `sequence_file/configs_dir/credentials_dir/lock_file` → logger + `UsageStore(backup/"cache")` → `CredentialStore` → registry migrations (`run_migrations`, never abort; no-op if backup dir doesn't exist). Directories are **not** created at construction (`_setup_directories` is called by add/import).

### 1.2 Full tree under the backup root

```
<backup>/
  sequence.json                         account roster (§1.3)
  settings.json                         user settings (§1.5 / §7)
  mappings.json                         directory → account mappings (§1.6)
  autoswitch_state.json                 auto-switch cooldown/quarantine/anti-flap state (§1.8)
  .autoswitch_state.lock                FileLock for the state file
  .lock                                 the cswap account lock (FileLock)
  .migrations.json                      registry-migration applied map (§1.10)
  claude-swap.log[.1|.2|.3]             rotating log, 1 MiB × 3 backups (§1.11)
  configs/
    .claude-config-{num}-{email}.json   per-account snapshot of ~/.claude.json (raw text)
  credentials/
    .creds-{num}-{email}.enc            base64(raw credential string); file backend
    .creds-{num}-{email}.enc.prev       one retained previous generation (file backend)
    .swap-staging-{creds|config}-{num}.json   same-email swap staging (0600, O_EXCL)
    .unclaimed-manifest.json            forensic stash manifest (§1.4)
    .unclaimed-{entry_id}.enc           forensic stash entries (0600 base64, every platform)
    .consume-{num}.lock                 per-slot refresh-grant consume lock [PY-NEWER]
  cache/
    usage.json                          usage table, schemaVersion 2 (§1.7)
    .usage.lock                         FileLock for usage.json
    update_check.json                   {"timestamp": <epoch s>, "data": <latest version>} (cache.py)
  sessions/
    {num}-{slugified-email}/            per-account session profile (= CLAUDE_CONFIG_DIR) (§1.9)
    .{num}-{slug}.cswap-stale-credentials   sibling stale marker [PY-NEWER]
```

Email is embedded **raw** (unslugified) in `configs/` and `credentials/` file names; only `sessions/` uses the slug. Everything the store writes is `chmod 0600` (files) / `0700` (dirs) on non-Windows.

macOS Keychain items (generic passwords via `/usr/bin/security`, pinned absolute path):

| Service | Account (username) | Holds |
|---|---|---|
| `claude-swap` (`SECURITY_SERVICE`) | `account-{num}-{email}` | per-account backup credential (macOS while Keychain usable) |
| `claude-swap` | `account-{num}-{email}.prev` | retained previous generation |
| `Claude Code-credentials` | `keychain_account_name()` | Claude Code's **active** OAuth credential |
| `Claude Code` | `keychain_account_name()` | Claude Code's **active** managed API key |
| `Claude Code-credentials-{sha256(NFC(config_dir))[:8]}` | `keychain_account_name()` | Claude Code's credential for a session profile / custom `CLAUDE_CONFIG_DIR` (cswap reads/deletes, never writes) |
| `claude-code` (`KEYRING_SERVICE`) | `account-{num}-{email}` | LEGACY python-keyring backups (migration/purge only) |

`keychain_account_name()` = `$USER` → POSIX username → literal `"claude-code-user"`.

### 1.3 `sequence.json` — the roster

```json
{
  "activeAccountNumber": 1,
  "lastUpdated": "2026-07-17T12:00:00Z",
  "sequence": [1, 2, 3],
  "accounts": {
    "1": {
      "email": "a@example.com",
      "uuid": "acct-uuid",
      "organizationUuid": "",
      "organizationName": "",
      "added": "2026-01-01T00:00:00Z",
      "alias": "dev",
      "kind": "api_key",
      "disabled": true
    }
  }
}
```

Initial file (written by `_init_sequence_file` only if absent):
`{"activeAccountNumber": null, "lastUpdated": "<ts>", "sequence": [], "accounts": {}}`

| Key | Type | Notes |
|---|---|---|
| `activeAccountNumber` | int or null | cswap's *recorded* active slot. The *live* active slot is always re-derived from `~/.claude.json` `oauthAccount` (§10). Written as `int(num)`. Import treats `0` as unset. |
| `lastUpdated` | str | UTC `%Y-%m-%dT%H:%M:%SZ` (seconds, literal `Z`). Bumped on every mutation. |
| `sequence` | list of int | Rotation/list order. **Always sorted ascending** (append then sort in add/add-token/import/move/swap). |
| `accounts` | map str→record | Keys are **string** slot numbers (`"1"`). |

Record fields:

| Key | Type | Required | Semantics |
|---|---|---|---|
| `email` | str | yes | From `oauthAccount.emailAddress`, or synthesized `{api-key|setup-token}-{slot}@token.local`. Half of the identity. |
| `uuid` | str | yes | `oauthAccount.accountUuid`; `""` for add-token accounts. Only ever filled when empty (`backfill_account_uuid`), never rewritten. |
| `organizationUuid` | str | yes (post-migration) | `""` = personal. Other half of the identity. A missing key (pre-v0.6.0 record) triggers the org backfill migration. |
| `organizationName` | str | yes | `""` = personal; display only. Display tag = name or `"personal"`. |
| `added` | str | yes | First-add timestamp; not touched by refresh-in-place. |
| `alias` | str | optional | Present only when set; lowercase; validated (§2.6). Deleted (not blanked) on unset. |
| `kind` | str | optional | Present and `"api_key"` only for managed API-key accounts. Absent ⇒ `"oauth"` (setup-tokens are also oauth). |
| `disabled` | bool | optional | Present and `true` only while held out of rotation; `pop`'d on enable. |

Rules:
- **Absence-signals**: `alias`, `kind`, `disabled` must be **omitted** when unset (never `""`/`false`/`"oauth"`). Duplicate detection and back-compat reads depend on it.
- **Composite identity** = `(email, organizationUuid)`. `_find_account_slot(data, email, org)` returns the first slot where `record.email == email and record.get("organizationUuid","") == org`. Same email under different orgs = two legal accounts.
- `sequence` entries may reference a removed record (stale); readers tolerate it (`_account_is_switchable` → False).

Write discipline (`_write_json`): `json.dumps(data, indent=2)` → temp `path.with_suffix(".{pid}.tmp")` → re-read + `json.loads` (fail → unlink temp, `ConfigError("Generated invalid JSON")`) → `chmod 0600` on the **temp** file → `shutil.move(temp, path)`. The rename is the commit point.

Read discipline (`_read_json(path, strict)`) [PY-NEWER]: absent → `None`. The roster is read **strict**: present-but-unparseable → `ConfigError("{path} exists but could not be parsed ({e}). Repair or move it, then retry — refusing to overwrite it unread.")`; unreadable → `ConfigError("{path} exists but could not be read ({e}). Fix what is blocking the read, then retry.")`; non-object → `ConfigError("{path} holds {type}, not a JSON object. Repair or move it, then retry.")`. Non-strict reads (e.g. `~/.claude.json`) log `Invalid JSON in {path}` and return `None`. Windows reads/renames retry transient sharing errors (`winerror` 5/32/33; 10 attempts, 2 ms doubling to 250 ms).

Org-field backfill (`_migrate_org_fields` / `_get_sequence_data_migrated`): lazily, whenever any record lacks the `organizationUuid` key: for the record whose email equals the live `~/.claude.json` email → take `organizationUuid`/`organizationName` from the live config; otherwise parse the slot's backup config `oauthAccount`; on any failure both `""`. Idempotent, per-key-presence (a present `""` is not migrated). Called at the top of add/add-token/remove/swap/move/alias/disable/export/import.

### 1.4 Credential & config backups

- Config backup: `configs/.claude-config-{num}-{email}.json` = raw text of the account's `~/.claude.json` (whole file for `add`; for add-token a synthetic blob, §2.4), written with `write_text` + `chmod 0600`. Deleted with `unlink(missing_ok=True)` (never `exists()`-guarded).
- Credential backup: the raw credential **string** (OAuth JSON, or a bare `sk-ant-api…` key), stored:
  - Linux/WSL/Windows/UNKNOWN: always `credentials/.creds-{num}-{email}.enc` = standard base64 of the UTF-8 bytes (atomic: `mkstemp` in the same dir → write → `os.replace` → `chmod 0600`).
  - macOS: Keychain item `claude-swap` / `account-{num}-{email}` while the Keychain is usable; `.enc` fallback otherwise. **Reads are `.enc`-wins on every platform**: if the `.enc` exists and decodes (strict base64, `validate=True`) to a non-empty string it wins; corrupt/empty/whitespace `.enc` falls through to the Keychain; a healthy-Keychain read never materializes an `.enc`. After a successful Keychain write the shadow `.enc` is deleted (or rewritten fresh if delete fails; if that also fails → raise). After a file-mode write on macOS the stale Keychain copy is best-effort deleted.
  - Keychain usability cache (per process): `None`→`True` on first success, `→False` on `KEYCHAIN_ERRORS` (= `KeychainError`, subprocess timeout, `OSError`); never `False→True` except after `KEYCHAIN_RECHECK_COOLDOWN_S = 60` s on a **monotonic** clock (daemon self-heal). A **write** that fell back to file **pins** file mode (no re-probe). `security` calls time out at 5.0 s; rc 44 = not found (not an error).
- `.prev` retention: before overwriting a backup with a **different** value, copy the old value to `.creds-{num}-{email}.enc.prev` (file mode) or Keychain `account-{num}-{email}.prev` (Keychain mode). Best-effort; same-value rewrite keeps the existing `.prev`; deleting the account, and any renumber (move/swap) that writes into a key, drops `.prev`.
- Delete (`_delete_account_credentials`): for `num` and the legacy alias `"None"`: unlink `.enc`, quiet Keychain delete, delete `.prev` (both backends). `delete_account_credentials_strict` (move/swap pre-commit clears): after the sweep, unconditional `unlink(missing_ok=True)` + Keychain delete with errors **propagating** as `CredentialError("Could not clear stored credentials for slot {num} ({email}) — aborting before commit: {e}")`, then a read-back that must be empty (else same error without suffix).
- Swap staging (same-email swap only): `credentials/.swap-staging-{creds|config}-{num}.json`, 0600, `O_EXCL`. A leftover file → `ConfigError("Found leftover staging from an interrupted swap: {path}. It holds that slot's pre-swap credentials and may be the only surviving copy. Verify both accounts still work (`cswap list`), then delete the file and retry.")`.
- Unclaimed stash (write-only forensic copies of live credential bytes a switch displaces that belong to someone other than the outgoing slot): always 0600 base64 **files** (never Keychain). `entry_id = "{YYYYMMDDTHHMMSS}-{sha256(creds)[:12]}-{6 hex random}"`. Entry file written **before** the manifest. Manifest:
  ```json
  {"schemaVersion": 1, "entries": {"<entry_id>": {"createdAt": "2026-07-17T12:00:00Z", "reason": "displaced-live-login", "configSlot": "2", "fingerprint": "sha256:…", "liveOauthAccount": {...}, "resolvedIdentity": {...}, "credentialsMtime": 1752000000.0}}}
  ```
  A corrupt manifest is renamed aside to `{name}.corrupt-{int(time)}` before overwrite. Listed only in `--list --json` (`unclaimedCredentials`, sorted ids incl. orphans) and logs; recovery is manual (`/login` + `cswap add --slot N`); `cswap unclaimed` inspects them.
- Consume lock [PY-NEWER]: `credentials/.consume-{num}.lock` (FileLock, default 10 s) serializes every refresh-token POST for a slot (`consume_backup_grant`, §4.8).

### 1.5 `settings.json`

```json
{
  "schemaVersion": 1,
  "autoswitch": {
    "threshold": 90.0,
    "intervalSeconds": 60.0,
    "cooldownSeconds": 300.0,
    "hysteresisPct": 10.0,
    "strategy": "best",
    "includeApiKeyAccounts": false,
    "unhealthyTicks": 3,
    "model": "Fable,Opus"
  },
  "ui": { "theme": "auto" }
}
```

Written via `atomic_write_json` (`mkstemp` beside the **resolved** target, `json.dumps(indent=2)`, `os.replace`, `chmod 0600` file and `0700` on `path.parent` — writes **through** a symlink, never over it). `cswap config set` writes **only** the given key (+ `schemaVersion`), preserving unknown keys/sections; `unset` deletes the key and the section if it becomes empty. Missing/corrupt file on read → defaults + warning; corrupt file on write → `ConfigError("{path} is not valid JSON ({e}); fix or delete it before changing settings")`. Full key table in §7.

### 1.6 `mappings.json`

```json
{
  "schemaVersion": 1,
  "mappings": {
    "/Users/me/work/repo": {
      "email": "work@co.com",
      "organizationUuid": "org-1",
      "added": "2026-07-17T12:00:00Z"
    }
  }
}
```

- Key = `normalize_path(p)` = `Path(p).expanduser().resolve()` then `os.path.normcase` (case-fold + `\` on Windows; no-op on POSIX). `path`, `path/`, `path/.` normalize identically. A non-existent path is allowed (lexical resolve).
- `organizationUuid` is always a string (`org or ""`), never null. `added` rewritten on every `set`.
- Identity is `(email, organizationUuid)`, **never the slot number** (slots get reused).
- Write: `mkdir -p` + `chmod 0700` parent, `mkstemp(prefix=".mappings-", suffix=".tmp")`, `fchmod 0600`, write, `os.replace`; errors **propagate** (unlike share-sync).
- Load: `{}` on missing/corrupt/non-dict root/non-dict `mappings`.
- Never exported/imported (machine-local paths).

### 1.7 `cache/usage.json` (schemaVersion 2)

```json
{
  "schemaVersion": 2,
  "accounts": {
    "1": {
      "email": "a@x.com",
      "organizationUuid": "",
      "lastGood": { "five_hour": {"pct": 22.0, "resets_at": "2026-07-17T15:00:00Z", "countdown": "1h 0m", "clock": "20:39"},
                    "seven_day": {"pct": 61.0, "resets_at": "..."},
                    "spend": {"used": 729.0, "limit": 5000.0, "pct": 14.58, "currency": "USD", "resets_at": "...", "countdown": "...", "clock": "..."},
                    "scoped": [{"name": "Fable", "pct": 100.0, "resets_at": "...", "countdown": "3h 0m", "clock": "..."}] },
      "fetchedAt": 1752000000.0,
      "lastAttemptAt": 1752000000.0,
      "claimId": null,
      "claimUntil": 0.0,
      "consecutiveFailures": 0,
      "lastError": null,
      "backoffUntil": null,
      "nextPollAt": 1752000180.0,
      "pollIntervalS": 180.0,
      "last429At": null,
      "authDeadStrikes": 0,
      "struckFingerprint": null
    }
  }
}
```

| Field | Type | Written by | Meaning |
|---|---|---|---|
| `email`, `organizationUuid` | str | every write | identity guard; a row whose identity differs from the caller's map is invisible on read and **replaced** on write |
| `lastGood` | normalized usage dict or null | success | last successful measurement (stale-on-error: never touched by failures) |
| `fetchedAt` | epoch s (float) | success only | measurement time; `age_s = now - fetchedAt` |
| `lastAttemptAt` | epoch s | claim/record | last claim or record stamp; legacy claim signal |
| `claimId` [PY-NEWER] | hex uuid4 or null | claim/reserve | fencing token; `record()` with claims only applies when `row.claimId == expected` |
| `claimUntil` [PY-NEWER] | epoch s | claim/reserve (`now + 90`), cleared to `0.0` by record/clear | fetch lease; live while `now < claimUntil` |
| `consecutiveFailures` | int | record | reset to 0 on success |
| `lastError` | str or null | record | e.g. `http-429`, `timeout`, `network`, `bad-response`, `invalid_grant`, `no_refresh_token`, `no-access-token`, `refresh-failed`, `store-unmirrored`, `invalid_client`, `consume-busy`, `stash-unreadable`, or an exception type name |
| `backoffUntil` | epoch s or null | failure | `now + _failure_backoff_s(...)`; cleared on success |
| `nextPollAt`, `pollIntervalS` | epoch s / s | success (in the same transaction) or `set_poll_plan` | the adaptive plan every surface inherits |
| `last429At` | epoch s | any `http-429` failure | **never cleared by success** |
| `authDeadStrikes` | int | permanent-auth failure +1; success → 0; `clear_dead_token` → 0 | quarantine at ≥ `AUTH_DEAD_STRIKES = 1` |
| `struckFingerprint` [PY-NEWER] | str or null | permanent-auth failure (always overwritten with `FetchRecord.struck_fp`) | the credential generation the strike condemned; a stored credential whose fingerprint differs heals the strike |

Legacy/foreign handling: missing/corrupt/non-dict/`schemaVersion != 2` → empty table. Written with `atomic_write_json`. Sentinels are **never** persisted.

`cache/update_check.json` (cache.py): `{"timestamp": <epoch s>, "data": <any>}`, plain non-atomic `write_text`, TTL checked on read (`time.time() - timestamp < ttl`); `MISSING` sentinel distinguishes a cached `null`.

### 1.8 `autoswitch_state.json`

```json
{
  "schemaVersion": 1,
  "lastSwitchAt": 1752000000.0,
  "lastSwitchTo": "3",
  "lastSwitchFrom": 1,
  "leftHeadroom": 4.0,
  "leftRecoveryAt": 1752300000.0,
  "leftTrigger": "proactive",
  "quarantine": {
    "2": {
      "email": "b@example.com",
      "reason": "invalid_grant",
      "at": "2026-07-17T12:00:00Z",
      "refreshTokenFingerprint": "sha256:…"
    }
  }
}
```

| Key | Type | Meaning |
|---|---|---|
| `lastSwitchAt` | wall-clock epoch s | set only on a real (non-dry-run) engine switch; drives cooldown |
| `lastSwitchTo` | str slot | landing slot of the last engine switch |
| `lastSwitchFrom` [PY-NEWER] | int slot (from `account_ref`) or null | slot the engine left; the "no-return" anti-flap bar |
| `leftHeadroom` [PY-NEWER] | float or null | departed account's headroom at departure (null = unmeasured) |
| `leftRecoveryAt` [PY-NEWER] | epoch s or null | departed account's binding-window reset at departure (`inf` stored as null) |
| `leftTrigger` [PY-NEWER] | `proactive`/`at-limit`/`failover`/`consume-first` | trigger of that switch (so a `(null,null)` snapshot isn't mistaken for failover) |
| `quarantine` | map slot→entry | `reason` ∈ `invalid_grant`, `identity-conflict`; `at` = ISO seconds `Z`; `refreshTokenFingerprint` = `credential_fingerprint(creds)` or null |

All mutations are read-modify-write under `FileLock(<backup>/.autoswitch_state.lock)`; every write sets `schemaVersion = 1`; unknown keys round-trip; read errors → `{}`. Never mutated while any other lock is held; the switch path never takes this lock (no cycle).

### 1.9 `sessions/{num}-{slug}/` — session profile (a Claude Code `CLAUDE_CONFIG_DIR`)

`slugify_email`: NFC-normalize, then per **character** keep `ch.isascii() and (ch.isalnum() or ch in "._-")`, else `_` (e.g. `user+tag@example.com` → `user_tag_example.com`, `bø@x.com` → `b__x.com`).

Contents cswap writes/reads:

| Path | Owner | Notes |
|---|---|---|
| `.credentials.json` | cswap seeds (raw creds, 0600); Claude rotates | on macOS Claude migrates it into Keychain `Claude Code-credentials-{hash}` which then **shadows** the file |
| `.claude.json` | cswap merges `oauthAccount`, `hasCompletedOnboarding: true`, `theme` (setdefault, from backup config or `"dark"`); Claude owns the rest | never overwritten wholesale on re-bootstrap (profile `projects`/history survive) |
| `.cswap-shared.json` | cswap | `{"items": [...], "mode": "symlink"|"copy"}` share manifest |
| `.cswap-mcp-mirror-v1` | cswap | empty adoption marker for `mcpServers` mirroring |
| `.cswap-mcp-displaced.json` | cswap | `{"schemaVersion": 1, "mcpServers": {...}}` write-once stash of displaced session-local MCP defs |
| `settings.json`, `keybindings.json`, `CLAUDE.md`, `skills/`, `commands/`, `agents/` | symlinks (POSIX) / copies (Windows) into `~/.claude` when sharing | `SHARED_ITEMS` |
| `projects/`, `history.jsonl` | symlinks only with `--share-history` (POSIX only) | `HISTORY_ITEMS` |
| `sessions/<pid>.json`, `ide/<port>.lock` | Claude Code | PID files used for liveness (double `sessions/` nesting is intentional) |
| `.claude.json.lock` | proper-lockfile dir | taken by cswap only for the MCP splice |
| `<sessions>/.{num}-{slug}.cswap-stale-credentials` [PY-NEWER] | cswap | sibling stale marker (`stale_marker_for`); legacy child `<profile>/.cswap-stale-credentials` still honored on read and cleared |

### 1.10 `.migrations.json`

`{"version": 1, "applied": {"windows_keyring_to_files": "<ts>", "macos_keyring_to_security": "<ts>"}}`. `version` is never checked; corrupt → `{}`. Migrations return True (mark applied), False (skip, unmarked), or raise `MigrationIncomplete` (retry next run). The Rust port should honor an existing file so a machine already migrated under Python doesn't re-probe legacy backends.

### 1.11 Log file and the switch log

- `<backup>/claude-swap.log`, `RotatingFileHandler(maxBytes=1 MiB, backupCount=3)`, format `%(asctime)s - %(levelname)s - %(message)s` (Python `asctime` = `YYYY-MM-DD HH:MM:SS,mmm`), file level DEBUG, logger name `claude-swap`. The directory is created lazily on first record.
- **There is no separate switch log file.** "Switch history" is parsed from this log with `re.compile(r"Switched from account (\d+) to (\d+)")` (TUI/menubar show up to 10 most recent, or "No switches logged yet"). The line is emitted at INFO by `_perform_switch` after step 5: `Switched from account {current_account} to {target_account}` (`current_account` may be `None`). Direct-activation logs `Activated account {n} (forced, backup of current login skipped)` / `Activated account {n} (no prior live account)` instead. Other stable log lines that tooling greps: `Usage fetch failed for account N: http-429, retry-after Ks` (WARNING, never contains the email).

### 1.12 Atomic write helpers (four variants, same outcome)

`_write_json` (roster/config: with-suffix temp, JSON round-trip validation, chmod temp, `shutil.move`), `atomic_write_json` (settings/usage/state/session JSON: `mkstemp`, `os.replace`, chmod after, through-symlink), `_atomic_b64_write` (`.enc`), `transfer._atomic_write_file` (export file), `mappings._write` (`fchmod` before write). The port may unify them; keep the roster's JSON round-trip check and "temp in the same directory as the target".

---

## 2. Slot semantics

### 2.1 Numbering
- Slots are positive integers ≥ 1, stored as string keys in `accounts` and ints in `sequence`/`activeAccountNumber`. `"01"` is normalized with `str(int(x))` only in move/swap; identifier resolution returns digit strings unchanged.
- **Sparse slots are legal**: `remove` leaves gaps, `add` never fills them.
- **Next free slot** (`_get_next_account_number`) = `max(int(k) for k in accounts) + 1`, or `1` when there are no accounts. Gaps are never reused by `add`/`import`; only `--slot N` / `move` can target a gap.
- `move` target cap: `cap = max(99, max_existing_slot)`; `target > cap` → `ValidationError("Target slot {target} is out of range (1-{cap}): new accounts are numbered from the highest slot, so a large target would inflate future account numbers")`.

### 2.2 `add` (capture the live login) — dedupe and `--slot`
Preamble: `_setup_directories`, `_init_sequence_file`, `_migrate_org_fields`; alias normalized if given. Live identity `(email, organizationUuid or "")` from `~/.claude.json` `oauthAccount`; none → `ConfigError("No active Claude account found. Please log in first.")`.

- **Refresh-in-place** (no `--slot` and identity already managed): resolve slot; alias conflict check (`ValidationError("Alias '{alias}' is already used by account {n}")`); read live creds (`None` → `CredentialReadError("Failed to read credentials for current account")`, `""` → `CredentialReadError("No credentials found for current account")`); reject a live API key (`ValidationError("Active login is an API-key account. Add it with 'cswap --add-token sk-ant-api...' instead of --add-account.")`); read live config text; write creds + config backups; `clear_dead_token([slot])`; set alias if given (existing alias kept otherwise); `activeAccountNumber = slot`; bump `lastUpdated`. Prints `Updated credentials for Account {n} ({email} [{tag}]).`. `added` unchanged, `uuid` unchanged.
- **Explicit `--slot N`**: `N < 1` → `ConfigError("Slot number must be >= 1")`. If the same identity lives in another slot → `migrate_from = old` (that slot's files are deleted, record removed, mappings **kept**). If slot N holds a **different** identity → `warning("Slot {N} already occupied")`, print `{existing_email} [{tag}]`, prompt `Overwrite slot {N}? [y/N] ` unless `--yes`; answer not in `{"y","yes"}` (or EOF/Ctrl-C → `\nCancelled`) → `Cancelled`, return. On confirm → `displace_slot`: delete that slot's files, drop from `sequence`/`accounts`, **prune its mappings**. All destructive steps happen only after the new credentials were read successfully.
- **Plain add** (new identity, no slot) → next free slot.
- Record written: `{email, uuid, organizationUuid, organizationName, added: now}` + `alias` (explicit, else carried from the displaced-same-identity/migrated record). `sequence` append + sort; `activeAccountNumber = slot`. Prints `Added Account {n}: {email} [{tag}]` (+ dimmed `Moved from slot {old} → {new}` when migrated).
- [PY-NEWER] Capture reads the credential the way Claude resolves it (`_read_capture_credentials`): `CLAUDE_SECURESTORAGE_CONFIG_DIR` defined → read only that profile (empty value = default profile, unsuffixed Keychain item / `~/.claude/.credentials.json`), no fallback to the active store; else `CLAUDE_CONFIG_DIR` set → that profile (strict Keychain: unreadable raises `CredentialReadError`), falling back to the default profile only when it names the same directory, else that profile's own `primaryApiKey`.

### 2.3 `add-token` (setup token or `sk-ant-api…` key)
Token `"-"` → one stdin line (`rstrip("\n")`); empty arg → `getpass("Token: ")`; strip; empty → `ValidationError("Token cannot be empty")`. `is_api_key = looks_like_api_key(token)` = `startswith("sk-ant-api") and not startswith("{")`. `--email` must match `^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$` else `ValidationError("Invalid email format: {email}")`. Email defaulting: if omitted, `slot = slot or next_free`, `email = "{api-key|setup-token}-{slot}@token.local"`. Cross-kind collision: identity `(email, "")` existing as the other kind → `ValidationError("'{email}' already exists as an {API-key|OAuth} account (slot {n}); cannot add it as an {…} account. Pass a distinct --email.")`. Identity always `(email, "")`. Refresh-in-place / `--slot` / displace flow identical to `add`. Payloads in §10.

### 2.4 `remove`
No roster → `ConfigError("No accounts are managed yet")`. Identifier must be digits, a known alias, or a format-valid email (`ValidationError("Invalid account identifier: {id}")`). Email matching several accounts (human mode): list `  {num}: {email} [{tag}]`, prompt `Enter account number to remove: ` (invalid → `Cancelled`). Unknown → `AccountNotFoundError("No account found with identifier: {id}")`. `_ensure_no_live_session(num, email, "--remove-account")` **before** the prompt. Active slot → `warning("Warning: Account-{n} ({email}) is currently active")`. Prompt `Are you sure you want to permanently remove Account-{n} ({email})? [y/N] ` (only `"y"` after lower() proceeds) unless `--yes`. Then, in order: `_delete_account_files` (refuse if live session → creds both backends + `.prev` + legacy `None` alias → config file → session profile dir **and** its hashed Keychain entry (Keychain first) → clear stale markers), `del accounts[num]`, `sequence` filtered, `lastUpdated`, write; log `Removed account {n}: {email}`; print `Removed Account-{n} ({email})`; `_prune_mappings(email, org)` → `Removed {k} directory mapping(s) for this account` if any. **`usage.json` rows are NOT pruned** (identity guard hides them; `clear_dead_token` on re-import clears a lingering strike). `autoswitch_state.json` quarantine entries are released on the next engine tick (`account-replaced`). `activeAccountNumber` is left as-is.

### 2.5 `move` and `swap`
- `move ACCOUNT TARGET`: `TARGET` must be digits ≥ 1 (`ValidationError("Target slot must be a positive slot number, got: {target!r} (use `swap` to trade two accounts by identifier)")`), normalized `str(int)`, ≤ cap. All under one `FileLock`. `src == target` → no-op `(src, target, False)`. Target occupied → **swap** (`(src, target, True)`); empty → **relocate** (`(src, target, False)`).
- Relocate: re-check record + target empty (`ValidationError("Slot {target} is already occupied — retry the move")`); refuse live session (`--move-account`); read src creds/config; best-effort move `sessions/{src}-{slug}` → `sessions/{target}-{slug}`; **write-or-clear** the target key (write creds if any else `delete_account_credentials_strict`; write config if any else unlink); move record; renumber `sequence` then sort; `activeAccountNumber` follows if it was src; write (commit). Pre-commit failure → clear target key, move session dir back, re-raise. Post-commit best-effort: delete src files, drop `.prev` on target, log `Moved slot: {src} ({email}) -> {target}`.
- Swap: resolve both (`AccountNotFoundError`), same → `ValidationError("Cannot swap an account with itself")`, refuse live sessions (`--swap-accounts`), read both; same-email → stage both to `.swap-staging-*` (leftover → the `ConfigError` in §1.4; staging OSError → `ConfigError("Could not stage swap material, nothing was changed: {e}")`); swap session dirs via `.swapping` rename; write-or-clear each destination; swap records; renumber + sort `sequence`; swap `activeAccountNumber` if either; write (commit). Failure → `_rollback_swap` (keep staged copies + warning if restore fails). Post-commit: delete old keys (if emails differ), drop `.prev` on written keys, discard staging, log `Swapped slots: {a} ({ea}) <-> {b} ({eb})`.
- What travels with the number: alias, creds/config backups, session profile, `sequence` membership, `activeAccountNumber`, `disabled`. Mappings key on identity → unaffected. Usage rows / quarantine key on slot but carry identity → self-heal on next poll / next tick.

### 2.6 Aliases and identifier resolution
- `normalize_alias`: `strip().lower()`; empty → `ValueError("alias cannot be empty")`; all digits → `"alias '{name}' cannot be purely numeric (reserved for slot numbers)"`; leading `-` → `"alias '{name}' cannot start with '-' (would be read as a command flag)"`; must match `^[a-z0-9_.-]+$` else `"alias '{name}' may only contain letters, digits, '-', '_', and '.'"`. Callers wrap as `ValidationError`.
- Resolution precedence **number → alias → email**: `isdigit()` → returned as-is; else case-insensitive alias match (empty alias never matches); else exact email: 0 → `None`, 1 → slot, ≥2 → `ConfigError("Email '{id}' is ambiguous — matches accounts: {num [Org|personal], …}. Use account number instead (e.g., cswap --switch-to 1).")`.
- `alias set ID ALIAS` (conflict → `ConfigError("Alias '{a}' is already used by account {n}")`), `alias unset ID` (idempotent, no write if absent), `alias list` sorted by `int(num)`.

### 2.7 `disable` / `enable`
`record["disabled"] = True` / `pop("disabled")`; no-op with `Account-{n} ({email}) is already {disabled|enabled}.`; prints `{Disabled|Enabled} Account-{n} ({email}).`; extra notes when disabling the active slot (`  It is the active account — it stays live until you switch away; it just won't be an automatic switch target.`) or emptying the rotation (`warning("  No accounts remain in rotation — auto-switch and bare switch have nothing to pick. Re-enable one with cswap enable <num|email>.")`); enabling prints `  It is back in the rotation.`. Disabled slots stay valid explicit `switch <id>` targets; only auto/rotation/strategies skip them; re-enabling restores the original sequence position.

### 2.8 Switchable / rotation eligibility
`_account_is_switchable(num)` = record exists **and** non-empty stored credential backup **and** non-empty stored config backup. `switchable_account_numbers()` = `sequence` order filtered by switchable **and not disabled** — this is the candidate universe for auto-switch, bare rotation and the `best`/`next-available` strategies. `account_kind_for(num)` = `"api_key"` iff `kind == "api_key"`, else `"oauth"`.

### 2.9 `purge`
Refuses while any session profile has live PIDs (`SessionError("Live session-mode Claude instance(s) found: {name (PID …); …}. Exit them first, then retry --purge.")`); prompts `Are you sure you want to purge all data? [y/N] `; removes every backup credential (both backends, incl. `account-None-*` and legacy keyring service), session Keychain entries **before** dirs, closes log handlers, `rmtree(backup_dir)` and a distinct legacy dir; prints `Removed:` list and `Purge complete.`. Never touches the live Claude login.

---

## 3. Locking

| Lock | Artifact | Mechanism | Timeout / cadence | Protects |
|---|---|---|---|---|
| cswap account lock | `<backup>/.lock` | `FileLock`: POSIX `fcntl.flock(LOCK_EX|LOCK_NB)`, Windows `msvcrt.locking(LK_NBLCK, 1)`; poll 0.1 s on a monotonic clock; opened with `"w"` (content meaningless) | default **10.0 s**; session bootstrap **30.0 s**; timeout → `acquire()` returns False, context manager raises `LockError("Failed to acquire lock - another instance may be running")` | roster + backup mutations: add/remove/swap/move, switch body, `persist_backup_credentials`, `backfill_account_uuid`, consume gate re-read/CAS |
| usage-store lock | `<backup>/cache/.usage.lock` | `FileLock` | 10 s | RMW of `usage.json` (reads are lock-free) |
| auto-switch state lock | `<backup>/.autoswitch_state.lock` | `FileLock` | 10 s | RMW of `autoswitch_state.json`; held across recheck→switch→record in `_perform` |
| per-slot consume lock [PY-NEWER] | `<backup>/credentials/.consume-{num}.lock` | `FileLock` | 10 s (non-blocking semantics: busy → `consume-busy`, no wait beyond the acquire loop) | serializes refresh-token POSTs for a slot across surfaces |
| Claude Code credentials lock | dir `<config_home>.lock` (default `~/.claude.lock`; `CLAUDE_CONFIG_DIR=/x/dir` → `/x/dir.lock`) | npm `proper-lockfile` protocol: `mkdir` = mutex; stale when mtime older than **10.0 s** (wall clock); holder touches mtime every **3.0 s** (daemon thread; Claude Code touches every 5 s); stale lock is `rmdir`'d and retaken; wait sleeps `0.25 + rand*0.25` s | **9.0 s** → `ClaudeCodeLockTimeout("Could not acquire {name} — Claude Code appears to be refreshing credentials. Retry in a few seconds.")` (nothing mutated, safe to retry) | Claude Code's own OAuth refresh (read→refresh→save under this lock) |
| Claude Code config lock | dir `<global_config>.lock` (default `~/.claude.json.lock`; session profile: `<profile>/.claude.json.lock`) | same protocol | 9.0 s | `~/.claude.json` writes (`oauthAccount` splice, `primaryApiKey`, MCP mirror) |

Rules:
- **Order** when combined: `with FileLock(lock_file), claude_credentials_lock(), claude_config_lock():` — always cswap lock first, then credentials, then config. The whole switch mutation (backup outgoing → write target creds → splice config → update roster, plus rollback) runs under this triple lock so a mid-refresh Claude Code either finishes first (backup captures the rotated token) or re-reads under the lock, sees a non-expired credential and aborts its own refresh.
- **Never hold any lock across network I/O.** `FileLock` is **non-reentrant**; callers like `persist_backup_credentials`/`consume_backup_grant` must not be invoked while `lock_file` is held.
- Usage store protocol: (a) lock → read → decide eligibility + stamp `lastAttemptAt`/`claimId`/`claimUntil` → unlock; (b) fetch unlocked (thread pool, start stagger `idx * 0.25 s`); (c) lock → re-read → merge outcomes fenced by `claimId` → write plan in the same transaction → unlock.
- After every backup-credential write (`_post_backup_write`): if the slot has a live session → `mark_session_stale` (sibling marker; failure logged at ERROR); else `_invalidate_session_credentials` (delete session Keychain entry, unlink profile `.credentials.json`, clear markers; `.claude.json` kept).
- Destructive slot ops call `_ensure_no_live_session(num, email, action)`: live PIDs → `SessionError("Account-{n} ({email}) has a live session-mode Claude instance (PID {pids}). Exit it first, then retry {action}.")`; unreadable PID records → `SessionError("Account-{n} ({email}) has {k} session record(s) that could not be read, so whether a Claude instance is live cannot be determined. Inspect {dir}/sessions and remove or repair them, then retry {action}.")`. `_perform_switch` only **warns** about a live session (message in §6.13).

---

## 4. Usage model

### 4.1 Raw → normalized usage (`oauth.build_usage_result`)

Raw `GET https://api.anthropic.com/api/oauth/usage` (headers `Authorization: Bearer <access>`, `anthropic-beta: oauth-2025-04-20`, `User-Agent: claude-swap/1.0`, timeout 5 s):

```jsonc
{
  "five_hour": {"utilization": 22.0, "resets_at": "2026-…Z" | null},
  "seven_day": {"utilization": 61.0, "resets_at": "…" | null},
  "seven_day_opus": null,                      // ignored
  "extra_usage": {"is_enabled": true, "used_credits": 72900, "monthly_limit": 500000, "utilization": 14.58, "currency": "USD"},
  "limits": [ {"kind": "weekly_scoped", "group": "weekly", "percent": 100, "resets_at": "…Z",
               "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}, "is_active": true}, … ]
}
```

Normalized (this is what `lastGood` stores and every decision reads):

```jsonc
{
  "five_hour": {"pct": 22.0, "resets_at": "…", "countdown": "1h 0m", "clock": "20:39"},   // countdown/clock only when resets_at present
  "seven_day": {"pct": 61.0, …},
  "spend":     {"used": 729.0, "limit": 5000.0, "pct": 14.58, "currency": "USD", "resets_at"?, "countdown"?, "clock"?},
  "scoped":    [{"name": "Fable", "pct": 100.0, "resets_at"?, "countdown"?, "clock"?}]
}
```

- `five_hour`/`seven_day` produced only when the raw key is truthy; `pct` = raw `utilization` (uncoerced int/float; store as f64). `null` `resets_at` → no `resets_at`/`countdown`/`clock` keys.
- `spend` only when `extra_usage.is_enabled` **and** `used_credits`, `monthly_limit`, `utilization` are all non-null; credits are **cents** (÷100); `currency` defaults `"USD"`.
- `scoped` only from `limits[]` entries with a truthy `scope.model.display_name` and numeric `percent` (coerced float). Entries with `scope: null` (session, weekly_all) are dropped. No `limits` key → no `scoped` key.
- Empty result → `None`.
- `format_reset(resets_at)` → `(countdown, clock)`: `days>0 → "{d}d {h}h"`, `hours>0 → "{h}h {m}m"`, else `"{m}m"`; clock = local `HH:MM` same day else `"%b {day} %H:%M"` (day unpadded). `fresh_reset_strings(window)` recomputes from `resets_at` at render time, falling back to cached strings.

### 4.2 Windows, binding window, headroom, at-limit

- `relevant_windows(usage, models)` → `[(label, pct, resets_at), …]`: always `("5h", five_hour.pct, resets_at)` and `("7d", seven_day.pct, resets_at)` when the window dict has a numeric `pct`; when `models` is non-empty, each `scoped` entry whose `name.lower()` is in `{m.lower() for m in models}` (label = original-cased display name), and the sentinel `"all"` (any case) matches every scoped window. **`spend` is never a window.** Non-dict usage → `[]`.
- `account_headroom(usage, models)` = `100.0 - max(pct)` over relevant windows, or **`None`** when there are none (unknown — callers must never auto-skip on unknown).
- `binding_pct` = `100 - headroom` (None if unknown).
- **At limit** ⇔ `headroom is not None and headroom <= 0` (some relevant window `pct >= 100`). Three-way distinction `None` / `<= 0` / `> 0` is load-bearing everywhere.
- `limiting_reset_ts(usage, models)` = **latest** parseable `resets_at` among windows with `pct >= 100` (when the account is usable again). `earliest_future_reset_ts(usage, now, models)` = **earliest** parseable `resets_at > now` over all relevant windows. `parse_reset_ts` = `fromisoformat(s.replace("Z","+00:00")).timestamp()`, None on failure.
- [GO-ONLY] `atLimit`/`limitingWindows` JSON fields (DESIGN A15) and `RenewalTS` (A17, latest weekly reset) — not in Python.

### 4.3 Read model: `UsageEntry` and decision trust

`UsageEntry` fields: `sentinel`, `last_good`, `fetched_at`, `age_s`, `last_attempt_at`, `consecutive_failures`, `last_error`, `backoff_until`, `next_poll_at`, `poll_interval_s`, `last_429_at`, `auth_dead_strikes`, `struck_fingerprint`, `trust_extended`, `claim_until`.

Constants (`usage_store.py`, Python current):

| Name | Value | Meaning |
|---|---|---|
| `SCHEMA_VERSION` | 2 | |
| `STALE_OK_S` | 300.0 | last-good trusted for decisions up to this age unconditionally |
| `CLAIM_TTL_S` [PY-NEWER] | 90.0 | fetch lease length (`claimUntil = now + 90`) |
| `LEGACY_CLAIM_TTL_S` | 10.0 | rows without `claimUntil`: live claim while `now - lastAttemptAt < 10` |
| `TRUST_MAX_AGE_S` | 3600.0 | hard ceiling on trust extension (non-429 failures) |
| `RATE_LIMIT_TRUST_MAX_AGE_S` [PY-NEWER] | 7200.0 | ceiling on trust when `lastError == "http-429"` |
| `BACKOFF_BASE_S` / `BACKOFF_CAP_S` | 30.0 / 600.0 | failure backoff `30·2^(n-1)` capped (exponent clamped at `BACKOFF_MAX_SHIFT = 32`) |
| `RETRY_AFTER_MARGIN_S` [PY-NEWER] | 900.0 | added to a 429 Retry-After ask above the cap |
| `RETRY_AFTER_FLOOR_CAP_S` [PY-NEWER] | 4500.0 | park bound for 429 asks (non-429 asks are bounded by `TRUST_MAX_AGE_S`) |
| `AUTH_DEAD_STRIKES` | 1 | strikes → quarantine |
| `PERMANENT_AUTH_ERRORS` [PY-NEWER] | `{"invalid_grant", "no_refresh_token"}` | only these advance the strike |

Derived methods:
- `fresh(now, ttl=SERVE_TTL_S=180)` = `fetched_at is not None and now - fetched_at <= ttl`.
- `in_backoff(now)` = `backoff_until is not None and now < backoff_until`.
- `claimed(now)` = `now < claim_until` if `claim_until` set, else legacy `now - last_attempt_at < 10`.
- `recent_429(now)` = `last_429_at` set and `now < anchor + RECENT_429_WINDOW_S(3600)`, where `anchor = backoff_until` if `last_error == "http-429" and backoff_until > last_429_at`, else `last_429_at` (recency measured from when the honored backoff **lifts**).
- `token_dead(threshold=1, stored_fp=None)` = `auth_dead_strikes >= threshold` **and not** (`stored_fp` given and `struck_fingerprint` recorded and they differ). A row struck before fingerprints existed binds unconditionally. The collector additionally checks the active slot's **backup** as a second stored source and holds the strike if that backup is unreadable.
- `trust_extended` (computed in `entries()`): `within_ceiling and (consecutive_failures > 0 or (next_poll_at and now < next_poll_at) or live_claim)` where
  - `within_ceiling` = for `lastError == "http-429"`: `now < min(earliest future relevant-window reset (models-aware), now + (7200 - age_s))` (usage is monotone inside a window, so a throttled last-good is a valid lower bound until the soonest window rolls over); otherwise `age_s <= 3600`.
- **`decision_value()`** = `sentinel` if set; else `last_good` if `age_s <= 300 or trust_extended`; else **`None`** ("too old to drive a decision"). Display code reads `last_good`/`age_s` directly regardless (human list shows numbers with `· Xm ago` once `age_s > 180`; JSON carries decision-grade `usage` and, when null, `lastGoodUsage`/`lastGoodFetchedAt`/`lastGoodAgeSeconds`).

Sentinels (derived every pass, never persisted; `json_output.py`): `"no credentials"`, `"token expired"` (`USAGE_TOKEN_EXPIRED`), `"api key"`, `"keychain unavailable"`, `"re-login needed"` (dead token), `"foreign credential"` [PY-NEWER]. JSON `usageStatus`: `ok | token_expired | api_key | keychain_unavailable | relogin_required | foreign_credential | no_credentials | unavailable`. Static derivation (`_static_usage_sentinel`): API-key creds → `api key`; no creds / no access token → `keychain unavailable` (active slot with a failed Keychain read, or an inactive slot whose own backup read was unreadable) else `no credentials`; dead token → `re-login needed`; an expired active token that cannot reach the fetch path this pass (backoff/claim/plan gate) → `token expired`.

### 4.4 Failure backoff (`_failure_backoff_s(n, retry_after, rate_limited)`)

```
computed = min(30 * 2**min(max(0, n-1), 32), 600)           # 30,60,120,240,480,600,600,…
if retry_after is None:            return computed
if retry_after == 0:
    if not rate_limited:           return computed           # 503 "retry now" is not the 429 edge
    return min(max(computed, EDGE_BACKOFF_S=300), 600)       # saturated-budget edge
asked = retry_after
if retry_after > 600 and rate_limited: asked += 900          # RETRY_AFTER_MARGIN_S (429 only)
asked = min(asked, 4500 if rate_limited else 3600)           # park bound per arm
return max(asked, computed)
```
`rate_limited = (error == "http-429")`. Examples: `(1, 90)` → 90; `(5, 10)` → 480; `(1, 300)` → 300; `(1, 3600)` 429 → 4500; `(1, 3600)` non-429 → 3600; `(1, 86400)` 429 → 4500; `(50, None)` → 600.

### 4.5 `record()` merge rules
- Sentinel record: clears `claimId`/`claimUntil` only.
- Success: `lastGood = usage`, `fetchedAt = now`, plan `(nextPollAt, pollIntervalS)` written in the **same** transaction when supplied, `consecutiveFailures = 0`, `lastError = null`, `backoffUntil = null`, `authDeadStrikes = 0`.
- Failure: `consecutiveFailures += 1`, `lastError = error`, `http-429` → `last429At = now`, `backoffUntil = now + backoff`, permanent-auth → `authDeadStrikes += 1` and `struckFingerprint = struck_fp` (always overwritten). `lastGood`/`fetchedAt` untouched.
- Fencing: with `claims`, a row is applied only if identity matches and `row.claimId == claims[num]`; an unfenced writer defers to a **live** lease only (expired leftover tickets age out).

### 4.6 Eligibility (`reserve` / `_row_eligible`)
Ineligible if: `authDeadStrikes >= 1`; `now < backoffUntil`; live claim. Then `stale = fetchedAt is None or now - fetchedAt > 180`, `poll_due = nextPollAt is not None and now >= nextPollAt`, `overslept = repair_overslept and plan_oversleeps_interval` (a `nextPollAt` later than `now + max(pollIntervalS, 600)*1.1 + 60` — an obsolete reset-parked plan):
- on-demand (`respect_plans=True`, list/status/switch/dashboards): `stale and (poll_due or nextPollAt is None or overslept)`.
- auto engine scheduled (`respect_plans=False, repair_overslept=True`): `poll_due or (stale and (nextPollAt is None or overslept))`.
- auto engine escalation (`respect_plans=False, repair_overslept=False`): `poll_due or stale`.
A row with mismatched identity is replaced with a fresh row and won immediately.

`due_candidate(candidates, entries, now)`: skip sentinel / dead / in-backoff / not-yet-due (unless overslept); rank `(0, 0.0, num)` for missing or never-fetched, `(1, fetched_at, num)` otherwise; sort ascending, return first (stalest first, ties by slot string).

### 4.7 Usage fetch orchestration (`try_fetch_usage_for_account`)
- No access token → `no-access-token`.
- Inactive account with refresh token and `is_oauth_token_expired(expiresAt)` (expired = `now_ms + 300000 >= expiresAt`; unknown expiry = not expired): refresh first via `refresh_via` (the consume gate) or a direct POST. Success → adopt. `invalid_grant`/`no_refresh_token` → return that error **without** hitting the usage endpoint, `struck_fp = consumed_fp or fingerprint(snapshot)`. Deterministic kinds (`store-unmirrored`, `invalid_client`, `consume-busy`, `stash-unreadable`) → returned as-is. `transient` → fall through with the expired token.
- Usage 401 on an inactive account with a refresh token → one refresh + retry; refresh failure → the dead/deterministic kind or `refresh-failed`.
- **Active account**: never proactively refreshed here (Claude Code owns it); a 401 is not retried. [PY-NEWER] The switcher's `_fetch_active_usage` refreshes an expired active token under Claude Code's own lock protocol, with provenance checks; failures surface as `token expired`.
- Inactive account with a **session profile**: the profile's credential (Keychain hashed item on macOS, else profile `.credentials.json`) supersedes the backup; fetched read-only (no refresh); expired + live session → `token expired`; drifted profile identity → fall back to backup.
- Error classification: `HTTPError` → `http-{code}` + Retry-After seconds (float, negative→0, HTTP-date→None); timeouts → `timeout`; other `URLError` → `network`; JSON decode → `bad-response`; else exception type name. WARNING log `Usage fetch failed for account N: {kind}[, retry-after Ks][ (per-token usage budget reached; backing off)]` — never with the email.

### 4.8 Refresh-grant consume gate (`consume_backup_grant`) [PY-NEWER]
Every backup refresh-token POST (auto-switch freshen, inactive-account usage refresh, session bootstrap) goes through one gate: refuse when `CLAUDE_SECURESTORAGE_CONFIG_DIR` is set (`store-unmirrored`); acquire `.consume-{num}.lock` (busy → `consume-busy`); under `FileLock(lock_file)` re-read the freshest backup (unreadable Keychain → `transient`), adopt a stashed successor, consult the session profile for a newer generation; POST outside the lock; then compare-and-swap on the refresh-token fingerprint and persist (or stash the successor if the backup moved). Outcome carries `credentials`, `error`, `token_account`, `consumed_fp`, `stashed`. A demoted outcome (`transient` **with** credentials) means the grant was spent but not persisted.

### 4.9 Credential fingerprint
`credential_fingerprint(creds)`: empty → `None`; non-empty string `claudeAiOauth.refreshToken` → `"sha256:" + hex(sha256(refreshToken))` (rotation-stable); else `"sha256-full:" + hex(sha256(creds))`.

### 4.10 Pace (weekly windows only, `pace.py`)

Constants: `WEEKLY_PERIOD_S = 604800`, `SUPPRESS_AFTER_RESET_S = 86400` (24 h), `AHEAD_THRESHOLD_PCT = 15.0`.

`compute_pace(window, fetched_at)` → `PaceResult{expected_pct, actual_pct, elapsed_s, period_s, ahead}` or `None`:
```
if window not dict or fetched_at is None: None
pct = window["pct"]; must be numeric else None
next_reset = fromisoformat(window["resets_at"]).timestamp()   # raw string, no Z substitution; None → None
remaining = (next_reset - fetched_at) mod period               # Python float mod: result in [0, period)
elapsed   = 0 if remaining == 0 else period - remaining
if elapsed < 86400: None                                       # suppress right after a reset
expected_pct = min(100, elapsed / period * 100)
ahead = (pct - expected_pct) >= 15
```
Elapsed is measured against `fetched_at`, never wall-clock `now`, so stale-served data is judged at its measurement time. Applies to `seven_day` and every `scoped` window; never `five_hour`.

`projected_exhaustion_ts(pace, fetched_at)`: `None` if `elapsed_s <= 0 or actual_pct <= 0`; `rate = actual/elapsed` (pct/s); `remaining = 100 - actual`; `<= 0` → `fetched_at`; else `fetched_at + remaining / rate`.

`will_last_to_reset(pace)`: `actual_pct <= 0` → `True`; `elapsed_s <= 0` → `None`; `rate = actual/elapsed`; `projected_total = actual + rate * (period - elapsed)`; return `projected_total <= 100`. (Equivalent to "not over expected at all" — stricter than the 15-pt marker.)

JSON projection (`json_output._pace_fields`, only on `sevenDay` and each `scoped[]` entry, only when `fetched_at` known and pace computable): `expectedPct` = `round(expected_pct, 1)`, `aheadOfPace` = bool, `projectedExhaustionAt` = ISO seconds `Z` when not None, `willLastToReset` when not None. Human list: append `  (ahead of pace)` to the 7d row and to scoped rows (scoped rows at/over 100 show `  (!)` instead).

### 4.11 JSON usage row (list/status)
`usage_to_json`: `fiveHour {pct, resetsAt?, countdown?, clock?}`, `sevenDay {…, + pace}`, `spend {used, limit, pct, currency, resetsAt?, countdown?, clock?}`, `scoped [{name, pct, resetsAt?, countdown?, clock?, + pace}]`. Row: `number, email, organizationName, organizationUuid, isOrganization, active, usageStatus, usage` + optional `alias`, `disabled: true`, `usageFetchedAt` (ISO Z) / `usageAgeSeconds` (round 1) alongside non-null usage, or `lastGoodUsage`/`lastGoodFetchedAt`/`lastGoodAgeSeconds` when usage is null but a measurement exists.

---

## 5. Poll policy (`poll_policy.py`)

Measured endpoint budget: ~28–30 requests per rolling ~60 min per identity × UA class (the identity is the account/org under the fixed-deadline regime and the access token under the `Retry-After: 0` regime); not a refilling bucket. Target ≤ ~1 request / 3 min per token.

| Constant | Value |
|---|---|
| `SERVE_TTL_S` | 180.0 — younger entries are served without any fetch (sustained-rate governor) |
| `MIN_INTERVAL_S` | 180.0 |
| `URGENT_INTERVAL_S` | 60.0 |
| `ACTIVE_MAX_INTERVAL_S` | 300.0 |
| `CANDIDATE_DEFAULT_INTERVAL_S` | 300.0 |
| `CANDIDATE_MAX_INTERVAL_S` | 600.0 |
| `EXHAUSTED_INTERVAL_S` [PY-NEWER] | 600.0 |
| `MOVEMENT_DELTA_PCT` | 1.0 |
| `JITTER_FRAC` | 0.1 |
| `EDGE_BACKOFF_S` | 300.0 |
| `POST_429_MIN_INTERVAL_S` | 360.0 |
| `RECENT_429_WINDOW_S` | 3600.0 |
| `POST_429_BACKOFF_MULT` [PY-NEWER] | 1.5 |
| `POST_429_MAX_INTERVAL_S` [PY-NEWER] | 1800.0 |
| `ESCALATION_MARGIN_PCT` | 15.0 |
| `RESET_SLACK_S` | 60.0 |

`plan_after_fetch(prev_interval, prev_usage, new_usage, is_active, threshold, models, recent_429, now, rng)` → `(next_poll_at, interval)`:
1. `default = 180 (active) | 300 (candidate)`; `ceiling = 300 | 600`; `base = prev_interval or default`.
2. `prev_pct`, `new_pct` = binding pcts. Either unknown → `interval = default`, not moving. `|Δ| >= 1.0` → moving, `interval = max(180, base/2)`. Else `interval = min(ceiling, max(180, base*1.5))` (an urgent 60 s base snaps back to 180).
3. Urgent: `is_active and moving and not recent_429 and new_pct >= threshold - 15` → `interval = 60`.
4. `recent_429` (AIMD): `increased = max(base*1.5, 360)`; `interval = min(1800, max(interval, increased))`.
5. At limit (`headroom <= 0`): `interval = max(interval, 600)`.
6. `next_poll = now + interval * (1 + 0.1*(2*rng()-1))`.
7. At limit: `next_poll = min(next_poll, limiting_reset_ts + 60)` if that reset is in the future. Otherwise `next_poll = min(next_poll, earliest_future_reset + 60)` when known.
Worked values (jitter 0): first fetch active→180, candidate→300; unmoved 300→450, 500→600 (cap), active 250→300; moved 600→300, 200→180; sub-1.0 wiggle is not movement; urgent active 78→82 with threshold 90 → 60; urgent suppressed by recent 429 → 360.

Plans are persisted by whichever collector fetched (`_plans_after_fetch` → `record(..., plans)`), using `_poll_policy_inputs()` = the hosted engine's pinned `(threshold, models)` (`set_poll_policy_inputs`) else `settings.json` (`autoswitch.threshold`, `parse_model_names(autoswitch.model)`, reloaded when the file's mtime changes).

Post-switch replan (`_replan_new_active`): pull the new active account's plan to `next_poll = max(now, fetched_at + 180)`, interval 180, only ever earlier; never-measured accounts left plan-less; best-effort.

### 5.1 List / status / TUI refresh rules
- `cswap list`/`status`, `switch` strategies, and every dashboard take an **on-demand pass** (`fetch=None`): every slot is a candidate but the store's `respect_plans=True` gate applies (stale > 180 s **and** poll-due/no-plan/overslept). No surface has a "force fetch now"; the TUI's `f` and menubar "Refresh now" just run another on-demand pass. Failed fetches keep serving `lastGood` (stale-on-error) with an age note in human output once older than 180 s.
- `list` skips proactive token refresh for any slot with a live session PID.
- TUI dashboard polls the snapshot source every `POLL_INTERVAL_S = 3.0` s (single-flight worker); rows flash `FLASH_S = 1.5` s when `fetchedAt` changed; a "stale" tag appears when `age_s > STALE_OK_S (300)`. While the Auto screen hosts an engine, dashboards go **store-only** (`fetch=set()`) — the engine is the sole fetcher.
- Menubar: refresh timer 30/60/300 s (default 60, `refresh_interval` in its own settings); store-only while an engine runs; re-reads the active account when `~/.claude.json` mtime changes (1 s tick).
- `SnapshotSource._reconcile`: if a new snapshot's `fetched_at` regressed (or vanished) for the same identity, keep the previous entry with a recomputed age; a `token expired` sentinel is carried forward while `fetched_at` is unchanged.
- Human list row format: `  {num}: {alias (email)|email} [{tag}] (active) (disabled)` then indented usage lines `├ 5h:  22%   resets 20:39         in 1h 0m` … `└ 7d:  61% … (ahead of pace) · 12m ago`, spend as `$$:`, scoped as `{name}:` with `  (!)` at ≥100; sentinel rows show the `SENTINEL_NOTES` text plus `└ last seen 53% used · 12m ago`; no measurement → `usage unavailable ({ERROR_NOTES[last_error] or last_error})`.

---

## 6. Auto-switch engine (`autoswitch.py`, Python current) — full algorithm

### 6.1 Constants

| Name | Value | Meaning |
|---|---|---|
| `STATE_FILENAME` | `autoswitch_state.json` | |
| `STATE_SCHEMA_VERSION` | 1 | |
| `FRESHEN_BUFFER_MS` | 600000 (10 min) | freshen a target whose token expires within this; 2× Claude Code's 5-min buffer |
| `MAX_SLEEP_S` [PY-NEWER] | `= EXHAUSTED_INTERVAL_S = 600.0` (Go docs: 21600) | cap on a blocked sleep toward a known reset |
| `NO_RESET_FALLBACK_S` | 300.0 | blocked/idle-hold cadence when no reset is known |
| `IDLE_HOLD_MAX_S` | 1800.0 | max elapsed idle-hold before unhealthy counting resumes |
| `RECOVERY_HYSTERESIS_S` [PY-NEWER] | 300.0 | anti-flap margin on the reset axis |
| `RECOVERY_HORIZON_S` [PY-NEWER] | 14400.0 (4 h) | a reset farther than this stops being worth headroom |
| `HORIZON_HEADROOM_RATIO` [PY-NEWER] | 2.0 | ratio margin on the headroom axis in the all-above-threshold state |
| `SPENT_HEADROOM_PCT` [PY-NEWER] | 3.0 | below this an account is "spent"; headroom comparisons are noise |
| `ESCALATION_MARGIN_PCT` (poll_policy) | 15.0 | escalate to a full candidate refresh within this of the threshold |
| `RESET_SLACK_S` (poll_policy) | 60.0 | |
| `USAGE_TOKEN_EXPIRED` | `"token expired"` | idle-hold sentinel |
| `_SYSTEMIC_STATUSES` [PY-NEWER] | `("store-unmirrored", "invalid_client", "stash-unreadable", "consume-busy")` — insertion order = reporting precedence | deterministic freshen refusals |

Settings consumed: `threshold`, `interval_seconds`, `cooldown_seconds`, `hysteresis_pct`, `strategy` (`best` | `consume-first`), `include_api_key_accounts`, `unhealthy_ticks`, `model` (parsed once at construction into `_models`; passed to every window read). Construction also pins `switcher.set_poll_policy_inputs(threshold, models)`. `apply_threshold(t)` (TUI session override) atomically replaces `settings.threshold` and re-pins; model axes are fixed. `clock` = wall time (persisted cooldown must survive processes); tests inject a fake clock.

### 6.2 Events (`to_json()` = `{"schemaVersion": 1, "event": kind, "ts": ISO-seconds-Z, …}`; JSONL = one compact object per line, flushed; additive — consumers ignore unknown kinds/fields)

| kind | extra fields | human line |
|---|---|---|
| `poll` | `active` (`{number:int, email}` or null), `headroomPct` (`{num: float|null}`), `threshold`, `fetchErrors` (`{num: lastError}` only for accounts whose decision value is null and have a `last_error`; omitted when empty), `windowsPct` (`{num: {"5h": pct, "7d": pct, "<Model>": pct}}` in relevant-window order, scoped names only when a model is configured; omitted when empty) | `poll: no active account` / `Account-{n} ({email}): {used}% used|usage unknown[ ({err})] (switch at {pct_label(threshold)}%)[ | others: #{n}: {5h 3% · 7d 89%|62%|? (err)|?}, …]` |
| `switch` | `trigger` (`proactive`|`at-limit`|`failover`|`consume-first`), `from` (ref or null), `to` (ref), `warnings` (list), `dryRun` (bool) | `Switched|[dry-run] would switch Account-{a}|(none) -> Account-{b} ({email})|? ({trigger})` |
| `no-switch` | `reason`, `detail` (`""` when none) | `no switch: {reason}[ ({detail})]` |
| `account-quarantined` | `number` (str), `email`, `reason` (`invalid_grant`|`identity-conflict`) | `Account-{n} ({email}) quarantined: {reason}. Log in with it and run 'cswap --add-account --slot {n}' to recover.` |
| `account-unquarantined` | `number`, `email`, `reason` (`account-replaced`|`credentials-replaced`) | `Account-{n} ({email}) back in rotation ({reason})` |
| `all-exhausted` | `earliestResetAt` (ISO Z or null) | `all accounts exhausted; earliest reset {ts}` / `…; no reset time known` |
| `sleep` | `seconds` (rounded 1 dp), `until` (ISO Z) | `sleeping {m}m (until {until})` |
| `error` | `message`, `transient` (bool) | `error: {message}[ (will retry)]` |
| `config-warning` | `message` | `warning: {message}` |

`no-switch` reasons (exhaustive): `unmanaged-active-account`, `no-active-account`, `active-api-key`, `below-threshold`, `active-idle`, `active-usage-unknown`, `cooldown`, `no-candidates`, `no-comparison`, `no-qualifying-candidate`, `no-viable-target`, `already-active`, and [PY-NEWER] `reset-unknown`, `already-consuming-soonest`, `stale-usage`.

`pct_label(v) = f"{v:.10g}"` (90.0→`90`, 99.9→`99.9`, 62.60000000000001→`62.6`); used on **both** sides of `below-threshold` detail and the poll header.

### 6.3 `TickOutcome` → `--once` exit codes
`SWITCHED = 0` (a switch happened, or would in dry-run), `ERROR = 1` (network trouble, lock contention, transient/systemic freshen failure, any `ClaudeSwitchError`), `NO_ACTION = 2` (below threshold, cooldown, idle-hold, api-key active, consume-first holds, already-active), `BLOCKED = 3` (wanted to switch but no candidates / no comparison / no qualifying candidate / all exhausted / no viable target). `tick()` never raises: `ClaudeSwitchError` → `ErrorEvent(str(e), transient=True)`; any other exception → `ErrorEvent("{TypeName}: {e}")`.

### 6.4 `_tick_inner` step by step

1. Reset per-tick: `_sleep_until_ts = None`, `_blocked_wait_long = False`, `_idle_hold_slow = False`; snapshot `settings`.
2. `state = _read_state()`; if not dry-run, `state = _release_recovered_quarantines(state)` (§6.10). `quarantined = keys of state["quarantine"]`.
3. `current = switcher.current_account_number()` (live `~/.claude.json` identity → slot; **no** fallback to `activeAccountNumber`). If `None`: emit `poll(active=None, headroom={})`; then `has_live_login()` → `no-switch unmanaged-active-account` (`run 'cswap --add-account' to include it in rotation`) else `no-active-account` (`log in and run 'cswap --add-account' first`); return NO_ACTION.
4. `active_ref = {number: int(current), email: account_email(current) or ""}`.
5. `(entries, usage, headroom) = _collect_scheduled_usage(current, quarantined, threshold=settings.threshold)` (§6.5).
6. Emit `poll`.
7. If `not _model_check_done`: `_check_model_names` (§6.12).
8. Active is api-key and `not include_api_key_accounts` → `no-switch active-api-key` (`API-key accounts have no quota to watch`), NO_ACTION.
9. `active_headroom = headroom[current]`:
   - **known**: `_unhealthy_ticks = 0`, `_idle_hold_since = None`, `utilization = 100 - h`.
     - `utilization < threshold`: if `strategy != "consume-first"` → `no-switch below-threshold` (`{pct_label(util)}% < {pct_label(threshold)}%`), NO_ACTION. Else `trigger = "consume-first"`.
     - else `trigger = "at-limit"` if `h <= 0` else `"proactive"`.
   - **unknown** (`None`): if `usage[current] == "token expired"` (owned-and-expired sentinel): start/continue idle-hold (`_idle_hold_since`); if `now - since <= 1800`: `_unhealthy_ticks = 0`, `_idle_hold_slow = True`, `no-switch active-idle` (`token expired while Claude Code is idle; resumes on next use`), NO_ACTION; past the cap → WARNING log and fall through. Otherwise (`None` = genuine failure) `_idle_hold_since = None`. Then `_unhealthy_ticks += 1`; if `< unhealthy_ticks` → `no-switch active-usage-unknown` (`{k}/{N} before failover`), NO_ACTION; else `trigger = "failover"`.
10. Cooldown gate: `trigger in (proactive, consume-first) and _in_cooldown(state)` → `no-switch cooldown`, NO_ACTION. (`at-limit` and `failover` bypass cooldown.)
11. Candidates: `candidates = [n for n in switchable_account_numbers() if n != current and n not in quarantined]`; `oauth_candidates` = non-api-key; `api_key_candidates` = api-key ones **only if** `include_api_key_accounts` else `[]`.
    - `consume-first` with no oauth candidates and known active headroom → `no-switch below-threshold` (same detail), NO_ACTION (exit-code parity with `best`).
    - no oauth and no api-key candidates → `_blocked_wait_long = True`, `no-switch no-candidates`, BLOCKED.
12. Rank (§6.7): `(ordered, any_known, active_reset_ts) = _rank(...)` on the collected snapshot at `decided_now`.
13. **consume-first two-phase commit** [PY-NEWER]: if `trigger == "consume-first" and ordered`: refetch `{current, *candidates}` via `usage_entries_by_account(fetch=…)` (respect_plans=False; serves just-fetched rows from the store), recompute `usage`/`headroom`/`active_headroom`, and re-rank. The trigger is **not** re-classified even if the fresh active crossed the threshold.
14. `if not ordered and api_key_candidates and trigger != "consume-first": ordered = api_key_candidates` (last resort; never for a below-threshold nudge).
15. If `ordered` is empty:
    - `not any_known` → `no-switch no-comparison` (`no candidate has readable usage`), BLOCKED.
    - `trigger == "consume-first"`: `active_reset_ts is None` → `no-switch reset-unknown` (`active account's weekly reset time is unknown; consume-first is idle until it is reported`), NO_ACTION; else `no-switch already-consuming-soonest` (`no sooner-resetting account with room to spare`), NO_ACTION.
    - `truly_exhausted = all(h is not None and h <= 0 for oauth candidates)`: not truly exhausted → `no-switch no-qualifying-candidate` (`no candidate is below the threshold and better than the active account by the hysteresis margin, or usage is unreadable this tick`), BLOCKED at **normal cadence**. Truly exhausted → `_blocked_wait_long = True`; `earliest = _earliest_recovery(usage)` (§6.9); if not None `_sleep_until_ts = earliest + 60`; emit `all-exhausted(earliestResetAt)`; BLOCKED.
16. Freshen + switch loop (§6.8) with `left_snapshot = (active_headroom, _binding_recovery_ts(usage[current], models, decided_now))`.

### 6.5 Adaptive collection (`_collect_scheduled_usage`)
- `candidates` as above (quarantined never consume a poll slot).
- Phase A: `pre = usage_entries_by_account(fetch=set())` (store-only). Nominate the **active** slot when: never fetched (`age_s is None`), or `stale_candidate_plan` (`age_s >= 300 and poll_interval_s > 300 and binding pct < 100` — a candidate-style plan left by a role change), or `overslept_plan`, or poll-due (`now >= next_poll_at`), or no plan and `age_s >= 180`. If **not** in an idle-hold, add exactly one `due_candidate(candidates, pre, now)`. Fetch with `scheduled = not stale_candidate_plan` (preserves valid future plans; the stale-candidate override may beat them). `usage = {n: entry.decision_value()}`.
- Phase B (escalate) when there is at least one candidate and: active headroom unknown **and** `active_value != "token expired"` (idle-hold never escalates), **or** `100 - active_headroom >= threshold - 15`. Escalation fetch = `{current, *candidates}` minus any slot that is decision-trusted **exhausted** (`headroom <= 0`) with a still-valid plan whose interval `> 600` (preserve a wider post-429 plan). Candidate selection never runs on the pre-escalation snapshot for at-limit/proactive/failover; consume-first decides provisionally and re-verifies at commit (step 13).
- Backoff is enforced by the collector for the active account too (a Retry-After is never defeated). `headroom = {n: account_headroom(usage[n], models)}`.

### 6.6 Threshold / binding window / model folding
Headroom = `100 - max(pct over relevant windows)` with `models` from `autoswitch.model` (`"Fable"`, `"Opus,Sonnet"`, `"all"`). Switch when `utilization = 100 - active_headroom >= threshold`. An account with 5h=5% but Fable=100% under `model=Fable` has headroom 0 (at-limit).

### 6.7 Candidate ranking (`_rank_candidates`, pure; wrapped by `_rank` with the no-return bar)

Inputs: `trigger`, `consume_first`, `oauth_candidates` (sequence order), `no_return`, `usage`, `headroom`, `current`, `active_headroom`, `settings`, `now`. Returns `(ordered, any_known, active_reset_ts)`.

```
active_reset_ts = seven_day_reset_ts(usage[current], now) if consume_first else None      # future 7d reset or None
all_above = every_account_above_threshold(oauth_candidates, headroom, active_headroom, threshold)
           # active known and >= threshold, and every MEASURED candidate >= threshold (needs ≥1 measured)
best_candidate_headroom = max(known candidate headrooms, default 0.0)
active_recovery_ts = binding_recovery_ts(usage[current], models, now) if all_above else 0.0
for num in oauth_candidates (sequence order):
    h = headroom[num];  None → skip (unreadable);  any_known = True
    h <= 0 → skip (at its limit)
    num == no_return → skip (the account we just left; §6.11)
    reset_ts    = seven_day_reset_ts(usage[num], now) if consume_first else None
    recovery_ts = binding_recovery_ts(usage[num], models, now) if all_above else 0.0
    if trigger in (proactive, consume-first):
        if (100 - h) >= threshold and not all_above → skip               # landing must be healthy
        if all_above:                                                     # "everyone over the line" escape
            by_recovery = recovery_is_useful(recovery_ts, active_recovery_ts, active_headroom or 0, best_candidate_headroom, now)
            if by_recovery:
                if recovery_ts >= active_recovery_ts - 300 → skip         # must come back ≥5 min sooner
            else:
                if h < (active_headroom or 0) * 2.0:
                    if (active_headroom or 0) <= 3.0 and h >= (active_headroom or 0) and recovery_ts < active_recovery_ts - 300:
                        fallback.append(((0, recovery_ts, -h), num))
                    skip
        elif consume_first:
            if trigger == consume-first and (reset_ts is None or active_reset_ts is None or reset_ts >= active_reset_ts) → skip
        elif active_headroom is not None:                                 # strategy best, proactive
            if h - active_headroom < hysteresis_pct → skip
    key = (0, recovery_ts, -h) if (all_above and by_recovery) else (1, -h, recovery_ts)   when all_above and trigger in (proactive, consume-first)
        = (reset_ts or inf, -h)                                            when consume_first (below threshold)
        = (-h,)                                                            otherwise (best)
    qualifying.append((key, num))
qualifying = qualifying or fallback; stable sort ascending by key (sequence order breaks ties)
```
- `binding_recovery_ts`: reset of the **binding** (max-pct) relevant window, `inf` when unknown/past.
- `seven_day_reset_ts`: `seven_day.resets_at` only if strictly in the future, else None.
- `recovery_is_useful`: True when both `active_headroom <= 3` and `best_candidate_headroom <= 3` (everything spent → rank by reset), else True when either the candidate's or the active's recovery is within 4 h of `now`.
- `at-limit` and `failover` apply **no** landing/hysteresis gate (only `h > 0`), so the `best` ordering is `(-h,)`: most headroom first, ties in sequence order.
- [GO-ONLY] `soonest-reset` (DESIGN A17): ordering-only, two tiers (below threshold by latest weekly `resets_at` ascending, unknown last, then headroom; at/over threshold by headroom) — no Python counterpart. The CLI `switch --strategy` vocabulary (`best`, `next-available`) is separate from the persisted `autoswitch.strategy`.

### 6.8 Freshen + perform
For each `num` in `ordered` (`email = account_email(num)`):
- `consume-first`: the candidate's entry must be `fresh(now)` (≤180 s) after the phase-2 refetch, else `no-switch stale-usage` (`account {n} usage could not be refreshed this tick (backoff or a concurrent poller); retrying`), NO_ACTION — never act on stale data or slide to a worse target.
- dry-run → `_perform` immediately (no freshen, no quarantine, no state writes).
- `status = _freshen_target(num, email)`:
  1. api-key → `ok`.
  2. live `cswap run` session on the slot → `skip-live-session`.
  3. no stored creds → `transient`; not OAuth JSON → `invalid_grant`.
  4. `near_expiry = now_ms + 600000 >= expiresAt` (numeric); not near → `ok` (no refresh).
  5. `outcome = switcher.consume_backup_grant(num, email, creds)` (§4.8; the successor is persisted by the gate **before** any identity check). Success → `_note_token_identity` (org compared first: both non-empty and different → conflict; empty slot uuid → `backfill_account_uuid` and `ok`; else conflict iff uuids differ) → `identity-conflict` | `ok`. `invalid_grant`/`no_refresh_token` → `invalid_grant`. A systemic status → returned as-is. Else `transient`.
- Dispatch: `identity-conflict` → `_quarantine(num, email, "identity-conflict")`, continue; `invalid_grant` → `_quarantine(num, email, "invalid_grant")`, continue; `transient` → flag, continue; systemic → remember the highest-precedence one, continue; `skip-live-session` → continue; `ok` → `_perform`.
- After the loop: systemic or transient → `error("could not freshen: " + _SYSTEMIC_MESSAGES[systemic])` / `error("could not freshen any candidate (network?)")`, ERROR; else `no-switch no-viable-target`, BLOCKED.

`_perform(number, email, trigger, left)`:
- dry-run: emit `switch(dryRun=true, from=current ref or null, to=ref)`, SWITCHED, nothing written.
- real: under the **state lock**: re-read state; proactive/consume-first still in cooldown → `no-switch cooldown`, NO_ACTION; `result = switcher.switch_to(number, json_output=True)`; not switched → `no-switch already-active` (detail = result reason), NO_ACTION; else write `schemaVersion=1`, `lastSwitchAt=clock()`, `lastSwitchTo=number`, `lastSwitchFrom=result.from.number`, `leftHeadroom, leftRecoveryAt = left` (inf → null), `leftTrigger=trigger`. Outside the lock emit `switch(trigger, from=result.from, to=result.to, warnings=result.warnings)`, SWITCHED.

### 6.9 All-exhausted recovery time (`_earliest_recovery`)
For each account whose decision value is a dict: `blocked = relevant windows with pct >= 100`; none → skip. `usable_at = limiting_reset_ts` (latest reset among blocked windows); `None` **or `<= now`** → return `None` immediately (recovery unprovable — never oversleep toward another account's later reset). Answer = min `usable_at` across exhausted accounts (active included), as UTC datetime.

### 6.10 Quarantine lifecycle
- `_quarantine(num, email, reason)`: `fingerprint = credential_fingerprint(creds)`; state `quarantine[num] = {email, reason, at: now-iso, refreshTokenFingerprint}`; emit `account-quarantined`.
- `_release_recovered_quarantines` (real ticks only, top of tick): for each entry, `account_email(num)` empty or ≠ entry email → release `account-replaced`; else current `credential_fingerprint` ≠ stored → release `credentials-replaced`; drop entries under the lock and emit `account-unquarantined`. Dry-run never releases.
- Quarantined slots are excluded from candidates and from poll scheduling. (The usage store has its own, independent dead-token quarantine keyed on `authDeadStrikes`/`struckFingerprint`.)

### 6.11 Cooldown, hysteresis, anti-flap
- `_in_cooldown(state)` = `lastSwitchAt` numeric and `clock() - lastSwitchAt < cooldown_seconds` (default 300, range 0–86400). Set only on a real switch. Applies to `proactive` and `consume-first`; checked before ranking and again under the state lock.
- Hysteresis (`hysteresis_pct`, default 10, range 0–50) applies only to `best`/`proactive`: candidate must land below the threshold **and** beat the active by the full margin (`h - active_h >= hysteresis`). `at-limit`/`failover` skip both gates.
- No-return bar [PY-NEWER] (`_no_return_account`): only for proactive/consume-first; only while `lastSwitchTo == current` (the engine still stands where it put itself; a manual switch disarms it; a pre-upgrade record without `lastSwitchTo` keeps the bar); the barred slot is `str(lastSwitchFrom)`. Released when `_left_account_recovered` is true **and** the barred account beats the active by `≥ active × 2.0` (or, with the active unreadable, sits below the threshold). If barring empties the ranking and `recovered` is true, `_rank` re-ranks without the bar.
- `_left_account_recovered` (is the account we left better than at departure?): no `lastSwitchFrom` or no `leftHeadroom` key → True (no evidence releases). Failover snapshot (`leftTrigger == "failover"`, or legacy `(null,null)`): True if `h > 100 - threshold` (landing-eligible), else True if the peer's binding reset is ≥300 s sooner than the active's and (active reset known or peer reset within 4 h). Ordinary snapshot: True if `h > active × 2 + 3` (or active unreadable and `h > 100 - threshold`); or `h >= min(leftHeadroom + 3, 100)`; or peer binding reset `< leftRecoveryAt - 300` (`leftRecoveryAt` null = inf).

### 6.12 Model-name typo guard (`_check_model_names`)
One-shot per run. `wanted` = configured names except `all` (lowercased). Bare `all` → done. `relevant` = switchable, non-quarantined, non-api-key slots; only when **every** relevant slot has a readable dict this tick: `seen` = lowercased `scoped[].name` across them; `missing` → `config-warning("autoswitch.model: {names} matches no account's usage windows — only the 5h/7d limits are being watched for it (typo?)")`. Never forces a refresh.

### 6.13 Switch mechanics invoked by the engine (`switch_to(number, json_output=True)`)
Returns `{schemaVersion, switched, from, to, strategy: "direct", reason, message, warnings}`. Already-active short-circuit (`reason: "already-active"`) unless `--force`. `_perform_switch` (§3 for locks): warn if the target has a live session (`Account-{n} ({email}) has a live session-mode Claude instance (PID …). Running the same account as both the default login and a session can make one copy's token go stale if the server rotates it. If the session later fails to authenticate, exit it and re-run 'cswap run {n}'.` — rides in `warnings` in JSON mode); pre-lock identity prefetch (`fetch_oauth_profile`, advisory); under the triple lock: back up the outgoing slot (classified `own-bytes`/`own-family`/`own-rotated`/`unresolved`/`foreign`/`foreign-synced`/`alien` — foreign/alien bytes go to the unclaimed stash instead of the outgoing slot's backup), write target creds to the active store (Keychain or `.credentials.json`; API key → `primaryApiKey` + `customApiKeyResponses.approved`), splice `oauthAccount` into `~/.claude.json` (salvaging an unparseable file aside as `.claude.json.unreadable-{ts}[.n]`), set `activeAccountNumber`; rollback in reverse on failure (`SwitchError("Switch failed and was rolled back: {e}")` / `"…and rollback also failed: {e}. Manual recovery may be needed."`). Direct-activation path (fresh machine / unmanaged live login / `--force`) skips the outgoing backup. After the lock: post-switch follow-up (`Restart Claude Code to apply immediately — otherwise the session can take up to ~30 seconds to pick up the new account.` for Keychain, `New account is active on your next message — no restart needed.` for file), `_replan_new_active`.

### 6.14 Loop timing (`run_loop`, `_next_delay`, `_respect_poll_plan`)
```
loop: wake.clear() (at TOP); if stop → return 0; outcome = tick(); delay = _next_delay(outcome)
      if delay > interval*1.5: emit sleep(seconds=delay, until=now+delay); wake.wait(delay)
_next_delay:
  BLOCKED with _sleep_until_ts → clamp(sleep_until - now, [interval, MAX_SLEEP_S=600])
  BLOCKED with _blocked_wait_long → max(interval, 300)
  BLOCKED otherwise (resolvable) → jittered normal
  NO_ACTION with _idle_hold_slow → max(interval, 300)
  else → _respect_poll_plan(interval * (0.9 + 0.2*random()))
_respect_poll_plan(delay): if the active slot has a plan, return min(delay, max(next_poll_at - now, 60))  # only ever shortens; never below URGENT 60 s
```
`stop()` sets both `_stop` and `_wake` (latching; wired to SIGTERM); `wake()` cuts the sleep short (used by `apply_threshold`). Interval default 60 (range 15–3600); normal cadence 54–66 s.

### 6.15 CLI (`cswap auto`)
Flags: `--once`, `--json`, `--interval SECONDS`, `--threshold PCT`, `--cooldown SECONDS`, `--model NAMES`, `--include-api-key-accounts` / `--no-include-api-key-accounts` (default None = settings), `--strategy {best,consume-first}` [PY-NEWER], `--dry-run`, `--debug`. Settings = `merged_with_cli(load_settings(backup), args)` (non-None overrides only, then re-clamped). Root guard: `geteuid() == 0` outside a container → `Error: Do not run this script as root (unless running in a container)`, exit 1. `--once` → `sys.exit(tick().value)`. Loop mode prints a dimmed banner `Auto-switch running: threshold {t:.0f}%, every {i:.0f}s[ (dry-run)] — Ctrl-C to stop` (human mode), exits 0 on stop, 130 on Ctrl-C (`Auto-switch stopped`). Human lines are `HH:MM:SS  {event.human()}` (switch accent, error/quarantine yellow, poll/no-switch/sleep dimmed). `ClaudeSwitchError` before the loop → error envelope (compact in `--json`) or `Error: {e}`, exit 1.

---

## 7. Settings keys (`settings.py` `SETTING_SPECS`, single source of truth)

| Dotted key | JSON section.key | Field | Kind | Range / choices | Default | CLI override (`cswap auto`) | Help |
|---|---|---|---|---|---|---|---|
| `autoswitch.threshold` | `autoswitch.threshold` | `threshold` | float | 50.0 – 99.9 | 90.0 | `--threshold PCT` | Switch when the binding 5h/7d window reaches this pct |
| `autoswitch.intervalSeconds` | `autoswitch.intervalSeconds` | `interval_seconds` | float | 15.0 – 3600.0 | 60.0 | `--interval SECONDS` | Poll interval for the cswap auto loop, in seconds |
| `autoswitch.cooldownSeconds` | `autoswitch.cooldownSeconds` | `cooldown_seconds` | float | 0.0 – 86400.0 | 300.0 | `--cooldown SECONDS` | Minimum seconds between proactive switches |
| `autoswitch.hysteresisPct` | `autoswitch.hysteresisPct` | `hysteresis_pct` | float | 0.0 – 50.0 | 10.0 | (none) | A target must beat the active account by this many pct |
| `autoswitch.strategy` | `autoswitch.strategy` | `strategy` | choice | `best`, `consume-first` [PY-NEWER] (Go: `best`, `soonest-reset`) | `best` | `--strategy` | How auto-switch picks the target account |
| `autoswitch.includeApiKeyAccounts` | `autoswitch.includeApiKeyAccounts` | `include_api_key_accounts` | bool | — | false | `--include-api-key-accounts` / `--no-…` | Allow rotating onto managed API-key accounts (bill per token) |
| `autoswitch.unhealthyTicks` | `autoswitch.unhealthyTicks` | `unhealthy_ticks` | int | 1 – 100 | 3 | (none) | Consecutive failed polls before an account is unhealthy |
| `autoswitch.model` | `autoswitch.model` | `model` | string | non-empty; comma list or `all` | null (unset) | `--model NAMES` | Also switch on these models' weekly limits (e.g. Fable, Fable,Opus, or all) |
| `ui.theme` | `ui.theme` | `theme` | choice | `dark`, `light`, `auto` | `auto` | — | Color theme; auto follows the terminal background |

Load semantics (forgiving, `_clamped`): numeric fields — bool or non-number → default, else clamped into `[lo, hi]` (int fields cast to int); bool → `bool(value)`; string → kept only if a non-empty string, else default (null); choice → unknown value logs `settings.json: unsupported {key} {value!r}; using {default!r}` and uses the default. Missing file/section/`TypeError` on construction → all defaults. `parse_model_names`: split on `,`, strip, drop empties, dedupe case-insensitively (first spelling wins), `()` for null/empty.

`config set` (strict, `parse_setting_value`): bool accepts `true/1/yes` / `false/0/no` (case-insensitive) else `ConfigError("{key} expects true or false (or 1/0, yes/no), got '{v}'")`; choice must be exact (`"{key} must be one of: a, b"`); string non-empty (`"{key} expects a non-empty value; use 'cswap config unset {key}' to clear it"`); int/float parse (`"{key} expects an integer|a number, got '{v}'"`) and range (`"{key} must be between {lo} and {hi}"`); unknown key → `ConfigError("unknown setting '{key}'\nValid keys: …")`. Whole-number floats render as ints (`80.0` → `80`); `None` → `(none)`; bools → `true`/`false`. `config list --json` → `{"schemaVersion":1,"path":…,"settings":[{"key","value","isSet"},…]}` where `isSet` = key literally present in the file.

CLI merge (`merged_with_cli`): overlay only non-None argparse values for `threshold`, `interval→interval_seconds`, `cooldown→cooldown_seconds`, `include_api_key_accounts`, `model`, `strategy`; identity when nothing set; re-clamp. Note: `hysteresisPct` and `unhealthyTicks` have no flags. The manual `cswap switch --strategy {best,next-available} [--model NAMES]` reads `autoswitch.model` as its default model list (source reported as `--model` or `settings`).

---

## 8. Export / import (`transfer.py`)

### 8.1 Envelope (`FORMAT_VERSION = 1`)
```json
{
  "version": 1,
  "exportedAt": "2026-01-01T00:00:00Z",
  "exportedFrom": "macos|linux|wsl|windows|unknown",
  "swapVersion": "1.2.3",
  "encrypted": false,
  "activeAccountNumber": 2,
  "accounts": [
    {
      "number": 1, "email": "alice@example.com", "uuid": "acct-uuid",
      "organizationUuid": "org-a", "organizationName": "Acme", "added": "2024-01-01T00:00:00Z",
      "credentials": { "claudeAiOauth": { "accessToken": "...", "refreshToken": "...", "expiresAt": 1752000000000, "scopes": [...] } },
      "config": { "oauthAccount": { "emailAddress": "...", "accountUuid": "...", "organizationUuid": "...", "organizationName": "..." } },
      "kind": "api_key",
      "alias": "dev"
    }
  ]
}
```
- `credentials` is a JSON object for OAuth accounts (default slimmed to `{"claudeAiOauth": …}` only [PY-NEWER]; legacy shapes without that key export verbatim) or a raw `sk-ant-api…` string for API-key accounts (then `"kind": "api_key"`). `--full` keeps the whole credential object and the whole `~/.claude.json`; default `config` = `{"oauthAccount": …}` only (`TransferError("config for {email} is missing oauthAccount — cannot export")` if absent).
- `activeAccountNumber` = recorded active slot only if it made it into `accounts`, else null.
- `alias` present only when set; `uuid`/`organizationUuid`/`organizationName`/`added` normalized to `""` never null.
- Output: `-` → stdout (indent 2 + `\n`, no summary at all); file → `~`-expanded path, `mkstemp` beside it, 0600, `os.replace`; stderr `Exported {n} account(s) to {path}`. Skip warnings always go to stderr.

### 8.2 Export selection
`--account NUM|EMAIL|ALIAS` → exactly one; unknown → `TransferError("account not found: {id}")`; missing backup → hard `CredentialReadError("no backup credentials found for account {n} ({email})")` / `ConfigError("no backup config found for account {n} ({email})")`. Bulk: all slots sorted by int; broken slots skipped with stderr `Skipping Account-{n} ({email}): no stored credentials/config — re-add with: cswap --add-account --slot {n}`; all skipped → `TransferError("no exportable accounts — all managed slots are missing stored credentials/config. Re-add with: cswap --add-account --slot <number>")`. The **live** account is read from the live store (`_read_credentials()` / live `~/.claude.json`) instead of the backup (fresher token). No accounts → `TransferError("no accounts to export — run cswap --add-account first")`.

### 8.3 Import validation (pass 1, no writes)
Source `-` = stdin, else file (`TransferError("import file not found: {path}")`). Errors in order: invalid JSON (`"export file is not valid JSON: {exc}"`), non-object (`"export file must be a JSON object"`), `version != 1` (`"unsupported export version: {v!r} (expected 1)"`), `encrypted is True` (`"encrypted exports are not supported in this version — decrypt before piping (e.g. gpg -d backup.gpg | cswap --import -)"`), `accounts` not a non-empty list (`"export file has no accounts to import"`). Per entry: must be an object; `email` string matching the email regex (`"invalid or missing email in imported account: {email!r}"` — also the path-traversal guard); `number` int (not bool) ≥ 1 (`"invalid slot number in imported account ({email}): {n!r}"`); `organizationUuid|organizationName|uuid|added|alias` string if present (`"{field} for {email} must be a string, got {type}"`); alias normalizable (`"invalid alias for {email}: {e}"`); `config` object (`"config for {email} must be a JSON object"`); `kind == "api_key"` **or** string credentials ⇒ API key: must be a string passing `looks_like_api_key` (`"API-key credentials for {email} must be a raw sk-ant-api… string"`), stored stripped; else object (`"credentials for {email} must be a JSON object"`), stored as `json.dumps(obj)` (re-serialized). Duplicate `(email, org)` in the envelope → `"duplicate account in export: {email} (org={org or 'personal'})"`; duplicate alias → `"duplicate alias in export: {alias}"`; alias owned locally by a **different** identity → stderr `Warning: alias '{a}' for {email} already used by an existing account, dropping the imported alias` (kept when the owner is the same identity). `added` defaults to now when absent.

### 8.4 Import writes (pass 2)
`_setup_directories()`, `_init_sequence_file()`. For each entry in envelope order (roster re-read each iteration; no FileLock — parity gap the port may deliberately close):
- `existing = _find_account_slot(data, email, org)` (identity, **never** the exported number).
  - exists + `--force` → `overwrote` in place at the existing slot (snapshot `had_strike`/`same_generation` from the identity-guarded usage row first).
  - exists + no force + `_slot_token_dead(slot, email)` [PY-NEWER] → `replaced` in place (auto-heal of a refresh-token-dead quarantine).
  - exists otherwise → stderr `Skipped {email} (already exists, use --force)`, `skipped += 1`, record `resolved_active_slot` if it is the envelope's active, continue (nothing touched).
  - not existing → `imported`; `target = exported number` if that slot is free locally, else next free (`max+1`, gaps never filled).
- Live session warning for overwrite/replace: `Warning: {email} (slot {n}) has a live session-mode instance (PID a, b); its session profile keeps the pre-import credentials until it is restarted via 'cswap run'.`
- Write creds + config backups (→ `_post_backup_write` invalidates/marks the session profile), `clear_dead_token([target])` unconditionally (issue #138), then roster record `{email, uuid, organizationUuid, organizationName, added}` + `kind: "api_key"` / `alias` when applicable, `sequence` append+sort, `lastUpdated`, write.
- Per-entry stderr: `Overwrote {email} (slot {n})` (+ `  └ cleared this slot's stored dead-token strike` when it had one, + `  └ this import holds the same credential generation the strike condemned; another permanent auth failure will quarantine it again — recover with a newer export or a re-login` when the fingerprint matches the struck one), `Replaced {email} (slot {n} was quarantined: refresh token dead)`, `Imported {email} → slot {n}`.
- Active seeding: if the local `activeAccountNumber in (None, 0)` and the envelope's active account resolved locally → set it to the **resolved** local slot. Never overrides an existing local choice.
- Summary (stderr): `Done: {i} imported, {o} overwritten, {s} skipped` + `, {r} replaced (dead token)` when r > 0.
- If the live login's slot was written this run: `Note: {email} is your current live login — activate the imported credentials with: cswap --switch-to {slot} --force`.
- Mappings are never touched by import (`--force` keeps identity).

---

## 9. Session profiles and directory mappings

### 9.1 `cswap run [NUM|EMAIL|ALIAS] [--no-share] [--share-history|--no-share-history] [--debug] [-- <claude args>]`
- Bare `run`: `slot_for_directory(cwd)` → `(None, None)`: `No account mapped for {cwd} — launching the default account.` + `exec_default` (plain `claude`, env untouched); `(None, email)`: `warning("Mapped account {email} no longer exists — launching the default account.")` + `exec_default`; `(slot, email)`: run that slot.
- `run()`: `claude` must be on PATH (`SessionError("'claude' was not found on PATH. Install Claude Code first.")`); `--share-history` on Windows → `SessionError("--share-history is not supported on Windows yet: sharing uses re-synced copies there, which would fork the history instead of sharing it.")`; `resolve_account` (ambiguous email is a hard `ConfigError`); API-key accounts refused (`"Account-{n} ({email}) is an API-key account; 'cswap run' (session mode) does not support API-key accounts yet. Use 'cswap --switch-to' to make it your default login instead."`).
- Same-account fast path (only when `CLAUDE_CONFIG_DIR` is **not** preset): live identity == target → `Account-{n} ({email}) is already the active default login — launching claude directly.` and exec plain `claude` with the env untouched. A preset `CLAUDE_CONFIG_DIR` → `warning("CLAUDE_CONFIG_DIR is already set ({v}); overriding it for this launch.")` and no fast path.
- Env scrub for the session launch and the auth probe (never for the fast path / default launch): `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR` → `Ignoring {vars} for this session — it would override the selected account inside Claude Code.`
- Launch: `Launching Account-{n} ({email}) [session mode]`; `env[CLAUDE_CONFIG_DIR] = session_dir`; POSIX `execvpe` (never returns; no lock held across exec), Windows subprocess + mirror exit code (Ctrl-C → 130).

### 9.2 `setup_session` (bootstrap/reuse)
1. `stale = is_session_stale(dir) and profile_is_quiescent(dir)` (marker honored only with no live PIDs).
2. Lock-free reuse: `not stale and _is_session_valid` → `_sync_sharing`, return.
3. [PY-NEWER] Pre-lock refresh through `consume_backup_grant` when the backup has a refresh token (setup-tokens skip silently). A demoted outcome (error **with** credentials) refuses: stashed → `SessionError("Account-{n}'s refreshed credential could not be stored, so the backup still holds a spent grant. The successor is stashed — please retry, and the next run adopts it automatically.")`; not stashed → `SessionError("Account-{n}'s refreshed credential could neither be stored nor stashed, so the backup holds a spent grant and the successor is gone. Fix the storage failure first; retrying before that spends nothing but earns a strike. If the slot strikes, log in again and re-add it: cswap --add-account --slot {n}")`. A plain failure → `warning("Could not refresh the token for Account-{n}; continuing with the stored credentials.")`.
4. Under `FileLock(lock_file, timeout=30)`: re-check stale (→ `_invalidate_session_credentials` + clear markers); if valid now: re-bootstrap only when the profile's credential fingerprint differs from the backup **and** the profile is quiescent, then sync, return. Else `_bootstrap`, `_sync_sharing`, validate: `unknown` verdict + local artifacts usable → treat as valid; `unknown`/`unreachable` → `SessionError("Session profile for Account-{n} ({email}) could not be verified: `claude auth status` did not run or did not answer. The profile is left in place — check that `claude` is on PATH, then retry.")`; invalid → cleanup (delete Keychain entry + rmtree) and `SessionError("Session profile for Account-{n} ({email}) failed validation. Log in with that account and re-add it: cswap --add-account --slot {n}")`.
5. `_bootstrap`: delete the profile's hashed Keychain entry first; creds required (`"Account-{n} has no stored credentials. Re-add with: cswap --add-account --slot {n}"`); backup config `oauthAccount` required (`"Account-{n} has no stored config backup. Re-add with: …"`); `mkdir` 0700; write `.credentials.json` verbatim 0600; merge into existing `.claude.json`: `oauthAccount`, `hasCompletedOnboarding = true`, `theme` setdefault (backup theme or `"dark"`); 0600.
6. `_is_session_valid`: `claude auth status --json` (via `shutil.which`, timeout 10 s, scrubbed env with `CLAUDE_CONFIG_DIR=session_dir`): rc 0, JSON, `loggedIn is True`, `authMethod == "claude.ai"`, `email == email`, org compared only when both non-empty.

### 9.3 Sharing (`_sync_sharing`, every launch, lock-free)
`active = SHARED_ITEMS if share else () + HISTORY_ITEMS if share_history (forced off on Windows) else ()`. Source is always the literal `~/.claude` (never `CLAUDE_CONFIG_DIR`). Prune manifest-listed items no longer active (never a real non-symlink history dir/file); no active items → delete manifest. For each item: history items go through `_prepare_history_share` first (merge a real profile copy into `~/.claude` when no session is live — dir merge deepest-first, filename collisions keep the source copy; `history.jsonl` merged line-wise deduped; seed a missing source with 0700-every-level dirs / 0600 file; defer with `Not sharing {name} yet: another session is using this profile — retrying on the next launch.`); missing source → prune + skip; existing symlink → adopt, repoint if wrong (POSIX) or unlink+copy (Windows); existing real path not in manifest → `Not sharing {name}: the session profile already has its own copy.`; else symlink (POSIX) / `copytree`/`copy2` (Windows), warnings on OSError. Manifest `{"items": [...], "mode": "symlink"|"copy"}` written atomically.
MCP mirror (`_sync_mcp_servers`, first step of every sync): one-way copy of the default profile's top-level `mcpServers` (from `get_default_global_config_path()`) into the profile's `.claude.json`; `--no-share` removes it only from adopted profiles (marker `.cswap-mcp-mirror-v1`); the first adoption stashes displaced session-local entries to `.cswap-mcp-displaced.json` (write-once; an invalid squatter blocks the reset); the splice runs under the profile's `.claude.json.lock` proper-lockfile and fails open on every malformed input / lock timeout.

### 9.4 Directory mappings
- `cswap map [NUM|EMAIL|ALIAS] [PATH]` (no account → list), `cswap unmap [PATH]` (default cwd). Mapping a non-existent directory is allowed with `Warning: {path} is not an existing directory (mapping it anyway)`. Output `Mapped {path} → Account-{n} ({email}) [(was {prev_email})]`, `Unmapped {path}` / `No mapping for {path}`, list rows `  {path} → {slot}: {email} [{tag}]` or `  {path} → {email} (account removed)`.
- Lookup (`MappingStore.resolve(cwd)`): a mapping matches when its key equals the normalized cwd or is one of its `Path.parents` (component-aware, not string-prefix); the **longest key** wins (deepest ancestor); linear scan; None when nothing matches. `slot_for_directory` then resolves `(email, organizationUuid)` → slot via the roster (three-state result).
- Pruning: `remove` and slot-**overwrite** prune; slot-migration (`add --slot` same identity), `move`, `swap`, `import --force` keep mappings.

### 9.5 Process detection
Reads Claude Code's `<config>/sessions/{pid}.json` (`pid`, `sessionId`, `cwd`, `startedAt` ms, `kind`, `entrypoint`, `status`) and `<config>/ide/{port}.lock` (`pid`, `ideName`, `workspaceFolders`); keeps only `is_pid_alive(pid)` (`pid <= 1` → dead; POSIX `kill(pid, 0)`, `EPERM` = alive; Windows `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`). `live_sessions_for(session_dir)` = the same scan against a profile dir; `scan_live_sessions` also counts unreadable records.

---

## 10. Claude-specific facts (to be replaced by Codex equivalents in `ccsw`)

Paths and env:
- Config home: `CLAUDE_CONFIG_DIR` if set else `~/.claude`. Global config: `<config_home>/.config.json` if it exists (legacy) else `(CLAUDE_CONFIG_DIR or $HOME)/.claude.json` (note: at home root, not inside `.claude/`). Credentials file: `<config_home>/.credentials.json`. Default-profile variants ignore `CLAUDE_CONFIG_DIR`.
- `CLAUDE_SECURESTORAGE_CONFIG_DIR` (Claude ≥ 2.1.220): redirects Claude's secure store; cswap mirrors it on capture and refuses to consume grants while it is set (`store-unmirrored`).
- A `CLAUDE_CONFIG_DIR` inside `<backup>/sessions/` is a cswap session pin: other commands ignore it and print `This shell is pinned via cswap env; operating on the default login.` [GO-ONLY `cswap env`].
- Auth-override env vars scrubbed in session mode: listed in §9.1.

`~/.claude.json` fields cswap reads/writes: `oauthAccount{emailAddress, accountUuid, organizationUuid, organizationName, organizationRole, displayName}` (identity; spliced on switch), `primaryApiKey` (managed API key when not in Keychain), `customApiKeyResponses{approved: [last 20 chars of key], rejected: []}`, `mcpServers` (mirrored into session profiles), `hasCompletedOnboarding`, `theme`, `projects` (preserved). Written with `json.dumps(indent=2)` preserving unknown keys, atomic, 0600, under `~/.claude.json.lock`.

Credential formats:
- OAuth: `{"claudeAiOauth": {"accessToken": "sk-ant-oat01-…", "refreshToken": "…", "expiresAt": <epoch ms>, "scopes": [...], "subscriptionType": "...", "rateLimitTier": "..."}, "organizationUuid": "..."}` — siblings preserved on refresh; `expiresAt` in **milliseconds**; expired when `now_ms + 300000 >= expiresAt`.
- Setup token (`add-token`): `{"claudeAiOauth": {"accessToken": "<token>", "scopes": ["user:inference"]}}` (no refresh token → never refreshable; `no_refresh_token` is permanent).
- Managed API key: bare string `sk-ant-api…` (`looks_like_api_key`); active store = Keychain `Claude Code` or `primaryApiKey`; OAuth and API key are mutually exclusive (activating one clears the other, mirroring `saveApiKey`/`removeApiKey`).
- add-token synthetic config: `{"oauthAccount": {"emailAddress": "<email>", "accountUuid": "", "organizationUuid": null, "organizationName": null}}` (nulls in the config blob, `""` in the roster record).

Endpoints (User-Agent `claude-swap/1.0`, system CA, no proxy/retry library):
- Token refresh `POST https://platform.claude.com/v1/oauth/token`, body `{"grant_type":"refresh_token","refresh_token":"…","client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e"}` (no scope), `Content-Type: application/json`, timeout 10 s. Response `access_token`, `expires_in` (s → `expiresAt = now_ms + expires_in*1000`), optional `refresh_token` (rotation), `scope` (space-delimited → `scopes`), optional `account{uuid,email_address}` / `organization{uuid}` (→ `token_account`). Permanent (`invalid_grant`) only when HTTP 400/401/403 **and** body contains `invalid_grant` or `invalid_client` (case-sensitive substring); everything else `transient`.
- Profile `GET https://api.anthropic.com/api/oauth/profile` (Bearer, timeout 5 s; 401 → proceed without identity) → `{account:{uuid,email}, organization:{uuid}}`.
- Usage `GET https://api.anthropic.com/api/oauth/usage` (Bearer + `anthropic-beta: oauth-2025-04-20`, timeout 5 s) — shape in §4.1. Budget model in §5.
- PyPI update check `https://pypi.org/pypi/claude-swap/json`, 2 s timeout, 24 h cache (`cache/update_check.json`).

macOS Keychain interop: `/usr/bin/security` only (never a framework binding; creator == reader avoids prompts); `find-generic-password -a <acct> -w -s <svc>` (strip exactly one trailing `\n`; rc 44 = absent; rc 51 etc. = error); `add-generic-password -U -a … -s … -X <hex>` via `security -i` stdin when the command line ≤ 4032 bytes else argv; `delete-generic-password` (rc 0/44 ok); 5 s timeout per call; attribute-only `item_exists` (no `-w`, never prompts). Session-profile items use service `Claude Code-credentials-{sha256(NFC(raw CLAUDE_CONFIG_DIR string))[:8]}` and are read/deleted but never written by cswap. Keychain propagation latency ~30 s (why proactive switching + the 10-min freshen buffer exist).

Claude Code lock protocol: npm `proper-lockfile` dirs `~/.claude.lock` / `~/.claude.json.lock`, 10 s staleness, 5 s touch (cswap 3 s), Claude retries a held credentials lock 5× with 1–2 s jitter; Claude aborts its own refresh if the re-read token is not expired; Claude invalidates its memoized OAuth token only when `.credentials.json`'s mtime changes or the file is absent (hence the rewrite-when-present `_refresh_stale_credentials_file` after a Keychain write).

Claude Code process files: `<config>/sessions/{pid}.json`, `<config>/ide/{port}.lock`; entrypoint labels `cli→CLI`, `claude-vscode→VS Code`, `claude-desktop→Desktop`, `sdk-cli|sdk-ts|sdk-py→SDK`, `mcp→MCP`, `local-agent→Agent`, `remote→Remote`. `claude auth status --json` (`loggedIn`, `authMethod: "claude.ai"`, `email`, `orgId`) is the local session-validity probe.

Legacy storage (migrations only): python `keyring` service `claude-code` (macOS + Windows Credential Manager, ~2500-byte limit) → migrated to `claude-swap` Keychain service / `.enc` files; `~/.claude-swap-backup` → XDG path on Linux/WSL.

---

## 11. Error taxonomy and exit codes (for JSON `error.type` round-tripping)

`ClaudeSwitchError` ← `ConfigError`, `SwitchError`, `SessionError`, `ValidationError`, `AccountNotFoundError`, `TransferError`, `MigrationError`, `MigrationIncomplete`, `LockError` ← `ClaudeCodeLockTimeout`, `CredentialError` ← `CredentialReadError`, `CredentialWriteError`. Handled errors → exit 1 with `Error: {msg}` or `{"schemaVersion":1,"error":{"type":"<ClassName>","message":"…"}}`. Argparse usage errors → 2. `auto --once` → 0/1/2/3. SIGINT → 130. `SCHEMA_VERSION = 1` on every JSON payload; additive-only evolution (optional keys omitted when not applicable).
