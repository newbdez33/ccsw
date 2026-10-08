# ccsw — Claude provider design

Status: draft, 2026-10-07. Author: Jacky (with Claude). Amends
`docs/specs/2026-09-29-ccsw-design.md` (the v0.1 design). Where this document is
silent, the v0.1 design and the research notes in `docs/research/` (cswap CLI contract,
cswap TUI, cswap data model / auto-switch) are the contract; where they disagree with this
document, this document wins.

## 1. Purpose

`ccsw` becomes a multi-account switcher for **both** the OpenAI Codex CLI and Claude
Code. One roster, one dashboard, one set of commands. The Claude mechanics are ported
from claude-swap (`cswap` 0.25.0, MIT, already attributed in `NOTICE`) into a `src/claude/`
module tree that mirrors `src/codex/`; nothing shells out to the Python `cswap`.

The user-facing rule that shapes everything else: **account numbers are one global space.**
`ccsw switch 2` looks the slot up and acts on whichever provider the account belongs to.
No command needs a provider prefix to name an account.

## 2. Scope

In (phased, §16): `provider` on every roster record; `src/claude/` (paths, Keychain,
credentials, Claude Code locks, OAuth refresh, usage); `add` that captures both live
logins; `add-token` that recognises Anthropic tokens; provider-aware `switch`, `list`,
`status`, `auto`, `run` / `env` / `map` / `unmap`, `export` / `import`; the TUI with a
section per provider.

Out (deliberate, documented): the macOS menu bar, the `unclaimed` stash and the identity
oracle (`foreign_credential` is never produced), process detection (`Running instances:`),
user-scope MCP mirroring in session mode, Claude Code's `~/.claude.json` config snapshots
beyond `oauthAccount`, self-upgrade.

## 3. Concepts

**Provider** — `codex` or `claude`. Every account has exactly one. The two words are
reserved: they are refused as aliases and accepted as a *selector* argument wherever an
account identifier is optional (§6.1).

**Account** — a saved login of one provider. Slots stay global, sparse and never re-packed;
`add` allocates `max(existing)+1` across both providers.

**Identity** — `(provider, email, organizationUuid)`. For Codex `organizationUuid` is the
Codex account id as in v0.1. For Claude it is `oauthAccount.organizationUuid` from
`~/.claude.json`, and `uuid` is `oauthAccount.accountUuid`. Token accounts of either
provider have an empty `organizationUuid`.

**Active login** — one per provider, re-derived from the live files on every command (v0.1
§3). The roster caches both: `activeAccountNumber` keeps the Codex slot for compatibility
and `activeByProvider` carries both.

