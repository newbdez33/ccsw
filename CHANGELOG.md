# Changelog

All notable changes to `ccsw` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## v0.9.0 — 2026-10-11

### Added

- `ccsw serve`: an embedded remote console for account quota, details, usage
  refresh, and guarded Codex or Claude switches. It supports loopback, a
  verified local Tailscale address, or an explicit HTTPS origin behind
  Tailscale Serve. One-use pairing, expiring browser sessions, read-only mode,
  request deduplication, and live snapshots protect remote actions.
- A responsive bitmap interface with provider filters, search, account details,
  activity, offline states, and explicit partial switch results. Hover fills
  complete in about 267 ms and respect reduced motion.
- Switchable Bitmap and Paper console themes. Paper uses local Inter fonts,
  solid quota bars with hatched tracks, borderless rounded panels, rounded
  line icons, and yellow highlights. Both themes support light and dark colors,
  follow the system by default, and remember manual choices.
- Usage values at or above 90% turn red in console cards, account rows, and
  details, matching the terminal's critical threshold.
- A Remote Console TUI menu to start or stop the server, select local or
  Tailscale access and read-only mode, and open a browser with a fresh pairing
  link. Links pair automatically and clear the code from the address bar.
  The service stays active on Back and stops when the TUI exits.

### Changed

- Shortened the README to a product overview, screenshots, and installation.
  Separate guides cover account usage, the remote console, session profiles,
  transfers, upgrades, and development. Release archives include the guides
  and screenshots.

### Fixed

- Codex reset cards now fetch the earliest expiry from the reset credit list
  when the usage response reports only a count, so the `(in 10d)` hint appears
  after the next usage fetch. A failed list request keeps the usage result
  and does not trigger a token refresh.

## v0.8.3 — 2026-10-10

### Fixed

- `import --from-cswap` now counts the claude-swap accounts it could not read
  (roster entries with no `.enc` file, no Keychain item, or no email) in the
  report's `skipped`, so `--json` consumers and the `Done: … skipped` line
  account for every account the store listed. They were already announced on
  stderr as `Skipping Account-N …`, but the counter stayed at zero and a
  `--retire` run could rename the store with no machine-readable trace that an
  account had been left behind.

## v0.8.2 — 2026-10-10

### Added

- Codex accounts show when their soonest reset credit expires, the same
  `♥ 3 (in 12d)` hint Claude accounts got in v0.8.1: the earliest `expires_at`
  of the counted `rate_limit_reset_credits.credits[]` entries is kept as
  `reset_credits_end_at` and appears after the account's next fetch.

## v0.8.1 — 2026-10-10

### Added

- The dashboard's reset-card heart says when the soonest grant behind it ends:
  `♥ 2 (in 10d)` on account cards and mini rows (hours and minutes under a day,
  `expired` once a stale measurement outlives the grant). The earliest `ends_at`
  of the counted `cedar_ember` grants is kept in the usage cache as
  `reset_credits_end_at`; rows cached by earlier releases show the hint after
  their next fetch.

## v0.8.0 — 2026-10-10

### Added

- `ccsw import --from-cswap [DIR] [--retire] [--json]` reads a claude-swap store
  directly — the roster in `sequence.json`, each account's credentials from the
  macOS Keychain (service `claude-swap`) or its base64 `.enc` file, and the
  `.claude.json` snapshot — and imports it through the regular importer, so
  identity matching, slot numbers and the Codex roster behave as for any export.
  DIR defaults to claude-swap's location on the platform (`~/.claude-swap-backup`;
  `$XDG_DATA_HOME/claude-swap` on Linux). Reads are `.enc`-wins, as in claude-swap.
  `--retire` renames the store to `<dir>.migrated-<stamp>` after a successful run
  (every account it held is in ccsw by then), so a leftover claude-swap finds no
  accounts to refresh. Exit 2 means there was nothing to import (no store, an
  empty roster, or an already-migrated store); `--json` prints the report — also
  for file and stdin imports — for scripts such as the Token Beats desktop app,
  which runs this once on first launch.
