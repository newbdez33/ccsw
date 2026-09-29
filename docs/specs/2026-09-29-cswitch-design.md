# cswitch — design

Status: v0.1 design, 2026-09-29. Author: Jacky (with Claude). Research inputs live in
`docs/research/` (cswap CLI contract, cswap TUI, cswap data model / auto-switch, Codex
porting notes from codex-switch). Where this document and a research note disagree,
this document wins; where this document is silent, the research note is the contract.

## 1. Purpose

`cswitch` is a multi-account switcher for the OpenAI Codex CLI. It keeps several Codex
logins on one machine, activates one of them as the live `auth.json`, switches between
them without re-authenticating, reports each account's 5-hour / weekly usage, switches
automatically before an account hits its limit, and runs extra accounts in parallel
terminals.

It is `cswap` (claude-swap, the Claude Code account switcher) re-targeted at Codex:

- **Interface** — the command grammar, options, human output, `--json` schema, exit codes,
  settings keys, and the full-screen dashboard (TUI) follow cswap 0.25.0. A user who knows
  `cswap` can use `cswitch` by replacing the word. Scripts written for `cswap --json` work
  against `cswitch` for every field both tools share.
- **Mechanics** — everything that touches Codex (credential file format, identity from the
  id_token, usage API, token refresh, app-server daemon restart, launch flags, isolated
  `CODEX_HOME`) is ported from codex-switch.

Written in Rust (edition 2024, MSRV 1.88); single static binary; macOS, Linux, Windows.

## 2. Scope of v0.1

In: `list`, `status`, `switch` (rotate / `<id>` / `--strategy best|next-available`
/ `--model` / `--force`), `add`, `add-token`, `remove`, `disable`, `enable`, `alias`,
`move`, `swap`, `run`, `env`, `map`, `unmap`, `auto` (loop and `--once`, JSONL events),
`config` (`list|get|set|unset|path`), `export`, `import`, `tui`, `watch`, `purge`,
`help`, `--version`, legacy `--flag` spellings, the TUI (dashboard, switch, watch, auto
screens, modals, dark/light themes).

Out (accepted grammar, deliberate behavior):

| cswap feature | cswitch v0.1 |
|---|---|
| `menubar` | exits 1: `The menu bar is not available in cswitch.` |
| `unclaimed` | not provided (no forensic stash; see §7.3) |
| `upgrade` / `update` | guidance only, exit 1: prints how to reinstall (`cargo install --git`, or a release download) |
| `list --token-status` | supported; shows stored-token expiry from the JWT `exp` claim |
| `export --full` | accepted, no effect (Codex has no per-account config snapshot) |
| `run --share-history` | POSIX only; shares `sessions/` and `history.jsonl` by symlink |
| macOS Keychain, `~/.claude.json`, Claude Code lock protocol, VS Code / Desktop / Chrome, process detection (`Running instances:`) | not applicable; omitted |
| passive update notice | omitted in v0.1 |

## 3. Concepts

**Account** — a saved Codex login: a snapshot of `auth.json` (ChatGPT OAuth tokens, or an
API key). cswitch never changes the OpenAI subscription; it only moves credentials into
and out of Codex's credential file.

**Slot** — accounts occupy numbered slots starting at 1, sparse, never re-packed; `add`
allocates `max(existing)+1`. Every command that names an account accepts `NUM`, `EMAIL`
or `ALIAS` (resolution order number → alias → email, exactly as cswap §3).

**Identity** — `(email, accountId)` where `accountId` is the `chatgpt_account_id` claim
(the ChatGPT workspace/account the login belongs to). One email can exist under several
workspaces, so both parts are needed. API-key accounts have identity `(email, "")` where
the email is `--email` or the synthesized `api-key-<slot>@token.local`.

**Active login** — the account whose credentials are in `$CODEX_HOME/auth.json`. It is
re-derived from that file on every command (identity match, then byte-hash match for
API keys), never trusted from the roster alone.