**Display tag** — Codex: workspace name, else plan label, else `personal` (v0.1). Claude:
`organizationName`, else `personal` (cswap's rule).

## 4. Claude mapping (cswap → `src/claude/`)

| cswap (Python) | ccsw (`src/claude/`) |
|---|---|
| config home `CLAUDE_CONFIG_DIR` else `~/.claude`; global config `<home>/.config.json` if it exists (legacy) else `(CLAUDE_CONFIG_DIR \|\| $HOME)/.claude.json`; credentials `<home>/.credentials.json` | `paths.rs`, same three rules; `..` components rejected as for `CODEX_HOME` |
| live OAuth credential: Keychain service `Claude Code-credentials`, account `$USER` (else the OS user name, else `claude-code-user`); then `.credentials.json`; then the managed API key (Keychain service `Claude Code`, then `primaryApiKey` in the global config) | `credentials.rs::read_live()` in that order; a Keychain failure flips the process to the file backend for the rest of the run (sticky) and reports `keychain_unavailable` when nothing else covered it |
| `macos_keychain.py`: `/usr/bin/security find-generic-password -a <acct> -w -s <svc>` (rc 44 = not found), `add-generic-password -U -a -s -X <hex>` through `security -i` on stdin (argv fallback above 4032 bytes), `delete-generic-password`; 5 s timeout per call | `keychain.rs`, same commands, same absolute path, same limits; compiled on every platform, callable only on macOS |
| credential object `{"claudeAiOauth": {accessToken, refreshToken, expiresAt (ms), scopes, …}, …siblings}`; siblings such as `mcpOAuth` are machine-scoped and must survive a switch | the slot file stores the account's `claudeAiOauth` plus its `oauthAccount`; a switch replaces only `claudeAiOauth` inside the live object and keeps every other top-level key |
| managed API key `sk-ant-api…` (`kind: api_key`); setup-token `sk-ant-oat…` wrapped as `{"claudeAiOauth": {"accessToken": …, "scopes": ["user:inference"]}}` | same wrapping; the slot file is `{"primaryApiKey": "sk-ant-api…", "oauthAccount": {…}}` for a managed key |
| Claude Code locks (`claude_locks.py`): directory locks `<home>/.oauth_refresh.lock` then `<home>.lock` (`~/.claude.lock`), stale 60 s; `~/.claude.json.lock`, stale 10 s; touched every 3 s; 9 s wait budget per lock with 1–2 s jittered sleeps | `locks.rs`, identical protocol and constants; timeout → `LockError("Claude Code is holding <lock>; retry in a moment")`; no network while held |
| refresh `POST https://platform.claude.com/v1/oauth/token`, JSON `{grant_type: "refresh_token", refresh_token, client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e"}`; response `access_token`, `expires_in`, optional `refresh_token`, `scope` | `oauth.rs`; `expiresAt = now_ms + expires_in·1000`, `scopes = scope.split(' ')`; expiring = `now_ms + 300 000 ≥ expiresAt`; override `CCSW_CLAUDE_TOKEN_URL` |
| usage `GET https://api.anthropic.com/api/oauth/usage`, headers `Authorization: Bearer`, `anthropic-beta: oauth-2025-04-20`, `User-Agent: claude-swap/1.0` | `usage.rs`; same headers, `User-Agent: ccsw/<version>`; override `CCSW_CLAUDE_USAGE_URL`; timeouts as the Codex client |
| windows `five_hour`, `seven_day`, `extra_usage` → `spend`, `limits[]` with `scope.model.display_name` → `scoped[]` | `NormalizedUsage` gains `spend: Option<Spend>`; `five_hour`/`seven_day`/`scoped` as today; `limited` false, `plan_type` and `reset_credits` absent |
| restart follow-up: Keychain `Restart Claude Code to apply immediately — otherwise the session can take up to ~30 seconds to pick up the new account.`; file `New account is active on your next message — no restart needed.` | verbatim, chosen by the backend the live write landed on |
| session profile `CLAUDE_CONFIG_DIR`, Keychain service `Claude Code-credentials-<sha256(dir)[:8]>` per profile | `session.rs` (phase 3, §10) |
| env scrub `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR` | same list, `run` only |
| `'claude' was not found on PATH. Install Claude Code first.` | same text |
| sentinels `token expired — refresh deferred this pass; retries automatically`, `keychain unavailable — locked or in use; try again`, `re-login needed — refresh token dead; log in with Claude Code, then run: cswap add` | same text with `ccsw add`; `UsageSentinel::KeychainUnavailable` added, `usageStatus: keychain_unavailable` |

## 5. On-disk layout (`$CCSW_HOME`)

Unchanged files keep their v0.1 shape. Deltas:

`sequence.json`:

```json
{
  "activeAccountNumber": 2,
  "activeByProvider": {"codex": 2, "claude": 5},
  "lastUpdated": "2026-10-07T10:00:00Z",
  "sequence": [1, 2, 5],
  "accounts": {
    "2": {"provider": "codex",  "email": "…", "uuid": "…", "organizationUuid": "…", "organizationName": "", "planType": "pro", "added": "…"},
    "5": {"provider": "claude", "email": "…", "uuid": "<accountUuid>", "organizationUuid": "<org uuid>", "organizationName": "Acme", "added": "…", "alias": "cc"}
  }
}
```

- `provider` is always written; absent on read means `codex`, so a v0.1 store loads as is.
- `activeByProvider` is additive; `activeAccountNumber` mirrors `activeByProvider.codex`.
- `credentials/<n>.json` (0600) holds the provider's own object: Codex `auth.json` as in
  v0.1; Claude `{"claudeAiOauth": {…}, "oauthAccount": {…}}` or
  `{"primaryApiKey": "sk-ant-api…", "oauthAccount": {…}}`. `oauthAccount` is stored
  verbatim from the live global config (every key, not just the four identity fields) so a
  switch can splice it back unchanged. Token accounts get the synthesized
  `{"emailAddress": <email>, "accountUuid": "", "organizationUuid": null, "organizationName": null}`.
- `backups/claude/<unix_nanos>.json` — the outgoing live Claude login
  `{"credentials": <live object>, "oauthAccount": <live>}`, three kept, written before every
  Claude switch (the Codex side keeps its `auth.json.bak.*` in `$CODEX_HOME`).
- `mappings.json` entries gain `provider`; a directory may map one account per provider.
- `autoswitch_state.json` keeps its v1 shape: one engine (Claude, §9) owns it. Quarantine
  entries that earlier Codex runs left behind are inert (Codex slots are never candidates).
- `cache/usage.json` is unchanged (rows are keyed by slot and carry the row's identity).
- `sessions/<n>-<slug>/` is a `CODEX_HOME` or a `CLAUDE_CONFIG_DIR` depending on the slot.

The Codex credential-store gate (`cli_auth_credentials_store` must be absent or `"file"`)
is enforced only when a command touches a Codex account or captures the Codex live login,
so a Claude-only user is never blocked by it.

## 6. Command contract

### 6.1 Provider selector

`codex` and `claude` are accepted as the positional argument of `switch`, `add`, `list`,
`status` and `run` to mean "this provider"; `auto` accepts only `claude`, its sole provider (§9). They never collide with slots (digits),
emails (contain `@`) or aliases (the two words are rejected by alias validation with
`ValidationError("alias 'claude' is reserved for the provider selector")`).

When a command that takes an optional identifier gets none and **both providers have
accounts**, it needs the selector: `ConfigError("Both Codex and Claude accounts are managed —
say which: ccsw <verb> codex | ccsw <verb> claude")`. When only one provider has
accounts the command behaves exactly as v0.1, so single-provider stores see no change.

### 6.2 Deltas per command

| command | delta |
|---|---|
| `add [codex\|claude] [--slot N] [--alias NAME]` | Without a selector, reads both live logins. Each login that is **not** yet managed is added (so one run may add two accounts, each with its own `Added Account <n>: <email> [<tag>]` line). A login that is already managed is refreshed in place as in v0.1 and reported with `Updated credentials for Account <n> …`. When both live logins were already managed, those two lines are followed by `Both current logins were already managed: Account-2 (codex), Account-5 (claude) — nothing new was added.` (exit 0). No live login at all → `ConfigError("No active Codex or Claude login found. Log in first.")`; with a selector and no login for that provider → `ConfigError("No active Claude account found. Please log in first.")` (or `Codex`). `--slot` / `--alias` require exactly one login to be added; with two → `ValidationError("--slot/--alias need a single login; two new logins were found. Add one at a time: ccsw add codex …")`. A live Claude managed API key is captured like a Codex one (kind `api_key`, email `api-key-<slot>@token.local`). |
| `add-token [TOKEN\|-] [--email] [--slot]` | Kind by prefix: `sk-ant-api…` → Claude API key; `sk-ant-oat…` → Claude setup-token (OAuth object, no refresh token, never refreshed, sentinel-free); anything else not starting with `{` → OpenAI API key (v0.1). Emails default to `api-key-<n>@token.local` / `setup-token-<n>@token.local`. |
| `switch [ID\|codex\|claude] [--strategy] [--model] [--force]` | `ID` resolves across the whole roster and the switch runs on the record's provider (§7). Rotation and `--strategy` operate within one provider: the selector, else the only provider with accounts, else the §6.1 error. `--model NAMES` is matched against the target provider's windows only. |
| `list [codex\|claude]` | Human: when both providers have accounts the table is two blocks, `Codex accounts:` then `Claude accounts:`, each with the v0.1 row format and the global slot numbers; a single-provider store prints the v0.1 `Accounts:` block. Claude rows add `$$: $<used> / $<limit>` first when `spend` is present. The selector filters. |
| `status [codex\|claude]` | One `Active account:` block per provider that has a live login (`Codex:` / `Claude:` prefixes when both exist). |
| `remove` / `disable` / `enable` / `alias` / `move` / `swap` | Unchanged: they already act on slots. |
| `run [ID\|codex\|claude] [-- …]` / `env ID` / `map ID DIR` / `unmap DIR [codex\|claude]` | Provider from the record. `run` executes `codex` or `claude` with `CODEX_HOME` or `CLAUDE_CONFIG_DIR` pinned; `env` prints the matching export. A bare `run` in a mapped directory with mappings for both providers needs the selector (§6.1 error). `unmap DIR` with two mappings removes both unless a selector is given. |
| `auto [claude] …` | Runs the Claude Code engine (§9). `auto codex`, or `auto` on a roster with no Claude Code accounts, fails with `Auto-switch covers Claude Code accounts only: Codex sessions do not pick up a switched account without a restart. Add a Claude Code account with 'ccsw add claude' first.` (exit 1). |
| `config` | Unchanged keys. `autoswitch.model` is matched against the Claude accounts' window names (`Fable`, …); a name no account reports raises a one-shot `config-warning` event (§9). |
| `export` / `import` | §11. |
| `purge` | Also removes `backups/`. Never touches `$CODEX_HOME` or the Claude config home. |
| `help`, `--version` | Header `Multi-Account Switcher for OpenAI Codex and Claude Code`; every synopsis that takes an identifier mentions the selector. |

Legacy `--flag` spellings keep working with the same rules (`--switch` is rotation and
follows §6.1; `--switch-to N` is global).

### 6.3 JSON (`schemaVersion: 2`)

- Every account row gains `provider`.
- `list`: `activeAccountNumber` keeps the Codex slot; `active` is added as
  `{"codex": <n|null>, "claude": <n|null>}`.
- `status`: `active` becomes `{"codex": <row|null>, "claude": <row|null>}`.
- `switch`: `from` / `to` refs gain `provider`.
- `auto --json` events carry `provider: "claude"` and move to `schemaVersion: 2`.
- `usage` gains `spend {used, limit, pct, currency, resetsAt?}` on Claude rows;
  `usageStatus` adds `keychain_unavailable`.
- The error envelope is unchanged apart from the version.

## 7. Switching a Claude account

Same shape as v0.1 §7.2 with Claude's own locks and backends:

1. Resolve the slot; load its slot file (`SwitchError` if missing or without
   `oauthAccount`).
2. Take, in order, the store lock, `<home>/.oauth_refresh.lock`, `<home>.lock`, then
   `~/.claude.json.lock` (§4 constants). Under them:
   a. Read the live login (Keychain → file → managed key) and the live `oauthAccount`. If
      it belongs to a managed slot, fold the live `claudeAiOauth` back into that slot when
      it is newer (different `refreshToken` and a strictly larger `expiresAt`, or a managed
      key); an unmanaged live login is left alone with the v0.1 warning.
   b. Write `backups/claude/<unix_nanos>.json` (keep 3).
   c. Write the target credential: OAuth → `claudeAiOauth` replaced inside the live object
      (siblings kept) and written to the Keychain when usable, with an already-present
      `.credentials.json` rewritten (never created) so a running session hot-reloads;
      otherwise to the file, with any stale Keychain item deleted best-effort and the
      process pinned to the file backend. Managed key → Keychain service `Claude Code`
      when usable, else `primaryApiKey` in the global config; the other kind's locations
      are cleared. Splice `oauthAccount` into the global config, preserving every other key
      (atomic write, 0600).
   d. Set `activeByProvider.claude`, bump `lastUpdated`.
3. Outside the locks: print the follow-up line for the backend used (§4).
4. Log `Switched from account <a> to <b> (claude)`.

`--force` rewrites the live login from the slot file without the fold-back, as in v0.1.
A Codex switch is unchanged (v0.1 §7.2) and never touches Claude files, and vice versa.

## 8. Usage and token refresh (Claude)

- Fetch and classification as v0.1 §8.1 (429 / Retry-After, `http-<code>`, `timeout`,
  `network`, `bad-response`), against the Anthropic endpoint and headers of §4.
- Normalization: `five_hour` / `seven_day` from `utilization` + `resets_at`; `spend` only when
  `extra_usage.is_enabled` and `used_credits`, `monthly_limit`, `utilization` are all present
  (credits are cents, ÷100; `currency` defaults `USD`); `scoped[]` from `limits[]` entries
  with a `scope.model.display_name` and a numeric `percent`.
- Rows (human, TUI): `$$` (when present), `5h`, `7d`, then each scoped window by name —
  cswap's order. Codex rows keep the v0.1 order and the `credits` line.
- Refresh discipline: an **inactive** Claude account is refreshed when `expiresAt` is within
  the 5-minute buffer (and reactively on 401/403); the rotation is persisted into the slot
  file with compare-and-swap on the presented refresh token. The **active** Claude account
  is never refreshed by ccsw — Claude Code owns it; a 401 on it yields the `token
  expired` sentinel until Claude Code rotates the token. Terminal verdicts
  (`invalid_grant`, `invalid_client`, any 4xx except 429/408 that names the grant) →
  `relogin_required`, strike bound to the SHA-256 of the refresh token (v0.1 §8.3).
  A stored copy of the live refresh token is also protected, even under another identity.
  If the live credential cannot be read, no Claude token is refreshed. Auto-switch does
  not target slots that hold the live credential, and checks ownership again before switching.
  Setup-tokens have no refresh token and are never refreshed nor struck.
- Poll policy, cache, TTLs and the on-demand pass are the v0.1 §8.4 rules; the pass runs
  per provider (active account plus one due candidate for each).
- `usage.limited` is never set for Claude; headroom comes from the windows alone.

## 9. Auto-switch: Claude Code only

Codex cannot be switched under a running session: Codex CLI 0.157+ attaches sessions to an
app-server daemon that loads `auth.json` once and re-reads it only for the account it
already holds, and `codex exec` / `--no-daemon` read it at start-up. A switch therefore
only reaches sessions started (or reconnected after a daemon restart) afterwards, which is
what the manual `switch` follow-up already says. Claude Code picks a switched login up
inside the running session (next message on the file backend, ~30 s on the Keychain), so
proactive switching is useful there and nowhere else. v0.1's Codex auto-switch is withdrawn.

- `ccsw auto [claude] …` runs one engine over the Claude Code accounts: the active
  account is the live Claude login (`collect::live_login_for(Claude)`), the candidates are
  the switchable Claude slots, refresh and switch go through the phase-1 Claude paths.
  Settings, flags, exit codes, events, `no-switch` reasons, banner and signal handling are
  v0.1 §9.
- `auto codex`, or `auto` on a roster without a Claude Code account, fails before any tick
  with `Auto-switch covers Claude Code accounts only: Codex sessions do not pick up a
  switched account without a restart. Add a Claude Code account with 'ccsw add claude'
  first.` (exit 1).
- The engine body is v0.1 §9 unchanged, with two Claude additions: a `keychain unavailable`
  active account stays held (`no-switch active-idle`) until its login is readable and
  never triggers failover. The existing 30-minute cap still applies to `token expired`,
  and the `active-idle` detail names Claude Code.
- Events carry `provider: "claude"` and `schemaVersion: 2`; human lines are unchanged.
- `autoswitch.model` names that no Claude account's windows report raise one
  `config-warning` per run (`autoswitch.model: <names> matches no account's usage windows —
  only the 5h/7d limits are being watched for it (typo?)`), evaluated on the first tick
  where every candidate's usage is readable.
- `autoswitch_state.json` keeps its v1 shape (§5). The engine keeps a `provider` field so
  the choice is one constant, but nothing constructs a Codex engine.

## 10. Session mode (Claude, phase 3)

- Profile `sessions/<n>-<slug>/` is the `CLAUDE_CONFIG_DIR`. Each launch seeds
  `.credentials.json` (the slot's live object) and `.claude.json` inside the profile with
  `oauthAccount` (the global-config path rule of §4 puts it there when the env var is set).
  On macOS the profile's own Keychain item (service `Claude Code-credentials-<sha256(dir)[:8]>`,
  hashed from the exact string exported) is deleted before seeding so Claude Code reads the
  seed instead of a stale entry.
- Shared by symlink (copy on Windows) unless `--no-share`: `settings.json`,
  `keybindings.json`, `CLAUDE.md`, `skills/`, `commands/`, `agents/`. `--share-history`
  shares `projects/` and `history.jsonl` (POSIX only). Manifest and pruning as v0.1 §10.
- Launch: `Launching Account-<n> (<email>) [session mode]`, env `CLAUDE_CONFIG_DIR=<profile>`,
  the §4 scrub list with the v0.1 `Ignoring …` line, argv `claude <tail>`; POSIX `exec`,
  Windows child + exit-code mirror. Same-account fast path: target is the active Claude
  login and `CLAUDE_CONFIG_DIR` is not preset → plain `claude <tail>`.
- After the child exits, a token Claude Code rotated inside the profile (file, or the
  profile's Keychain item) is folded back into the slot by the §7 freshness rule.
- `map` / `unmap` / nearest-ancestor lookup per provider.

## 11. Export / import

- Envelope `version: 2`; `accounts[]` gain `provider`; `credentials` is the slot-file object
  of §5 for either provider. Identity matching is `(provider, email, organizationUuid)`.
- Import accepts: a v2 `.ccsw`; a v1 `.ccsw` (every account `codex`); a cswap
  `.cswap` export (`swapVersion` present, no `provider`) — every account `claude`, its
  `credentials` string parsed into the object and `config.oauthAccount` lifted into
  `oauthAccount`; a bare `sk-ant-api…` credential becomes `{"primaryApiKey": …}`.
- `--force`, dead-token auto-replace, `--account`, slot reuse, messages and the `Done:`
  summary are v0.1 §11.

## 12. TUI

The screens keep the v0.1 §12 layout, bars, keys, modals and refresh lanes. Deltas:

- **Grouping.** `AccountsSnapshot` keeps one flat list with global numbers; every account
  carries its provider. When both providers are present, the dashboard panel, the Switch
  list, the Watch list and the auto panel render a muted section header (`codex`, then
  `claude`) above each provider's accounts; each section shows its own `● active` card and
  minis. A single-provider store renders exactly as today (no headers), so the existing
  screenshots stay valid.
- **Cursor.** The Switch and Watch card lists walk every account across sections in slot
  order; `enter` emits `SwitchTo(n)` unchanged. `b` (best) applies to the provider of the
  account under the cursor.
- **Menu.** `Add account…` → `From current logins` (the §6.2 dual capture, confirm text
  `Back up the current Codex and Claude Code logins into ccsw?`) and `From a token…`
  (modal body: `OpenAI API key, or an Anthropic setup-token / API key (sk-ant-…)`). The
  Remove and Disable submenus list accounts under the same section headers (header rows are
  not selectable). Empty state: `Use the menu below: Add account — from your current Codex or
  Claude Code login, or from a token.`
- **Auto view.** The Claude Code active card, the Claude candidates and one event log;
  `Go live` starts the Claude engine. With no Claude Code account the view shows the §9
  notice as its first log line and starts no engine.
- **Height.** The dashboard panel is truncated to the available rows as today; the second
  section's card is the first thing to disappear, which is acceptable for v1.
- Toasts, sentinel labels and follow-up texts come from the provider that acted.

## 13. Errors

No new variants. Claude Code lock timeouts are `LockError`; Keychain failures that stop a
read or write are `CredentialReadError` / `CredentialWriteError` with the `security`
diagnostic; `keychain_unavailable` is a usage status, never an error. Exit codes are v0.1
§13.

## 14. Architecture

```
src/provider.rs        Provider { Codex, Claude } (serde "codex"/"claude"), selector parsing,
                       and the LiveLogin trait: read_live, write_live, identity, backup,
                       followup_line, refresh, fetch_usage, seed_profile
src/codex/…            unchanged; implements LiveLogin
src/claude/paths.rs    config home, global config path (legacy rule), credentials path,
                       Keychain service/account names
src/claude/keychain.rs /usr/bin/security wrapper: get / set (hex via stdin) / delete / exists
src/claude/credentials.rs  live read/write across backends, sticky file fallback, sibling-key
                       preservation, oauthAccount splice, slot-file model
src/claude/locks.rs    proper-lockfile directory locks (mkdir, touch, stale, wait budget)
src/claude/oauth.rs    token refresh
src/claude/usage.rs    usage client + normalization (spend, scoped)
src/claude/session.rs  profile seeding, hashed Keychain service name (phase 3)
```

`model.rs`: `AccountRecord.provider`, `Roster.active_by_provider`, `NormalizedUsage.spend`.
`switcher.rs` dispatches on `record.provider` through the trait; `collect.rs` runs the pass
per provider; `autoswitch.rs` runs for one provider (Claude); `session.rs` and `transfer.rs` branch on it;
`cli/` parses the selector and the two-block output; `tui/` groups by provider. `Paths`
gains the Claude paths and honours `CLAUDE_CONFIG_DIR`.

## 15. Testing

- Unit tests beside each `src/claude/` module: path rules, Keychain argv and hex encoding
  (the `security` call is behind a trait so tests record argv), credential read order and
  sibling preservation, lock staleness arithmetic, refresh parsing, usage normalization
  (spend cents, scoped filtering), selector parsing, alias reservation, `add` dual-capture
  decisions, `--once` exit-code folding.
- Integration tests follow the v0.1 pattern: a fake `claude` on `PATH` (records argv), the
  axum mock gains `/api/oauth/usage` and `/v1/oauth/token` in Anthropic shapes
  (`CCSW_CLAUDE_USAGE_URL`, `CCSW_CLAUDE_TOKEN_URL`), temp `CLAUDE_CONFIG_DIR` (which
  also relocates `.claude.json`), and `CCSW_KEYCHAIN=off` to force the file backend so the
  suite is identical on macOS, Linux and Windows CI. Covered: `add` with both / one / no
  live logins, `switch` across providers with the lock directories present and stale,
  fold-back freshness, `list --json` v2 shapes, `auto --once` on a mixed roster (the Claude engine switches Claude accounts; `auth.json` is untouched), v1 store and
  v1 export compatibility, `.cswap` import.
- `tui_render` adds a mixed roster: section headers, cross-section cursor, the auto view
  with two cards. `examples/tui_screenshot.rs` renders a mixed roster for the README.
- Quality gate unchanged: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --all`.

## 16. Phases

1. Roster `provider` + `activeByProvider`; `src/claude/` paths, Keychain, credentials, locks,
   OAuth, usage; `add`, `add-token`, `switch`, `list`, `status`, `remove`, `disable`,
   `alias`, `move`, `swap`; JSON v2; the TUI sections (dashboard, switch, watch, menus).
2. Auto-switch for Claude Code only: the engine re-pointed at Claude, the `keychain unavailable`
   hold, events v2 with `provider`, the model-name warning, the TUI auto view. Codex
   auto-switch is withdrawn (§9).
3. Session mode for Claude: `run`, `env`, `map`, `unmap`, profile seeding and fold-back.
4. Export / import v2 and `.cswap` import.

Each phase ships green on the quality gate and is usable on its own.

## 17. Assumptions recorded for the owner

1. This work starts from the `add-account-browser-login` branch once it is merged (it
   touches the same TUI files and carries the v0.2.0 bump).
2. Claude backups and slot credentials are plain 0600 files under `~/.ccsw`, not Keychain
   items (cswap keeps macOS backups in the Keychain; ccsw stays file-based like the Codex
   side).
3. A live Claude managed API key is captured by `add` (cswap refuses it).
4. The active Claude account is never refreshed by ccsw.
5. `--json` moves to `schemaVersion: 2`; `status.active` changes shape.
6. The usage `User-Agent` is `ccsw/<version>`; if the endpoint's non-first-party budget
   proves tighter under that string than under cswap's, the string is the only knob to turn.
7. Claude accounts show `organizationName` or `personal` as their tag; no plan label.
8. The v1 omissions of §2 stand.