- A static `x86_64-unknown-linux-musl` release archive, for hosts whose glibc is
  older than the builder's (WSL distributions).

### Changed

- The Claude Code usage requests identify as `claude-cli/2.1.296` when the local
  Claude Code version cannot be read.

## v0.7.5 — 2026-10-09

### Added

- `ccsw list --fetch-all` measures every stale or due account in one pass instead
  of the active account plus a single candidate, for collectors that poll the
  whole roster on a timer (for example `ccsw list claude --json --fetch-all` every
  ten minutes). Fresh rows are still served from the cache, and backoff,
  Retry-After and dead-token gates still apply, so each account stays inside its
  poll budget. The flag belongs to `list` only.

## v0.7.4 — 2026-10-09

### Added

- The dashboard's accounts panel scrolls when it does not fit above the menu,
  instead of being cut off, and shows a scrollbar while it overflows. The
  switch and watch lists show the same scrollbar. The mouse wheel and
  `PgUp`/`PgDn` scroll all three; the TUI now captures the mouse, as `cswap`
  does, so selecting terminal text takes the terminal's modifier key.

## v0.7.3 — 2026-10-09

### Changed

- Reset cards are drawn as a red heart with a green count (`♥ 2`) instead of
  an all-green spade.
- Account cards and one-line summaries wrap at the terminal width instead of
  being cut off at the right edge, as in `cswap`.

## v0.7.2 — 2026-10-08

### Changed

- The dashboard keeps every account visible when the terminal is short. As in
  `cswap`, the menu takes the rows left under the accounts panel and scrolls
  to keep the highlighted entry in view; unlike `cswap` it never shrinks below
  its title and the highlighted row, so Enter never acts on an entry that is
  off screen.

## v0.7.1 — 2026-10-08

### Added

- Claude Code accounts show their saved limit resets (`/limit-reset` grants) as
  the same green `♠ n` the dashboard uses for Codex reset credits. Claude usage
  requests ask for the `cedar_ember` block and count the grants that are not
  paused, have started and have not ended.

### Changed

- Claude usage requests identify as the installed Claude Code
  (`claude-cli/<version> (external, cli) ccsw/<version>`), the only client
  Anthropic lists the grants for. The version is read from the `claude`
  executable's install layout (native installer, Homebrew cask or npm) without
  starting it; `FALLBACK_CLI_VERSION` in `src/claude/usage.rs` stands in
  otherwise and is bumped at every release.

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

## v0.6.0 — 2026-10-08

### Added

- Claude Code session profiles for `run` and `env`, adapted from cswap. Profiles
  use `CLAUDE_CONFIG_DIR`, isolated Keychain items, shared customizations and
  optional shared history. Keep local files and merge existing history before linking.
- Provider selectors for session commands and independent directory mappings for
  each provider. Existing mapping files remain readable.
- `run --require-session` refuses the plain default-login fast path.
- Capture newer profile credentials before use, protect running profiles and
  duplicate token copies, and refuse writes when ownership cannot be checked.

### Changed

- `env --unset` clears both provider homes; add a provider selector to clear one.
- `unmap DIR` removes both provider mappings; `unmap DIR PROVIDER` removes one.
- Include the upstream session source reference and full MIT license in NOTICE.

### Fixed

- Resolve the live OAuth Keychain service from the exact configured directory,
  including Unicode normalization and secure-storage overrides. Custom profiles
  do not read or change the default managed API-key item.
- Check the file credential-store setting only for commands that use Codex.
- Limit sharing manifest cleanup to known shared item names.
- Reserve session profiles before launch and coordinate token consumption with
  in-flight refreshes, including duplicate credentials in other slots.
- Invalidate stale profiles after a new login, preflight both slots before an
  account move, and remove managed profile Keychain items during a safe purge.

## v0.5.0 — 2026-10-08

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