**Backup store** — `$CSWITCH_HOME`, default `~/.cswitch` on every platform. Holds the
roster, credentials, settings, mappings, usage cache, auto-switch state, session profiles
and the log (§5).

**Usage windows** — Codex enforces a 5-hour window and a weekly window per account, plus
model-specific pools (`additional_rate_limits`). cswitch reports remaining headroom as a
percentage and treats an account as *at limit* when a relevant window reaches 100 % (or
the API flags the account as limited).

**Session profile** — a private `CODEX_HOME` under `sessions/` holding one account's
credentials, used by `run` / `env` so one terminal can run a different account than the
machine-wide login.

## 4. Codex mapping (cswap → cswitch)

| cswap (Claude) | cswitch (Codex) |
|---|---|
| live login = `~/.claude.json` `oauthAccount` + `~/.claude/.credentials.json` / Keychain | live login = `$CODEX_HOME/auth.json` (default `~/.codex/auth.json`); `CODEX_HOME` override; `..` components rejected |
| credential store must be readable; Keychain on macOS | `$CODEX_HOME/config.toml` must have `cli_auth_credentials_store` absent or `"file"`; `keyring`/`auto`/`ephemeral` are refused with `ConfigError` |
| identity `(email, organizationUuid)`, tag = org name or `personal` | identity `(email, accountId)`; tag = workspace name, else plan label (`Plus`, `Pro 20×`, `Team`, …), else `personal` |
| OAuth credential `{claudeAiOauth:{accessToken,refreshToken,expiresAt,…}}` | `{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","tokens":{id_token,access_token,refresh_token,account_id},"last_refresh":"…Z"}` |
| API key `sk-ant-api…` (`kind: api_key`) | `{"auth_mode":"apikey","OPENAI_API_KEY":"sk-…"}`; `add-token` accepts any non-empty token that is not a JSON object; `kind: "api_key"` |
| setup-token (`sk-ant-oat…`) | none. `add-token` always registers an API key |
| token expiry `expiresAt` (ms) | `exp` claim of the access/id JWT; expiring = within 1800 s |
| refresh `POST platform.claude.com/v1/oauth/token` | `POST https://auth.openai.com/oauth/token` JSON `{client_id, grant_type:"refresh_token", refresh_token}`; single-use refresh tokens; CAS persistence (§8.3) |
| usage `GET api.anthropic.com/api/oauth/usage` | `GET https://chatgpt.com/backend-api/wham/usage` with `Authorization: Bearer <access_token>`, `ChatGPT-Account-ID`, UA `codex_cli_rs/0.144.1 (<os>; <arch>)` |
| windows `five_hour`, `seven_day`, `spend`, `scoped[]` | `five_hour` ← `rate_limit.primary_window`, `seven_day` ← `secondary_window` (free-plan single weekly window remapped), `scoped[]` ← `additional_rate_limits[]` named by `limit_name` (weekly window; 5h pool shown as `<name> 5h` only in list rows), `credits` ← `credits.balance` (no `spend`) |
| `--model NAMES` = per-model weekly `display_name`s | `--model NAMES` = `limit_name`s of model pools, matched case-insensitively; `all` = every pool |
| restart follow-up: Keychain ~30 s / file no-restart | after a switch that changed the live file: if the managed app-server daemon is running it is restarted (`codex app-server daemon restart`) and the follow-up reads `Restarted the Codex app-server daemon — new and reconnecting Codex sessions use Account-N.`; otherwise `New account is active for the next Codex session — restart any running codex exec / --no-daemon session.` A refused restart is a warning naming the manual command; the switch still succeeds |
| session profile `CLAUDE_CONFIG_DIR` | session profile `CODEX_HOME`; child launched as `codex --no-daemon …` when the installed Codex lists `--no-daemon` |
| shared items `settings.json`, `CLAUDE.md`, `skills/`, `commands/`, `agents/` | `config.toml` (copied, every launch), `AGENTS.md`, `prompts/`, `skills/` (symlinked; copied on Windows) |
| history `projects/` + `history.jsonl` | `sessions/` + `history.jsonl` (POSIX symlink only) |
| env scrub `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, … | env scrub `OPENAI_API_KEY`, `CODEX_API_KEY` |
| `'claude' was not found on PATH. Install Claude Code first.` | `'codex' was not found on PATH. Install the Codex CLI first.` |
| recovery hint `cswap add --slot N` | `cswitch add --slot N` |
| `ClaudeSwitchError` / `ClaudeCodeLockTimeout` | `CswitchError` (JSON `type` names in §13); no Codex lock protocol |
| README: do not `/logout` first | README: log in with the next account (`codex login`); do **not** run `codex logout` first, it may revoke the stored refresh token |

## 5. On-disk layout (`$CSWITCH_HOME`, default `~/.cswitch`)

```
sequence.json            roster (schema below)
settings.json            {"schemaVersion":1,"autoswitch":{…},"ui":{"theme":"auto"}}
mappings.json            {"schemaVersion":1,"mappings":{"<abs path>":{"email","accountId","added"}}}
autoswitch_state.json    {"schemaVersion":1,"lastSwitchAt","lastSwitchTo","lastSwitchFrom","quarantine":{…}}
.lock                    store lock (flock, 10 s)
.autoswitch_state.lock
credentials/<n>.json     the account's auth.json snapshot, 0600
credentials/<n>.json.prev  one retained previous generation
cache/usage.json         {"schemaVersion":2,"accounts":{"<n>":{…}}}  (§8.4)
cache/.usage.lock
sessions/<n>-<slug>/     session profiles (a CODEX_HOME)
cswitch.log[.1-3]        rotating log, 1 MiB × 3, created lazily
```

All files 0600, directories 0700 (POSIX). Every write is atomic: temp file in the same
directory, fsync, rename; JSON files are round-trip parsed before the rename.

`sequence.json`:

```json
{
  "activeAccountNumber": 1,
  "lastUpdated": "2026-09-29T10:00:00Z",
  "sequence": [1, 2],
  "accounts": {
    "1": {
      "email": "a@example.com",
      "uuid": "user-…",
      "organizationUuid": "acct-…",
      "organizationName": "Acme",
      "planType": "plus",
      "added": "2026-09-29T10:00:00Z",
      "alias": "dev",
      "kind": "api_key",
      "disabled": true
    }
  }
}
```

Field mapping: `uuid` = `chatgpt_user_id` (or `""`), `organizationUuid` = `accountId`
(the workspace / account id; `""` for API keys), `organizationName` = workspace name
(`""` when unknown), `planType` = last known plan wire value (additive, may be absent).
`alias`, `kind`, `disabled` are omitted when unset. `sequence` is always sorted.

Same-name keys keep cswap's meaning so a `cswap`-style `--json` consumer sees the
fields it expects; `accountId` and `planType` are exposed additively in JSON rows.

## 6. Command contract

The grammar, options, human strings, exit codes and `--json` payloads are those in
`docs/research/cswap-cli-contract.md` with these substitutions, applied everywhere:

- program name `cswitch`, data name `cswitch`, file extension `.cswitch`, export field
  `swapVersion` → `cswitchVersion` (plus `swapVersion` kept for readers that look for it);
- `Claude Code` → `Codex`, `claude` → `codex`, `Claude account` → `Codex account`,
  `cswap` → `cswitch`, `claude-swap` → `cswitch`;
- `[personal]` tag rule per §4;
- the restart follow-up lines per §4;
- `add-token` help/prompts talk about an API key only;
- help header `Multi-Account Switcher for OpenAI Codex`.

Deltas that are not pure renames:

| command | delta |
|---|---|
| `add` | reads `$CODEX_HOME/auth.json`. A ChatGPT login is identified from its id_token; no refresh is attempted on `add`. An API-key live login is accepted too (unlike cswap): it is stored as kind `api_key` under the email `api-key-<slot>@token.local`, or refreshed in place when the same key is already managed |
| `add-token` | token kinds: any value that does not start with `{` is an API key; there is no setup-token kind |
| `list` | rows: `5h`, `7d`, then one row per model pool `<limit_name>` (weekly window pct), then `credits: $<balance>` when the API reports a balance; the `$$` spend row does not exist; `usageStatus` values `keychain_unavailable` and `foreign_credential` are never produced |
| `switch` | follow-up lines per §4; a `--force` switch to the active slot rewrites the live file from the stored snapshot and restarts the daemon when the file changed |
| `run` | see §10 |
| `env` | prints `export CODEX_HOME='<dir>'` (`sh`), `set -gx CODEX_HOME '<dir>'` (`fish`), `$env:CODEX_HOME = '<dir>'` (`pwsh`); `--unset` prints the unset form |
| `config` | same nine keys and ranges; `autoswitch.model` names model pools |
| `export`/`import` | §11 |
| `purge` | removes `$CSWITCH_HOME` only; never touches `$CODEX_HOME` |

JSON: every payload carries `schemaVersion: 1`; account rows carry the cswap keys
(`number, email, organizationName, organizationUuid, isOrganization, active,
usageStatus, usage`) plus additive `accountId`, `planType`, and the usual optional
`alias`, `disabled`, `usageFetchedAt`, `usageAgeSeconds`, `lastGood*`. `usage` carries
`fiveHour`, `sevenDay` (with pace fields), `scoped[]`, and additive `credits`
(`{"balance": 12.5, "unlimited": false}`) instead of `spend`.

## 7. Switching

### 7.1 Locks

One store lock (`.lock`, flock, 10 s wait, 100 ms poll) serializes roster and credential
mutations and the whole switch body. No lock is held across network I/O. Codex has no
credential lock protocol to honor; the live file is replaced atomically.

### 7.2 Switch body (`switch_to`)

1. Resolve the target slot; load its credentials (`SwitchError` if missing).
2. Under the store lock:
   a. Read the live `auth.json`. If it belongs to a managed slot (identity match), fold it
      back into that slot when it is newer (§8.3 freshness rule: different refresh token
      and a `last_refresh` strictly newer than the stored one, or an API key). If the
      live file is unmanaged, leave it alone and warn once
      (`The live login does not match a managed account; it was left in place.`).
   b. Back up the live file to `auth.json.bak.<unix_nanos>` in `$CODEX_HOME` (keep 3).
   c. Write the target snapshot to `auth.json` (atomic, 0600).
   d. Set `activeAccountNumber`, bump `lastUpdated`.
3. Outside the lock: hash the live file before/after; if changed and
   `codex app-server daemon version` reports `{"status":"running"}`, run
   `codex app-server daemon restart`. Report per §4. Never start a stopped daemon.
4. Log `Switched from account <a> to <b>` at INFO.

Strategies, rotation, skips, no-op reasons and JSON payloads follow the cswap contract.
`next-available` and `best` use headroom over the relevant windows (5h, 7d, plus named
model pools).

### 7.3 Outgoing credential ownership

cswap classifies the departing live credential (own / foreign / alien) with an OAuth
profile call and stashes foreign bytes. cswitch keeps the simpler rule in 7.2a: identity
from the JWT decides; nothing is ever stashed. The live backup in `$CODEX_HOME`
(`auth.json.bak.*`, 3 kept) is the recovery point.

## 8. Usage and token refresh

### 8.1 Fetch

`GET https://chatgpt.com/backend-api/wham/usage`, headers as in §4, connect timeout
30 s, total 60 s, proxy from `CSWITCH_PROXY` env or `HTTPS_PROXY`/`ALL_PROXY`, OS trust
store plus bundled roots. Classification: 2xx parse; 429 → `http-429` with Retry-After
(header, then body hints); 401/403 with a refresh token → one refresh + one retry; other
non-2xx → `http-<code>`; timeout → `timeout`; transport → `network`; bad JSON →
`bad-response`.

### 8.2 Normalization

Parsed per `docs/research/codex-porting.md` §4.5 into:

```json
{"five_hour": {"pct": 42.0, "resets_at": "2026-09-30T02:00:00Z"},
 "seven_day": {"pct": 84.0, "resets_at": "…"},
 "scoped":    [{"name": "GPT-5.3-Codex-Spark", "pct": 0.0, "resets_at": "…"}],
 "credits":   {"balance": 0.0, "unlimited": false},
 "limited":   false, "plan_type": "pro"}
```

`resets_at` is `reset_at` (epoch) rendered as ISO seconds `Z`. `limited` (account
limited by the API) makes headroom 0. Free-plan single weekly window → `seven_day`.
`scoped[].pct` is the pool's weekly window when present, else its 5-hour window.

### 8.3 Refresh discipline (from codex-switch)

- The refresh token is single-use. A rotation is persisted immediately under the store
  lock with compare-and-swap on the presented refresh token: into the slot file and, when
  that slot is the active login, into the live file only if it still holds the presented
  token. `last_refresh` is stamped `%Y-%m-%dT%H:%M:%SZ`.
- A refresh is attempted for **inactive** accounts when the access or id JWT expires
  within 1800 s, and reactively on 401/403. The **active** account is refreshed only
  reactively, re-reading the live file first (Codex owns it).
- Terminal verdicts `refresh_token_reused`, `refresh_token_invalidated`, `invalid_grant`,
  `invalid_client`, `unauthorized_client`, `access_denied`, or any 4xx except 429/408 →
  `usageStatus: relogin_required`, dead-token strike bound to the SHA-256 of the refresh
  token; a newer stored credential heals it. Human sentinel:
  `re-login needed — refresh token dead; log in with Codex, then run: cswitch add`.
- Never cancel an in-flight refresh; never keep two copies of one account (import dedupe).

### 8.4 Usage store and poll policy

`cache/usage.json` (schema 2) rows: `email`, `accountId`, `lastGood`, `fetchedAt`,
`lastAttemptAt`, `consecutiveFailures`, `lastError`, `backoffUntil`, `nextPollAt`,
`pollIntervalS`, `last429At`, `authDeadStrikes`, `struckFingerprint`. Rules from
`cswap-model-autoswitch.md` §4.3–4.6 and §5 apply with these constants: SERVE_TTL 180 s,
STALE_OK 300 s, TRUST_MAX 3600 s (7200 s after a 429), backoff `30·2^(n-1)` cap 600 s,
Retry-After honored (+900 s margin above 600 s, cap 4500 s), poll plan defaults
180 s (active) / 300 s (candidate), ceilings 300 / 600 s, urgent 60 s within 15 pt of the
threshold, at-limit 600 s capped at reset + 60 s, jitter 10 %. The v0.1 store omits the
fenced `claimId` lease and uses `lastAttemptAt` with a 90 s claim window.

Every `list`, `status`, `switch --strategy`, and TUI tick takes the same on-demand pass:
rows older than 180 s that are due (or have no plan) are fetched, at most one inactive
candidate per pass plus the active account, with a stagger of 250 ms between requests.

## 9. Auto-switch (`auto`)

Settings, flags, clamps, exit codes (`0` switched, `1` error, `2` no action, `3` blocked),
JSONL event kinds and fields, `no-switch` reasons, human lines, loop banner and signal
handling are those of cswap (`cswap-cli-contract.md` §10, `cswap-model-autoswitch.md` §6).

v0.1 engine (a faithful subset of cswap's):

1. Read state (release quarantines whose credential fingerprint changed or whose slot
   changed identity). Active slot from the live file; none → `no-active-account` /
   `unmanaged-active-account`.
2. Collect usage: active account plus one due candidate; escalate to all candidates when
   the active headroom is unknown or within 15 pt of the threshold.
3. Classify: headroom known → below threshold (`no-switch below-threshold`, or trigger
   `consume-first` under that strategy) / `proactive` / `at-limit`; unknown → after
   `unhealthyTicks` consecutive unknowns → `failover`; a `token expired` sentinel on the
   active account holds (`active-idle`) up to 30 min.
4. Cooldown applies to `proactive` and `consume-first` only.
5. Candidates = switchable, enabled, not quarantined, not the active slot; API-key accounts
   only with `includeApiKeyAccounts`, as a last resort.
6. Ranking: `best` — proactive requires landing below the threshold and beating the active
   by `hysteresisPct`; at-limit / failover only require headroom > 0; order by headroom.
   `consume-first` — prefer the candidate whose weekly window resets soonest and has room.
   All-exhausted → `all-exhausted` event, sleep toward the earliest reset (+60 s, cap 600 s).
7. Freshen the chosen target (refresh when the token expires within 10 min); a terminal
   refresh failure quarantines it (`account-quarantined`, reason `invalid_grant`) and the
   next candidate is tried.
8. Perform: re-check cooldown under the state lock, `switch_to`, record
   `lastSwitchAt/To/From`, emit `switch`. Dry-run emits `switch` with `dryRun: true` and
   writes nothing.

Omitted from v0.1 (documented as such): the no-return anti-flap bar, the
all-above-threshold recovery-horizon escape, identity-conflict quarantine,
`config-warning` model typo guard, and the `soonest-reset` (Go-only) strategy.

## 10. Session mode (`run`, `env`, `map`, `unmap`)

- Profile dir `sessions/<n>-<slug(email)>/` (slug per cswap: NFC, keep ASCII alnum `._-`,
  else `_`). Bootstrap/reuse each launch: write `auth.json` from the slot (refreshing an
  expiring token first, best-effort), copy `$CODEX_HOME/config.toml` (whole file; a
  missing file yields none), link `AGENTS.md`, `prompts/`, `skills/` unless `--no-share`,
  link `sessions/` and `history.jsonl` with `--share-history` (POSIX only). Manifest
  `.cswitch-shared.json` `{"items":[…],"mode":"symlink"|"copy"}` prunes items no longer
  shared.
- Launch: `Launching Account-<n> (<email>) [session mode]`, env `CODEX_HOME=<profile>`,
  scrub `OPENAI_API_KEY` / `CODEX_API_KEY` (`Ignoring … for this session — it would
  override the selected account inside Codex.`), argv `codex [--no-daemon] <tail>`; POSIX
  `exec`, Windows child + exit-code mirror.
- Same-account fast path: target is the active login and `CODEX_HOME` not preset → exec
  plain `codex <tail>`.
- API-key accounts are accepted in session mode (their `auth.json` is a plain API key).
- After the child exits, tokens Codex rotated inside the profile are folded back into the
  slot (freshness rule) so the stored credential stays alive.
- `map` / `unmap` / nearest-ancestor lookup per cswap; mappings key on identity.

## 11. Export / import

Envelope `.cswitch` (JSON, indent 2):

```json
{"version": 1, "exportedAt": "…Z", "exportedFrom": "macos", "cswitchVersion": "0.1.0",
 "swapVersion": "0.1.0", "encrypted": false, "activeAccountNumber": 1,
 "accounts": [{"number": 1, "email": "…", "uuid": "…", "organizationUuid": "…",
               "organizationName": "…", "planType": "plus", "added": "…Z",
               "credentials": {…auth.json…}, "kind": "api_key", "alias": "dev"}]}
```

`credentials` is the auth.json object for both kinds (an API key is the object Codex
writes, not a bare string). Validation, identity matching on `(email, organizationUuid)`,
`--force`, dead-token auto-replace, `--account`, slot reuse, messages and the `Done:`
summary follow cswap §13; `config` entries are ignored on import.

## 12. TUI

ratatui + crossterm. Screens, layout, bars (`━ ╸ ─ ┃`, width 12–30, severity green < 70 /
amber ≥ 70 / red ≥ 90, dim when age > 300 s), key bindings, menus, modals, toasts, and the
two-lane 3 s refresh follow `docs/research/cswap-tui.md` with these mappings:

- rows `5h`, `7d`, model pools by name, `credits` (no `$$`);
- add menu: `From current Codex login` / `From an API key…`; the token modal asks for an
  OpenAI API key;
- sentinel labels per §8.3 and `API key (no quota)`;
- confirm texts say `Codex` / `cswitch auto`;
- theme `auto` resolves to dark unless `COLORFGBG` reports a light background (no OSC
  probe in v0.1); `ctrl+t` cycles; persisted to `ui.theme`.

Actions run on a blocking thread and return structured results (no ANSI capture): the
Output modal shows the command's human lines.

## 13. Errors and exit codes

`CswitchError` variants and JSON `type` strings: `ConfigError`, `SwitchError`,
`SessionError`, `LockError`, `AccountNotFoundError`, `ValidationError`, `TransferError`,
`CredentialReadError`, `CredentialWriteError`. Exit 1 for any of them (`Error: <msg>` on
stderr, or the JSON envelope on stdout in `--json` mode); 2 for usage errors (clap, and the
cswap cross-flag messages); 130 on Ctrl-C (`Operation cancelled` / `Auto-switch stopped`);
`auto --once` 0/1/2/3; `run` propagates the child's status. Root guard: refuse to run as
root outside a container.

## 14. Architecture

```
src/main.rs            → cli::run()
src/cli/               clap definitions, legacy-flag translation, cross-flag checks, dispatch,
                       output mode (human / json), exit codes; one file per command group
src/errors.rs          CswitchError
src/paths.rs           CSWITCH_HOME, CODEX_HOME, live auth path, credential-store gate
src/fsutil.rs          atomic writes, permissions, JSON read/write, file locks
src/codex/auth.rs      auth.json model, backups, identity, freshness rule
src/codex/jwt.rs       claim extraction, plan labels, expiry
src/codex/usage.rs     usage API client + normalization
src/codex/oauth.rs     token refresh
src/codex/app_server.rs daemon probe/restart, --no-daemon probe
src/store/             roster, credentials, settings, mappings, usage_store (+poll policy),
                       autoswitch state; Store = paths + lock
src/switcher.rs        account lifecycle + switch + resolve + snapshots (façade)
src/collect.rs         on-demand usage pass (which rows, refresh, record)
src/autoswitch.rs      engine, events, loop
src/session.rs         run/env profiles + mappings glue
src/transfer.rs        export/import
src/printer.rs         colors, formatting helpers (age, countdown, clock)
src/jsonout.rs         payload builders
src/tui/               app, screens, widgets, modals, theme
```

Testing: unit tests beside each module (pure functions: resolution, ranking, poll plan,
normalization, argv translation, formatting); integration tests under `tests/` drive the
binary with `CSWITCH_HOME`/`CODEX_HOME` pointed at temp dirs, a fake `codex` on `PATH`
(records argv; answers `--help`, `app-server daemon version|restart`), and a local axum
mock of the usage and token endpoints (`CSWITCH_USAGE_URL`, `CSWITCH_TOKEN_URL`
overrides). Quality gate: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test --all`.

## 15. Assumptions recorded for the owner

1. Repository `newbdez33/cswitch` is created **private**; flip to public when ready.
2. Store location `~/.cswitch` on all platforms (`CSWITCH_HOME` override), not cswap's
   XDG split.
3. `add-token` registers API keys only; ChatGPT logins are captured with `add` after
   `codex login`. A browser/device login flow inside cswitch is a later addition.
4. The TUI theme `auto` does not probe the terminal in v0.1.
5. `--json` keeps cswap's field names (`organizationUuid` = Codex account id) so existing
   scripts keep working; Codex-specific fields are additive.
