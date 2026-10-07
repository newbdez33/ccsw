# Changelog

All notable changes to `cswitch` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## v0.2.0 — 2026-10-07

### Added

- Add **Add new account** to the dashboard's add-account menu. It opens browser
  sign-in through the Codex CLI, then saves and activates the account.
- Isolate browser login in a temporary Codex home to preserve saved credentials.
  Show the login URL as a fallback and support cancellation and a ten-minute timeout.
- The TUI shows the account's rate-limit reset cards as a bold green `♠ <n>` at the
  end of the account's header line: on the full card (dashboard, switch, watch and
  auto screens) after the active marker and age, on the dashboard minis after the
  usage summary; nothing is shown at zero. The count comes from the usage API's
  `rate_limit_reset_credits.available_count` (or, without a count, the number of
  available Codex entries in its `credits[]`) and is cached as `reset_credits` in
  `cache/usage.json`. `list` and `--json` do not carry it yet.

## v0.1.0 — 2026-09-29

First release: the `cswap` (claude-swap 0.25.0) interface re-targeted at the OpenAI
Codex CLI, with the Codex mechanics ported from codex-switch. Single static binary for
macOS (Apple silicon and Intel), Linux (x86_64) and Windows (x86_64).

### Commands

- `list` / `ls` (with `--json`, `--token-status`), `status`, `switch` (rotation,
  `switch <num|email|alias>`, `--force`, `--strategy best|next-available`, `--model`),
  `add` (`--slot`, `--alias`), `add-token`, `remove` / `rm`, `disable`, `enable`,
  `alias`, `move`, `swap`, `purge`, `help`, `--version`.
- The legacy flag spellings (`--list`, `--switch-to 2`, `--add-account`, …) keep
  working; verbs are translated to them before parsing.
- `--json` prints exactly one document on stdout and nothing on stderr; handled errors
  become the `{"schemaVersion":1,"error":{"type","message"}}` envelope (exit 1).
- Exit codes: 0 success and no-op switches, 1 handled errors, 2 usage errors,
  130 on Ctrl-C.

### Usage and token refresh

- Every `list`, `status`, `switch --strategy` and dashboard tick runs the same
  on-demand pass: the active account plus one due candidate per pass, results cached
  in `cache/usage.json` for 180 s, backoff on failures, `Retry-After` honored.
- Rows report `5h`, `7d`, one row per model pool (`additional_rate_limits`, weekly
  window, `(!)` at 100 %) and `credits: $<balance>`; JSON rows carry `usage.fiveHour`,
  `usage.sevenDay` (with pace fields), `usage.scoped[]`, `usage.credits`,
  `usageFetchedAt` and `usageAgeSeconds`.
- 401/403 triggers one refresh and one retry; rotations are persisted at once with
  compare-and-swap on the presented refresh token into the slot file and, for the
  active account, into the live `auth.json`. Terminal verdicts mark the account
  `relogin_required` until a newer credential replaces it.

### Auto-switch

- `auto` (foreground loop) and `auto --once` (exit 0 switched, 1 error, 2 no action,
  3 blocked), `--json` JSONL events (`poll`, `switch`, `no-switch`, `all-exhausted`,
  `account-quarantined`, `sleep`, `error`), `--threshold`, `--interval`, `--cooldown`,
  `--model`, `--include-api-key-accounts`, `--strategy best|consume-first`,
  `--dry-run`, `--debug`.
- Settings in `settings.json` via `cswitch config list|get|set|unset|path`.
- State (`lastSwitchAt/To/From`, quarantine) in `autoswitch_state.json`.

### Session mode

- `run <account> [-- codex args]`, `env <account> [--shell sh|fish|pwsh] [--unset]`,
  `map`, `unmap`: a private `CODEX_HOME` per account under `sessions/`, `config.toml`
  copied, `AGENTS.md`, `prompts/` and `skills/` shared, optional `--share-history`.
  Codex is launched with `--no-daemon` when the installed version supports it.

### Export / import

- `.cswitch` envelope (`export <path>|-`, `--account`, `--full` accepted), `import`
  with identity matching, `--force`, dead-token auto-replace and the `Done:` summary.

### TUI

- `cswitch` / `cswitch tui` / `cswitch watch`: dashboard, switch, watch and auto
  screens, modals, toasts, dark and light themes (`ctrl+t`, persisted as `ui.theme`).

### Codex mechanics

- Live login is `$CODEX_HOME/auth.json`; the credential store must be `file`
  (`cli_auth_credentials_store` absent or `"file"` in `config.toml`).
- Identity is `(email, chatgpt_account_id)` from the id_token; the display tag is the
  workspace name, else the plan label, else `personal`.
- A switch folds a newer live credential back into its slot, backs the live file up
  as `auth.json.bak.<nanos>` (three kept), writes the target atomically, and restarts
  the Codex app-server daemon when one is running.
- Usage from `GET https://chatgpt.com/backend-api/wham/usage`, refresh through
  `POST https://auth.openai.com/oauth/token`; `CSWITCH_USAGE_URL` and
  `CSWITCH_TOKEN_URL` override them for tests.
- Rotating log `cswitch.log` (1 MiB, three backups) in the backup root, created on
  the first record; `--debug` mirrors records to stderr.

### Deviations from cswap in v0.1

Documented in `docs/specs/2026-09-29-cswitch-design.md` (§2, §9):

- `menubar` exits 1 (`The menu bar is not available in cswitch.`); `unclaimed` is not
  provided (no forensic stash of foreign credentials); `upgrade` / `update` print
  reinstall guidance only.
- JSON `usage.credits` (`{"balance","unlimited"}`) replaces cswap's `spend`; the
  human `$$` spend row does not exist.
- `add-token` registers OpenAI API keys only; there is no setup-token kind. ChatGPT
  logins are captured with `add` after `codex login`.
- The TUI theme `auto` follows `COLORFGBG` only (no OSC terminal probe).
- `run` folds tokens rotated inside the session profile back into the slot when the
  child exits (POSIX `exec` defers this to the next launch); `--share-history` is
  POSIX only.
- `export --full` is accepted without effect (Codex has no per-account config).
- Auto-switch omits the no-return anti-flap bar, the all-above-threshold recovery
  escape, identity-conflict quarantine, the `config-warning` model typo guard and the
  `soonest-reset` strategy.
- No macOS Keychain, `~/.claude.json`, Claude Code lock protocol, process detection
  (`Running instances:`) or passive update notice.
- The store lives in `~/.cswitch` on every platform (`CSWITCH_HOME` override), not in
  cswap's XDG split.