- A `keychain unavailable` active account stays held (`no-switch active-idle`) until
  its login is readable. It never counts toward failover, including after 30 minutes.
- `autoswitch.model` names that no account's usage windows report produce one
  `config-warning` event per run.

### Fixed

- Protect the live refresh token even when another slot saves it under a different
  identity. Skip every refresh while the live credential is unreadable.
- Keep other providers' quarantine records unchanged during automatic recovery.

## v0.4.0 — 2026-10-08

### Changed

- Use `ccsw` as the project, crate, binary, and release archive name.
- Use `~/.ccsw` for the store, `ccsw.log` for logs, and `.ccsw-shared.json` for
  session sharing manifests. Existing installations require a manual move.
- Use the `CCSW_*` prefix for environment variables.
- Use `.ccsw` for export file examples and `ccswVersion` in export metadata.
  Existing export files can still be imported.
- Move the repository to `https://github.com/newbdez33/ccsw`.

## v0.3.0 — 2026-10-08

### Added

- Claude Code accounts share the roster and the global slot numbers with Codex accounts.
  `ccsw add` captures the current Codex and Claude Code logins; `ccsw add claude`
  and `ccsw add codex` capture one.
- `add-token` recognises Anthropic setup-tokens (`sk-ant-oat…`) and API keys
  (`sk-ant-api…`).
- `ccsw switch <slot>` works across providers; `ccsw switch claude` and
  `ccsw switch codex` rotate within one provider.
- `list` prints a Codex block and a Claude block, and `status` reports the active
  account of each provider.
- The dashboard, switch and watch screens show a `codex` section and a `claude` section,
  and Claude cards add a `$$` extra-usage spend row.
- The outgoing Claude login is backed up under `backups/claude/` before a switch.
- `CCSW_KEYCHAIN=off` limits Claude credential access to the file backend.

### Changed

- `--json` output is `schemaVersion: 2`: rows and switch references carry `provider`,
  and `active` is a per-provider map on `list` and `status`.
- `codex` and `claude` are reserved alias names. An existing alias named `codex` or
  `claude` can no longer be reached with `ccsw switch <alias>`, because those words
  now select a provider; switch to that account by its slot number or email instead.
- A bare `ccsw add` captures the Claude Code login as well as the Codex one. Once the
  store holds accounts of both providers, a bare `ccsw switch` needs a `codex` or
  `claude` selector.
- `sequence.json` records carry `provider`, and the roster records `activeByProvider`.

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
- Settings in `settings.json` via `ccsw config list|get|set|unset|path`.
- State (`lastSwitchAt/To/From`, quarantine) in `autoswitch_state.json`.

### Session mode

- `run <account> [-- codex args]`, `env <account> [--shell sh|fish|pwsh] [--unset]`,
  `map`, `unmap`: a private `CODEX_HOME` per account under `sessions/`, `config.toml`
  copied, `AGENTS.md`, `prompts/` and `skills/` shared, optional `--share-history`.
  Codex is launched with `--no-daemon` when the installed version supports it.

### Export / import

- `.ccsw` envelope (`export <path>|-`, `--account`, `--full` accepted), `import`
  with identity matching, `--force`, dead-token auto-replace and the `Done:` summary.

### TUI

- `ccsw` / `ccsw tui` / `ccsw watch`: dashboard, switch, watch and auto
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
  `POST https://auth.openai.com/oauth/token`; `CCSW_USAGE_URL` and
  `CCSW_TOKEN_URL` override them for tests.
- Rotating log `ccsw.log` (1 MiB, three backups) in the backup root, created on
  the first record; `--debug` mirrors records to stderr.

### Deviations from cswap in v0.1

Documented in `docs/specs/2026-09-29-ccsw-design.md` (§2, §9):

- `menubar` exits 1 (`The menu bar is not available in ccsw.`); `unclaimed` is not
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
- The store lives in `~/.ccsw` on every platform (`CCSW_HOME` override), not in
  cswap's XDG split.
