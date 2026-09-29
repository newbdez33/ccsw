# cswap CLI contract — spec input for the `cswitch` Rust port

Sources read (all verbatim strings below come from these):

- Python reference (v0.25.0): `/Users/jacky/projects/dev/claude-swap/src/claude_swap/cli.py`, `json_output.py`, `printer.py`, `settings.py`, `mappings.py`, `exceptions.py`, `models.py`, `autoswitch.py` (events), `switcher.py` (output strings), `session.py`, `transfer.py`, `oauth.py`, plus `README.md`.
- Go contract docs: `scratchpad/cswap-go-docs/reference.md` (per-command man page), `08-cli-infra.md` (dispatcher/flags/settings/json_output/printer), `02-switcher-switch-list.md` (switch/list/status internals), plus targeted reads of `05-autoswitch.md` (§3 events, §19 CLI wiring), `06-session-mappings.md` (§1, §5), `07-transfer-migrations.md` (§1–4).

Where the Go doc and the Python source disagree, both are listed and the Python (v0.25.0) value is marked **[py]**; the Go-only value is marked **[go]**. See §16 for the full list of discrepancies.

Conventions in this file: `<n>` = slot number, `<email>` = recorded email, `<tag>` = organization name or the literal `personal`. Colored/dim/bold styling is noted as `accent(...)`, `dimmed(...)`, `muted(...)`, `bolded(...)`, `bold_accent(...)`, `yellowed(...)`; plain text is emitted when colors are off.

---

## 1. Invocation model and dispatch order

`main()` runs, in this order:

1. Force UTF-8 on stdout/stderr (`errors="replace"`).
2. Best-effort native TLS trust injection (Python-only; Rust's TLS stack uses the OS store natively — omit).
3. Theme probe: reads `ui.theme` from settings.json; **never probes the terminal (OSC query) when the first argv token is `run` or when `--json` is present**, so JSON stdout stays clean and an exec'd child is not confused.
4. **Pre-dispatch on the first argv token** (each handler builds its own parser and returns): `run`, `auto`, `config`, `map`, `unmap`, `unclaimed`, `alias`, `swap`, `move`. These verbs **must be the first argument**: `cswap --debug run 2` is not accepted; use `cswap run 2 --debug`.
5. **Bare invocation gate**: if argv is empty AND stdout is a TTY AND stdin is a TTY → argv becomes `["--tui"]` (opens the dashboard). Non-TTY bare invocation falls through to the "no command given" usage error (exit 2).
6. Verb → legacy-flag translation (§1.1).
7. Main parser: parse, cross-flag validation (§2.2), dispatch (§1.4), single JSON serialization, passive update notice (§4.5).

### 1.1 Verb → legacy flag translation (`_translate_subcommand`)

Fires only when the first token is a recognized verb (verbs never start with `-`). Tokens after the verb pass through verbatim so `--json`, `--strategy`, `--slot`, `--force`, ... keep combining.

| verb | rewrites to | | verb | rewrites to |
|---|---|-|---|---|
| `help` | `--help` | | `disable` | `--disable-account` |
| `list` | `--list` | | `enable` | `--enable-account` |
| `ls` | `--list` | | `export` | `--export` |
| `status` | `--status` | | `import` | `--import` |
| `add` | `--add-account` | | `purge` | `--purge` |
| `add-token` | `--add-token` | | `upgrade` | `--upgrade` |
| `remove` | `--remove-account` | | `update` | `--upgrade` |
| `rm` | `--remove-account` | | `tui` | `--tui` |
| | | | `watch` | `--watch` |
| | | | `menubar` | `--menubar` |

`switch` is special-cased:
- `switch <X>` where `X` does **not** start with `-` → `["--switch-to", X, ...rest]`
- `switch` alone, or `switch -<flag> ...` → `["--switch", ...rest]`

Unit behaviors (from tests): `[]`→`[]`; `["--list"]` unchanged; `["switch"]`→`["--switch"]`; `["switch","--strategy","best"]`→`["--switch","--strategy","best"]`; `["switch","2"]`→`["--switch-to","2"]`; `["switch","u@x.com","--json"]`→`["--switch-to","u@x.com","--json"]`; `["ls"]`→`["--list"]`; `["rm","2"]`→`["--remove-account","2"]`; `["update"]`→`["--upgrade"]`; `["export","b.cswap","--full"]`→`["--export","b.cswap","--full"]`; `["bogus"]` unchanged (parser then rejects: `unrecognized arguments: bogus`, exit 2).

Aliases summary line printed in help: `Aliases: ls=list  rm=remove  update=upgrade`.

### 1.2 Legacy `--flag` interface (hidden from `--help`, still fully supported)

All members of one mutually-exclusive group (`required=False`); combining two → argparse error `argument X: not allowed with argument Y` (exit 2).

| flag | kind | metavar |
|---|---|---|
| `--add-account` | store_true | — |
| `--remove-account` | value | `NUM\|EMAIL` |
| `--disable-account` | value | `NUM\|EMAIL` |
| `--enable-account` | value | `NUM\|EMAIL` |
| `--list` | store_true | — |
| `--switch` | store_true | — |
| `--switch-to` | value | `NUM\|EMAIL` |
| `--status` | store_true | — |
| `--purge` | store_true | — |
| `--export` | value | `PATH` |
| `--import` | value | `PATH` |
| `--tui` | store_true | — |
| `--watch` | store_true | — |
| `--menubar` | store_true | — |
| `--upgrade` | store_true | — |
| `--add-token` | optional value (`nargs="?"`, `const=""`) | `TOKEN\|-` |

`--add-token` with no value stores the empty string `""` (distinct from "not given"); downstream checks use "is not None" to mean add-token was selected. Model as `Option<String>` where `Some("")` = prompt interactively.

### 1.3 Program name

Usage/help shows the basename of argv[0] with `.exe`/`.pyw`/`.py` stripped; falls back to `cswap` when empty or one of `__main__`, `python`, `python3`, `py`. For the port: binary basename with `.exe` stripped, fallback `cswitch`.

### 1.4 Main-parser dispatch table

Before anything else: `--upgrade` runs **before the switcher is constructed** (never touches config/keychain); exit with its return code; Ctrl-C → `\nUpgrade cancelled` (dimmed), exit 130.

Then construct the switcher (`debug=args.debug`), run the **root guard** (POSIX only: euid 0 and not in a container → stderr `Error: Do not run this script as root (unless running in a container)`, exit 1), then:

| selected | action | returns JSON payload? |
|---|---|---|
| `--add-account` | `add_account(slot, alias)` | no |
| `--add-token` | `add_account_from_token(token, email, slot)` | no |
| `--remove-account X` | `remove_account(X)` | no |
| `--disable-account X` | `set_account_disabled(X, True)` | no |
| `--enable-account X` | `set_account_disabled(X, False)` | no |
| `--list` | `list_accounts(show_token_status, json_output)` | yes |
| `--switch` | model resolution (§7.3) then `switch(strategy, json_output, models, model_source)` | yes |
| `--switch-to X` | `switch_to(X, json_output, force)` | yes |
| `--status` | `status(json_output)` | yes |
| `--purge` | `purge()` | no |
| `--export P` | `export_accounts(P, account, full)` | no |
| `--import P` | `import_accounts(P, force)` | no |
| `--tui` | `tui.run(switcher)` → exit with its code | no |
| `--watch` | `tui.run(switcher, start="watch")` | no |
| `--menubar` | macOS-only menubar app | no |

Container detection (`_is_running_in_container`): env `CONTAINER` or `container` non-empty, `/.dockerenv` exists, `docker|lxc|containerd|kubepods` in `/proc/1/cgroup`, or `/proc/self/mountinfo` hints.

---

## 2. Global options and cross-flag validation

### 2.1 Visible options of the main parser (outside the exclusive group)

| flag | type | metavar | default | valid with |
|---|---|---|---|---|
| `--version` | version action | — | — | prints `<prog> <version>`, exit 0 |
| `-h`/`--help` | help | — | — | prints full help, exit 0 |
| `--debug` | flag | — | off | every command (each pre-dispatched verb also accepts its own `--debug`) |
| `--token-status` | flag | — | off | `list` only |
| `--json` | flag | — | off | `list`, `status`, `switch`, `switch <id>` (also `config list/get` and `auto` via their own parsers) |
| `--strategy` | choice `best` \| `next-available` | `{best,next-available}` | unset | bare `switch` only |
| `--model` | string (comma-separated display names, or `all`) | `NAMES` | unset | `switch --strategy ...` only |
| `--slot` | int | `NUM` | unset | `add`, `add-token` |
| `--email` | string | `EMAIL` | unset | `add-token` |
| `--account` | string | `NUM\|EMAIL` | unset | `export` |
| `--alias` | string | `NAME` | unset | `add` |
| `--force` | flag | — | off | `import`, `switch <id>` |
| `--full` | flag | — | off | `export` |

Help text for these (verbatim):
- `--debug`: `Enable debug logging`
- `--token-status`: `Show source-labelled OAuth token diagnostics (use with 'list')`
- `--json`: `Emit machine-readable JSON to stdout (use with 'list', 'status', or 'switch'). See README 'JSON output for scripting'.`
- `--strategy`: `With bare 'switch': pick the target by remaining 5h/7d quota. 'best' jumps to the account with the most headroom; 'next-available' rotates to the next account, skipping any at their limit`
- `--model`: `With 'switch --strategy': also count these models' per-model weekly limits when comparing accounts (comma-separated display names, or 'all'). Defaults to the autoswitch.model setting`
- `--slot`: `Specify slot number when adding account (use with 'add' or 'add-token')`
- `--email`: `Email address for the account. Optional with 'add-token'; defaults to setup-token-{slot}@token.local (or api-key-{slot}@token.local for API keys) since these tokens carry no real email metadata.`
- `--account`: `Limit export to one account (use with 'export')`
- `--alias`: `Set a short display alias for the account (use with 'add')`
- `--force`: `Overwrite existing accounts during import; with 'switch <num|email>', activate the stored credentials without backing up the current login first`
- `--full`: `Include full ~/.claude.json in export (default: oauthAccount only)`

### 2.2 Cross-flag validation (checked in this order, all exit 2 via `parser.error`, message on stderr prefixed by usage line + `<prog>: error: `)

1. No command selected → `no command given — try '<prog> help'` (must NOT mention legacy flags or "one of the arguments ... is required").
2. `--token-status` without `--list` → `--token-status can only be used with 'list'`
3. `--json` without list/status/switch/switch-to → `--json can only be used with 'list', 'status', or 'switch'`
4. `--json` with `--token-status` → `--token-status cannot be combined with --json`
5. `--strategy` without bare `--switch` → `--strategy can only be used with bare 'switch'`
6. `--model` without `--strategy` → `--model can only be used with 'switch --strategy best' or 'switch --strategy next-available'`
7. `--slot` without add/add-token → `--slot can only be used with 'add' or 'add-token'`
8. `--email` without add-token → `--email can only be used with 'add-token'`
9. `--account` without export → `--account can only be used with 'export'`
10. `--alias` without add → `--alias can only be used with 'add'`
11. `--force` without import/switch-to → `--force can only be used with 'import' or 'switch <num|email>'`
12. `--full` without export → `--full can only be used with 'export'`

Notes: `--json` IS accepted with `switch <id>` even though the message names only `'switch'`. Bare `--json` alone hits check 1. `--purge --json` hits check 3. `--strategy bogus` → argparse `argument --strategy: invalid choice: 'bogus' (choose from 'best', 'next-available')`, exit 2.

### 2.3 Help text (verbatim; `cswap` is `%(prog)s`)

Usage line: `usage: cswap <command> [args] [options]`

Description:
```
Multi-Account Switcher for Claude Code

Commands:
  cswap help                       show this help
  cswap list                       list managed accounts
  cswap status                     show current account
  cswap switch                     rotate to the next account
  cswap switch <num|email>         switch to a specific account
  cswap add                        add the current account
  cswap add-token [TOKEN|-]        register a setup-token or API key
  cswap remove <num|email>         remove an account
  cswap disable <num|email>        hold an account out of auto-rotation
  cswap enable <num|email>         return a disabled account to rotation
  cswap run <num|email> [-- ...]   run as an account, this terminal only
  cswap run                        run the current dir's mapped account
  cswap map <num|email> [path]     map a directory to an account
  cswap map                        list directory mappings
  cswap unmap [path]               remove a directory mapping
  cswap alias <num|email> <name>   set a short alias for an account
  cswap alias <num|email> --unset  remove an account's alias
  cswap alias                      list all aliases
  cswap swap <a> <b>               exchange two accounts' slot numbers
  cswap move <a> <slot>            assign an account to a slot (swaps if taken)
  cswap auto                       auto-switch when nearing rate limits
  cswap config [set KEY VALUE]     show or change settings (settings.json)
  cswap unclaimed [--purge ID]     list or drop stashed credential entries
  cswap export <path>              export accounts
  cswap import <path>              import accounts
  cswap tui                        interactive dashboard (also: bare cswap)
  cswap watch                      dashboard, opened on the live watch page
  cswap menubar                    macOS menu bar app
  cswap upgrade                    self-upgrade to latest
  cswap purge                      remove all claude-swap data

Aliases: ls=list  rm=remove  update=upgrade
```

Epilog:
```
Flags combine with subcommands:
  cswap switch --strategy best           # pick the account with most quota left
  cswap switch --strategy next-available # rotate, skipping rate-limited accounts
  cswap switch user@example.com
  cswap list --token-status
  cswap list --json
  cswap add --slot 3                      # add to a specific slot
  cswap add-token sk-ant-oat01-... --email me@example.com
  cswap run 2 -- --resume                 # forward args after '--' to claude
  cswap auto --once                       # single auto-switch tick (cron-friendly)
  cswap config set autoswitch.threshold 80

The original flag spellings (cswap --switch, cswap --list, ...) keep working.
```

The legacy flags are absent from the options section (help=SUPPRESS); the note containing "keep working" is present.

---

## 3. Account resolution: NUM | EMAIL | ALIAS

`_resolve_account_identifier(identifier)` precedence **number → alias → email**:

1. **NUM**: `identifier.isdigit()` → returned unchanged as the slot string (note: `"01"` stays `"01"` here and then fails the record lookup; `move`/`swap` normalize `"01"`→`"1"` separately). Slot numbers are stable, sparse (gaps allowed), never re-packed; `add` allocates `max(existing)+1`.
2. **ALIAS**: case-insensitive equality against each account's stored `alias` (`alias.lower() == identifier.lower()`); an empty alias never matches. Aliases are unique by construction.
3. **EMAIL**: exact, case-sensitive equality with the recorded `email`. **No prefix matching** anywhere. Zero matches → `None` (caller raises `AccountNotFoundError("No account found with identifier: <id>")`). More than one match (same email in two orgs) → `ConfigError`:
   `Email '<id>' is ambiguous — matches accounts: <n> [<OrgName>|personal], <n> [...]. Use account number instead (e.g., cswap --switch-to 1).`

Wrapper `resolve_account(identifier) -> (num, email, orgUuid)` (used by run/env/map/disable/enable): runs the org-field migration first, then raises `AccountNotFoundError("No account found with identifier: <id>")` on no match and `AccountNotFoundError("Account-<n> does not exist")` when the digit slot has no record; ambiguity is a hard `ConfigError` (never a prompt).

Interactive ambiguity handling exists only in `switch_to` and `remove_account` in **human** mode: they print `Multiple accounts found for '<email>':` then `  <n>: <email> [<tag>]` per match, then prompt `Enter account number to switch to: ` (or `Enter account number to remove: `); a non-digit or non-matching answer prints `Cancelled` (dimmed) and returns exit 0. In JSON mode there is no prompt: the ambiguity `ConfigError` becomes the error envelope.

`switch_to` and `remove_account` additionally pre-validate a non-digit, non-alias identifier as an email (`^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$`); failure → `ValidationError("Invalid account identifier: <id>")`.

Account identity everywhere is the composite `(email, organizationUuid)`; the slot number is only a handle.

### 3.1 Alias validity (`normalize_alias`)

Strip, lowercase, then: non-empty (`alias cannot be empty`); not purely numeric (`alias '<name>' cannot be purely numeric (reserved for slot numbers)`); not starting with `-` (`alias '<name>' cannot start with '-' (would be read as a command flag)`); must match `^[a-z0-9_.-]+$` (`alias '<name>' may only contain letters, digits, '-', '_', and '.'`). Violations surface as `ValidationError` (exit 1). Setting an alias already used by another account → `ConfigError("Alias '<alias>' is already used by account <n>")`; from `add --alias` it is a `ValidationError` with the same wording.

---

## 4. Output conventions

### 4.1 stdout vs stderr

- Human notices, prompts, tables and `warning()` (yellow) lines go to **stdout**. `error()` (red) lines go to **stderr**.
- **JSON mode** (`--json` on list/status/switch/switch-to/config list/get): stdout carries **exactly one** 2-space-indented JSON document. Nothing else is printed to stdout (no prompts, no notices, no first-run setup). A handled error is the **error envelope on stdout** (exit 1), nothing on stderr. Ctrl-C note goes to **stderr** in JSON mode (stdout otherwise). Passive update notice is suppressed.
- `auto --json`: one **compact** JSON object per line (JSONL) on stdout; error envelope also compact on stdout.
- `export -`: JSON to stdout with trailing `\n`; every notice/warning (`Skipping Account-...`, `Exported N account(s) ...` is fully suppressed) goes to stderr so stdout is pure JSON.
- `import`: all per-account lines and the `Done:` summary go to **stderr**.
- `config unset` on an unset key: notice on **stderr** (exit 0).
- `env` (Go-only command): only eval-able lines on stdout; every notice on stderr.
- The Ctrl-C cancellation note: `\nOperation cancelled` (dimmed), exit 130; in `auto` it is `\nAuto-switch stopped`; in `upgrade` it is `\nUpgrade cancelled`.

### 4.2 Color handling

Precedence: `NO_COLOR` present (even empty) → off; `FORCE_COLOR` present → on; stdout not a TTY → off; Windows → enable VT processing (on if that succeeds); `TERM=dumb` → off; else on. Computed once per process and cached. Palettes: dark (accent xterm 173, muted xterm 250, red 31, yellow 33) and light (`#954c2a`, `#635d55`, `#ad3128`, `#795911`), selected by `ui.theme` (`auto` follows terminal background detection).

### 4.3 Exit status table

| code | meaning |
|---|---|
| 0 | success; `--version`/`--help`; a switch no-op (`already-active`, `only-one-account`, ...); cancelled `[y/N]` prompts; TUI clean quit; `auto --once` **switched**; `auto` loop stopped by SIGTERM |
| 1 | any handled `ClaudeSwitchError` (`Error: <msg>` on stderr, or JSON envelope on stdout); root guard; `menubar` unavailable; `upgrade` guidance-only; `auto --once` **error**; `tui` when no terminal |
| 2 | usage errors: unknown flag/verb, bad value, missing required positional, exclusive-group violation, every cross-flag validation message, `no command given`; `auto --once` **no action needed** |
| 3 | `auto --once` **blocked** (wanted to switch but no viable target / all exhausted) |
| 130 | SIGINT (Ctrl-C) |
| (child) | `run` on POSIX execs `claude` and exits with its status; on Windows stays resident and mirrors the child's exit code |

### 4.4 Error envelope and error type names

```json
{
  "schemaVersion": 1,
  "error": { "type": "<ExceptionClassName>", "message": "<str(exc)>" }
}
```

`type` is the Python class name; the port must emit the same strings. Hierarchy:

```
ClaudeSwitchError
├── CredentialError
│   ├── CredentialReadError
│   └── CredentialWriteError
├── ConfigError
├── SwitchError
├── SessionError
├── LockError
│   └── ClaudeCodeLockTimeout
├── AccountNotFoundError
├── ValidationError
├── TransferError
├── MigrationError
└── MigrationIncomplete
```

Semantics worth keeping: `ClaudeCodeLockTimeout` = timed out on the CLI tool's own advisory lock, nothing mutated, safe to retry. `MigrationIncomplete` = run-once migration didn't finish; retried next run. `LockError` on the backup-root lock: `Failed to acquire lock - another instance may be running` (10 s timeout, 0.1 s poll).

### 4.5 Passive update notice

After a successful main-parser command, only when NOT `--purge`, NOT `--upgrade`, NOT `--json`: check a 24 h cached latest-version lookup (`<backup_root>/cache/update_check.json`, format `{"timestamp": <epoch>, "data": <latest|null>}`, 2 s network timeout, every failure cached as `null` and swallowed). If newer, print to **stderr**, muted, preceded by a blank line:
`A newer version of claude-swap is available (<latest>). You are using <current>. <hint>` where hint is `Run \`cswap upgrade\` to update.` / `Run \`<direct cmd>\` to update.` (Windows) / `Run \`cswap upgrade\` for upgrade instructions.` (unknown install method). Not printed by pre-dispatched verbs (`run`, `auto`, `config`, `map`, `unmap`, `alias`, `swap`, `move`, `unclaimed`).

### 4.6 Logging

Logger name `claude-swap`; rotating file `<backup_root>/claude-swap.log` (1 MB, 3 backups), created **lazily on first record** (a no-op run must not create the directory). `--debug` adds a stderr handler at DEBUG with format `LEVEL: message`.

---

## 5. `list` / `ls`

**Synopsis**: `cswap list [--json] [--token-status] [--debug]` (legacy `cswap --list`).

**Options**: `--json` (flag; mutually exclusive with `--token-status`), `--token-status` (flag; adds source-labelled OAuth token diagnostics per account), `--debug`.

**Reads**: `sequence.json`, per-account `configs/` + `credentials/`, `cache/usage.json`. May refresh inactive accounts' tokens and write their backups; never writes the live credentials while a Claude Code instance is running.

**Exit**: 0 success; 1 handled error; 2 usage error.

**Human output when no `sequence.json`**: prints `No accounts are managed yet.` (dimmed) then the first-run setup: if no live login → `No active Claude account found. Please log in first.` (dimmed); else prompt `No managed accounts found. Add current account (<email>) to managed list? [Y/n] ` — answer `n` → `Setup cancelled. You can run 'cswap --add-account' later.`; anything else runs `add_account()`. In JSON mode this **never prompts**: payload `{"schemaVersion":1,"activeAccountNumber":null,"accounts":[]}`.

**Human output format**:
```
Accounts:
  1: dev (test@example.com) [personal] (active)
     ├ 5h:  10%   resets 20:39         in 1h 30m
     └ 7d:  50%   resets Jul 5 08:59   in 1d 19h
     • active profile: fresh, refresh token yes, expires 21:39 in 1h 0m     # only with --token-status

  2: account2@example.com [Acme Corp] (disabled)
     no credentials
```
- Header `Accounts:` (bolded).
- Row: `  <n>: <label> [<tag>]<markers>` where label = `accent(alias) (email)` if alias set, else `email`; tag = org name or `personal` (muted, in brackets); markers: ` (active)` (bold_accent) when active, then ` (disabled)` (muted) when disabled.
- Usage lines (`_usage_entry_lines`), each indented 5 spaces, tree-connected with `├`/`└` (dimmed) and the text muted:
  - Sentinel state → first line dimmed `SENTINEL_NOTES[sentinel]`:
    - `token expired` → **[py]** `token expired — refresh deferred this pass; retries automatically` (**[go]** older text: `token expired — Claude Code refreshes the active account`)
    - `foreign credential` → `live credential belongs to another account — a switch repairs it` **[py]**
    - `api key` → `API key (no quota)`
    - `keychain unavailable` → `keychain unavailable — locked or in use; try again`
    - `re-login needed` → `re-login needed — refresh token dead; log in with Claude Code, then run: cswap add`
    - `no credentials` → rendered raw as `no credentials`
    If a last-good measurement exists (and sentinel ≠ api key), append `└ last seen <100-headroom>% used · <age>`.
  - Measurement → `_format_usage_lines` rows in order spend (`$$`), `5h`, `7d`, then each scoped per-model window by name; labels padded to widest+`:`; body `{pct:>3.0f}%   resets {clock:<12}  in {countdown}` (scoped rows append `  (!)` when pct ≥ 100; spend row is `{pct:>3.0f}%   resets {clock:<12}  ${used:,.2f} / ${limit:,.2f}`); without a reset cell just `{pct:>3.0f}%`. If served data is older than 180 s, the last line gets ` · <age>` (`just now` / `Nm ago` / `Nh ago` / `Nd ago`).
  - Neither → `usage unavailable` (dimmed), plus ` (<last_error>)` such as `http-429`, `http-401`, `timeout`.
- Blank line between accounts (not after the last).
- Then duplicate-account and lockstep-usage warnings via `warning()` (yellow, stdout) after a blank line.
- Then, if any Claude Code processes are detected, a `Running instances:` block (bolded): `  ● <label>   <cwd>  (<N> session[s], IDE)` grouped by (entrypoint label, abbreviated cwd). Labels: `cli`→`CLI`, `claude-vscode`→`VS Code`, `claude-desktop`→`Desktop`, `sdk-*`→`SDK`, `mcp`→`MCP`, `local-agent`→`Agent`, `remote`→`Remote`; IDE `Visual Studio Code`→`VS Code`. **Claude-specific**, see §15.
- `--token-status` adds after the usage lines one `     • <line>` per source: for the active account `active profile: <status>`; for others `session profile: <status>` (or `session profile: ignored (different account)`) and `stored backup: <status>`, where `<status>` = `fresh|expired, refresh token yes|no, expires <clock> in <countdown>` or `unknown expiry, refresh token yes|no`. API-key accounts emit no token line.

**Go reference example** (usage unavailable case):
```
$ cswap list
Accounts:
  1: alice@example.com [personal] (active)
     usage unavailable (http-429)

  2: dev (bob@example.com) [personal]
     usage unavailable (http-429)

  3: key@example.com [personal]
     API key (no quota)

  5: carol@example.com [personal] (disabled)
     usage unavailable (http-429)
```
**[go]** additionally renders an `at limit: <windows>` marker (e.g. `5h`, `7d`, `Fable 5`) and pairs it with JSON `atLimit`/`limitingWindows`; **[py]** has no such marker.

**`--json` payload** (`_build_list_payload`):
```json
{
  "schemaVersion": 1,
  "activeAccountNumber": 2,
  "accounts": [ <account-row>, ... ],
  "duplicateAccountWarnings": ["..."],   // [py] additive, only when non-empty
  "lockstepUsageWarnings": ["..."],      // [py] additive, only when non-empty
  "unclaimedCredentials": ["<id>", ...]  // [py] additive, sorted stash ids, only when non-empty
}
```
`<account-row>` — see §12.1. `activeAccountNumber` is `null` when no managed account is the live login.

**Errors**: `--token-status cannot be combined with --json` (exit 2); `--json can only be used with 'list', 'status', or 'switch'` (exit 2, parser); handled errors → `Error: <msg>` / envelope, exit 1.

---

## 6. `status`

**Synopsis**: `cswap status [--json] [--debug]` (legacy `--status`).

**Reads**: `sequence.json`, the live Claude config (`~/.claude.json` or `CLAUDE_CONFIG_DIR` equivalent; a cswap session pin is ignored), `cache/usage.json`. Writes nothing (aside from usage-cache refresh).

**Human output**:
- No live login: `Status: No active Claude account` (`Status:` bolded, rest dimmed).
- Live login but no `sequence.json`, or not a managed slot: `Status: <email> (not managed)`.
- Managed:
```
Status: Account-1 (test@example.com [personal])
  Total managed accounts: 2
  ├ 5h:  25%   resets 20:39         in 1h 30m
  └ 7d:  16%   resets Jul 5 08:59   in 1d 19h
```
Header: `bolded('Status:') accent('Account-<n>') (<email> muted('[<tag>]'))`; then `  dimmed('Total managed accounts: <total>')`; then the same `_usage_entry_lines` as `list`, indented 2 spaces.

**Go example**:
```
$ cswap status
Status: Account-1 (alice@example.com [personal])
  Total managed accounts: 4
  usage unavailable (http-429)
```

**`--json` payload** (`_build_status_payload`):
- No live login: `{"schemaVersion": 1, "active": null}`
- Live but unmanaged (no roster, or slot not found): `{"schemaVersion": 1, "active": {"email": "<email>", "managed": false}}`
- Managed:
```json
{
  "schemaVersion": 1,
  "active": {
    "number": 1,
    "email": "a@example.com",
    "organizationName": "",
    "organizationUuid": "",
    "isOrganization": false,
    "managed": true,
    "usageStatus": "ok",
    "usage": { ... } ,
    "alias": "dev",                    // only when set
    "usageFetchedAt": "2026-07-17T12:00:00Z",  // only alongside non-null usage
    "usageAgeSeconds": 1.0
  },
  "totalManagedAccounts": 2
}
```
Same additive rules as a list row (§12.1), including **[py]** `lastGoodUsage`/`lastGoodFetchedAt`/`lastGoodAgeSeconds` when `usage` is null but a last-good measurement exists. Note the Go example prints `"totalManagedAccounts"` and the Python only includes it on the managed branch.

**Exit**: 0 / 1. Script idiom: `cswap status --json | jq -r '.active.email'`.

---

## 7. `switch`

**Synopsis**:
```
cswap switch [--json] [--debug]                                            # rotate to next
cswap switch <NUM|EMAIL|ALIAS> [--json] [--force] [--debug]                # direct target
cswap switch --strategy {best|next-available} [--model NAMES] [--json] [--debug]
```
Legacy: `--switch`, `--switch-to <id>`.

**Options**: `--json`; `--force` (only with a target: rewrite the live login from the stored backup **without backing up the current login first**; skips the already-active guard); `--strategy best|next-available` (only with bare switch); `--model NAMES` (only with `--strategy`; comma list, case-insensitively deduped first-spelling-wins, or `all`).

**Locks**: backup-root `.lock` + the tool's own credential lock (`~/.claude.lock` dir) + config lock (`~/.claude.json.lock` dir), so a switch never interleaves with the tool's token refresh. Timeout on the tool's locks → `ClaudeCodeLockTimeout` (nothing mutated).

### 7.1 Bare `switch` (rotation) semantics

- Precondition `sequence.json` exists else `ConfigError("No accounts are managed yet")`.
- **No live login (fresh machine)**: target = recorded `activeAccountNumber`, else first of `sequence`. If disabled / not switchable it is skipped (human `Skipping Account-<n> (disabled)` or `Skipping Account-<n> (no stored credentials/config, re-add with cswap --add-account --slot <n>)`; JSON warning `Skipped Account-<n> (disabled)` / `Skipped Account-<n> (no stored credentials/config)`), falling back to the first enabled+switchable slot. None left → `ConfigError("No accounts remain in rotation. Re-enable one with: cswap enable <num|email>")` (if some are merely disabled) or `ConfigError("No managed accounts have valid stored credentials/config. Re-add a slot with: cswap --add-account --slot <number>")`. Strategies are ignored on this path. Human output: `Activated Account-<n> (<email>)`.
- **Live login unmanaged**: human mode prints `Notice: Active account '<email>' was not managed.`, auto-adds it, prints `It has been automatically added as Account-<n>.` and `Please run the switch command again to switch to the next account.`, exit 0. JSON mode: no auto-add, no-op payload `reason:"unmanaged-account"`, `from`=`to`=`{"number": null, "email": "<email>"}`, message `Active account is not managed; run cswap --add-account`.
- **Only one account**: human `Only one account is managed. Add more accounts to switch between.` (dimmed); JSON `reason:"only-one-account"`, same message.
- **Rotation**: anchor on `activeAccountNumber` (plain) or on the live slot (`next-available`); walk `sequence` circularly; skip disabled (`Skipping Account-<n> (disabled)` / warning `Skipped Account-<n> (disabled)`), skip non-switchable (`Skipping Account-<n> (no stored credentials/config, re-add with cswap --add-account --slot <n>)` / `Skipped Account-<n> (no stored credentials/config)`), and for `next-available` skip exhausted (`Skipping Account-<n> (at <label> limit)` where label is `5h/7d` or the binding window names e.g. `Fable`, `5h/Fable`). If none found: with exhausted skips → `warning("All other accounts are at their <limits_label> — staying on Account-<n>.")` / JSON `reason:"candidates-exhausted"`; otherwise dimmed `No other accounts have valid stored credentials/config.\nRe-add a skipped slot with: cswap --add-account --slot <number>` / JSON `reason:"no-valid-target"`, message `No other accounts have valid stored credentials/config.` `limits_label` = `usage limits` when models are configured, else `5h/7d limit`.
- If the target equals the live slot: `Already on Account-<n> (<email>)` (accent) / JSON `reason:"already-active"`.
- **`--strategy best`**: pick the enabled, switchable slot with the greatest headroom (`100 − max(5h, 7d[, named scoped windows])`); switch only if **strictly greater** than the current account's headroom; ties → earliest slot / stay. No-op notes:
  | note | JSON reason | human message |
  |---|---|---|
  | current usage unknown | `usage-unavailable` | `Current account usage is unavailable — staying on Account-<n>. Run cswap --switch to rotate.` |
  | no other account has usage | `usage-unavailable` | `No other account has usage data to compare — staying on Account-<n>. Run cswap --switch to rotate.` |
  | some usage unknown, none better | `usage-unavailable` | `No account with known usage has more remaining quota; some usage is unavailable — staying on Account-<n>.` |
  | already best | `already-best` | `Already on the account with the most remaining quota (Account-<n>).` (accent) |
  | all exhausted | `candidates-exhausted` | `All accounts are at their <limits_label> — staying on Account-<n>.` (warning) |
- Model announcement (human only, when models are in effect): `Using configured model limits: <m1, m2> (from --model)` or `(from autoswitch.model)` (dimmed). Inert-model typo guard (only when every account's usage is readable): `model(s) <names> match no account's usage windows (typo?)` via `warning()` or in JSON `warnings`.

### 7.2 `switch <id>` semantics

- Resolution per §3 (interactive disambiguation in human mode). Missing → `AccountNotFoundError("No account found with identifier: <id>")` / `Account-<n> does not exist`.
- **Already-active guard** (skipped with `--force`): if the live identity is this slot and the live credential matches the stored backup → human `Already on Account-<n> (<email>)` then dimmed `To rewrite the live login from the stored backup (e.g. after --import), run: cswap --switch-to <n> --force`, exit 0; JSON `strategy:"direct"`, `reason:"already-active"`.
- Broken target: `SwitchError("Account-<n> has no stored credentials. Re-add with: cswap --add-account --slot <n>")` / `SwitchError("Account-<n> has no stored config backup. Re-add with: cswap --add-account --slot <n>")` / `Invalid backup config: <e>` / `Invalid oauthAccount in backup`.
- Departing account backup failures: `CredentialReadError("Failed to read current credentials")`, `CredentialReadError("Current account credential is empty (Keychain unreadable?); refusing to overwrite its backup")`, `ConfigError("Claude config file not found")`, `ConfigError("Permission denied reading Claude config")`. Rollback on mid-switch failure: `SwitchError("Switch failed and was rolled back: <e>")` or `SwitchError("Switch failed and rollback also failed: <e>. Manual recovery may be needed.")`.
- Live-session warning (never blocks): `Account-<n> (<email>) has a live session-mode Claude instance (PID <ids>). Running the same account as both the default login and a session can make one copy's token go stale if the server rotates it. If the session later fails to authenticate, exit it and re-run 'cswap run <n>'.` (human `warning()`, JSON `warnings[]`).
- Credential-ownership warnings (identity oracle): `Credential ownership mismatch detected. The live credential was preserved and was not written into Account-<cur>. If Account-<other> later cannot authenticate, log in as it and run: cswap add --slot <other>`; `The live login does not match a managed account. It was preserved and not written into Account-<cur>. If you need that account, log in as it and run: cswap add`; `Credential ownership mismatch detected. The live credential already matches Account-<other>'s stored backup, so nothing was written into Account-<cur>.`
- `--force` on the already-active slot: JSON `switched:false`, `reason:"activated"`, message `Activated Account-<n> (<email>) from stored backup`; a cross-slot force is a normal `switched:true`.
- Mutating from inside a session shell (`CLAUDE_CONFIG_DIR` inside `<backup_root>/sessions/`) → `SwitchError("This shell is inside a cswap run session profile (CLAUDE_CONFIG_DIR points at it). Mutating accounts here would operate on the wrong live store — unset CLAUDE_CONFIG_DIR or run from a normal shell.")` (applies to switch/add/remove/swap/move/alias/purge).

### 7.3 Human output after a successful switch

```
Switched to Account-2 (bob@example.com)
Accounts:
  ... (full `list` rendering, post-switch usage) ...

<followup line>

```
Sequence: `accent('Switched to') Account-<n> (<email>)`, then a nested `list_accounts()` (on failure: `  (usage display unavailable — run cswap --list to retry)`), blank line, **followup line** (dimmed), blank line. On the direct-activation path (fresh machine / unmanaged live / `--force`): `accent('Activated') Account-<n> (<email>)`, blank line, followup, blank line (no list).

**Restart-semantics followup line** (keyed to where the active credential write landed):
- keychain backend (macOS): `Restart Claude Code to apply immediately — otherwise the session can take up to ~30 seconds to pick up the new account.`
- file backend (Linux/WSL/Windows, or macOS fallback): `New account is active on your next message — no restart needed.`

A switch never *requires* a restart to be correct; the note is informational.

### 7.4 `--json` payload

```json
{
  "schemaVersion": 1,
  "switched": true,
  "from": {"number": 1, "email": "alice@example.com"},
  "to": {"number": 2, "email": "bob@example.com"},
  "strategy": "direct",
  "reason": "switched",
  "message": "Switched to Account-2 (bob@example.com)",
  "warnings": [],
  "models": ["Fable"],           // only when a model list was in effect (CLI adds these)
  "modelSource": "cli"           // "cli" | "autoswitch.model"
}
```
- `strategy` ∈ `rotation` | `best` | `next-available` | `direct`.
- `reason` ∈ `switched` | `already-active` | `activated` | `unmanaged-account` | `only-one-account` | `candidates-exhausted` | `no-valid-target` | `usage-unavailable` | `already-best`.
- `switched` is identity-based (`from != to`). Every `switched:false` payload has `from == to`. `from.number` is `null` when leaving an unmanaged live login; `from` is `null` on a fresh machine. `to`/`from` may be `null` on some no-ops (only-one-account with an unresolved slot, no-valid-target without a current ref).
- `message` on success: `Switched to Account-<n> (<email>)`; on no-op `Already on Account-<n> (<email>)`.

**Errors (exit 2)**: `argument --strategy: invalid choice: '<v>' (choose from 'best', 'next-available')`; `--strategy can only be used with bare 'switch'`; `--model can only be used with 'switch --strategy best' or 'switch --strategy next-available'`; `--force can only be used with 'import' or 'switch <num|email>'`. Exit 1: `No account found with identifier: <id>` (`AccountNotFoundError`) and the `SwitchError`/`ConfigError`s above.

---

## 8. Account management commands

### 8.1 `add`

**Synopsis**: `cswap add [--slot NUM] [--alias NAME] [--debug]` (legacy `--add-account`).

**Options**: `--slot NUM` (int; default next free = `max(existing)+1`; `< 1` → `ConfigError("Slot number must be >= 1")`); `--alias NAME` (validated per §3.1).

**Semantics**: snapshots the currently logged-in account (live config `oauthAccount` + live credentials) into the backup root. Refuses inside a session shell. Refuses to capture a live API key (`_reject_live_api_key_capture`).
- No live login → `ConfigError("No active Claude account found. Please log in first.")`.
- Credentials unreadable → `CredentialReadError("Failed to read credentials for current account")`; empty → `CredentialReadError("No credentials found for current account")`; config missing → `ConfigError("Claude config file not found")`; permission → `ConfigError("Permission denied reading Claude config")`.
- **Already managed and no `--slot`** → refresh in place (credentials + config rewritten, dead-token quarantine cleared, `activeAccountNumber` set), output: `Updated credentials for Account <n> (<email> [<tag>]).` (`Updated credentials` accented). This is the documented recovery for `relogin_required`.
- **`--slot N` occupied by a different account**: `warning("Slot N already occupied")`, then `<existing_email> [<tag>]`, then prompt `Overwrite slot N? [y/N] `; anything other than `y`/`yes` → `Cancelled` (dimmed), exit 0. On yes: the occupant's files, roster record, and directory mappings are removed (`Removed <k> directory mapping(s) for this account`, dimmed).
- **`--slot N` while the same identity lives in another slot** → migrated: prints `Moved from slot <old> → <N>` (dimmed) before the added line; the alias carries over.
- Alias collision → `ValidationError("Alias '<alias>' is already used by account <n>")`.
- Success line: `Added Account <n>: <email> [<tag>]` (`Added` accented, tag muted). The new account becomes `activeAccountNumber`.

**Exit**: 0 (including cancelled prompt); 1 handled error.

### 8.2 `add-token`

**Synopsis**: `cswap add-token [TOKEN|-] [--email EMAIL] [--slot NUM] [--debug]` (legacy `--add-token [TOKEN|-]`).

**Positional**: `TOKEN` optional. Literal `-` → read one line from stdin (`rstrip("\n")`). Omitted → secure prompt `Token: ` (no echo). Token is stripped; empty → `ValidationError("Token cannot be empty")`.

**Kind detection**: `looks_like_api_key(t)` = stripped text starts with `sk-ant-api` and not with `{` → managed API key (stored raw, `kind: "api_key"`); anything else is treated as an OAuth setup-token and wrapped as `{"claudeAiOauth": {"accessToken": <token>, "scopes": ["user:inference"]}}`. No network calls are made.

**Email**: `--email` validated (`ValidationError("Invalid email format: <email>")`); default `setup-token-<slot>@token.local` or `api-key-<slot>@token.local`, where slot is `--slot` or the next free number. Token accounts are always `personal` (org uuid `""`). Cross-kind collision on the same email is rejected (`_reject_cross_kind_collision`).

**Semantics**: same slot/overwrite/migrate rules as `add` (`Slot N already occupied` + `Overwrite slot N? [y/N] `; `Moved from slot <old> → <N>`). Existing identity with no `--slot` → refresh in place: `Updated token for Account <n> (<email> [personal]).` or `Updated API key for Account <n> (...)`. Success: `Added Account <n>: <email> [personal] (from token)` / `(from API key)`.

Synthesized config: `{"oauthAccount": {"emailAddress": <email>, "accountUuid": "", "organizationUuid": null, "organizationName": null}}`.

**Exit**: 0 / 1.

### 8.3 `remove` / `rm`

**Synopsis**: `cswap remove <NUM|EMAIL|ALIAS> [--debug]` (legacy `--remove-account <id>`).

**Semantics**: `sequence.json` missing → `ConfigError("No accounts are managed yet")`. Resolution per §3 with interactive disambiguation (`Enter account number to remove: `). Live session-mode instance on that slot → `SessionError("Account-<n> (<email>) has a live session-mode Claude instance (PID <pids>). Exit it first, then retry --remove-account.")` (or the unreadable-records variant `... has <k> session record(s) that could not be read, so whether a Claude instance is live cannot be determined. Inspect <dir>/sessions and remove or repair them, then retry --remove-account.`). If the target is the active account: `warning("Warning: Account-<n> (<email>) is currently active")`. Prompt: `Are you sure you want to permanently remove Account-<n> (<email>)? [y/N] `; only `y` (case-insensitive) proceeds, else `Cancelled` (dimmed), exit 0. Deletes credential + config backups, session profile, roster record and sequence entry; prunes directory mappings (`Removed <k> directory mapping(s) for this account`, dimmed). Does **not** prune `cache/usage.json` rows. Output: `Removed Account-<n> (<email>)` (`Removed` accented). Go doc shows a trailing period; Python has none.

### 8.4 `disable` / `enable`

**Synopsis**: `cswap disable <NUM|EMAIL|ALIAS> [--debug]`, `cswap enable <NUM|EMAIL|ALIAS> [--debug]` (legacy `--disable-account`, `--enable-account`).

**Semantics**: `ConfigError("No accounts are managed yet")` if no roster; resolution via `resolve_account` (hard error on ambiguity). Sets/clears `disabled: true` on the roster record; `lastUpdated` bumped. Disabled slots are skipped by `auto`, bare rotation, and the `best`/`next-available` strategies but remain valid explicit `switch <id>` targets and keep their credentials. Re-enabling restores the original sequence position.

**Output**:
- Already in the requested state: `Account-<n> (<email>) is already disabled.` / `... is already enabled.` (dimmed), exit 0.
- `Disabled Account-<n> (<email>).` (verb accented); if it is the active account, a second dimmed line `  It is the active account — it stays live until you switch away; it just won't be an automatic switch target.`; if no switchable accounts remain, `warning("  No accounts remain in rotation — auto-switch and bare switch have nothing to pick. Re-enable one with cswap enable <num|email>.")`.
- `Enabled Account-<n> (<email>).` then dimmed `  It is back in the rotation.`

**Exit**: 0 / 1 (`No account found with identifier: <id>`).

### 8.5 `alias`

**Synopsis** (pre-dispatched, `prog = "cswap alias"`):
```
cswap alias                              # list
cswap alias <NUM|EMAIL|ALIAS> <NAME>     # set / rename (identifier may be the old alias)
cswap alias <NUM|EMAIL|ALIAS> --unset    # remove
[--debug]
```
Description: `Set, remove, or list a short display alias for an account. Once set, the alias can be used anywhere an account number or email is accepted (switch, remove, run, map).`
Epilog examples: `cswap alias 2 dev`, `cswap alias user@example.com dev`, `cswap alias 2 --unset`, `cswap alias                         # list all aliases`.

**Argument validation (exit 2)**: `--unset does not take a NAME argument`; `NUM|EMAIL is required with --unset`; `NAME is required (or pass --unset to remove the alias)`.

**Output**:
- List: `No aliases set` (dimmed) when none; else `Aliases:` (bolded) then `  <n>: <alias> (<email>)` (email muted, parenthesized) in slot order.
- Set: `Set alias '<normalized>' for Account <n>` (`Set alias` accented).
- Unset: `Removed alias for Account <n>` (idempotent: succeeds silently when no alias was set).

**Errors (exit 1)**: invalid name → `ValidationError` (§3.1 messages); collision → `ConfigError("Alias '<a>' is already used by account <n>")`; `No account found with identifier: <id>`; corrupt roster → `ConfigError` naming the file (never reports `No aliases set` on a corrupt roster **[go]**).

### 8.6 `swap`

**Synopsis** (pre-dispatched): `cswap swap <NUM|EMAIL|ALIAS> <NUM|EMAIL|ALIAS> [--debug]`. Description: `Exchange two accounts' slot numbers, so they trade places in \`cswap list\` and as numeric targets. Aliases, backups, and session history move with their account.` Examples: `cswap swap 1 2`, `cswap swap dev user@example.com`.

**Semantics**: under the account lock, both identifiers resolved; `ValidationError("Cannot swap an account with itself")`; roster records, credential/config backups, `sequence` membership (kept sorted), `activeAccountNumber` and session profile dirs all follow their account. Directory mappings (keyed on identity) are unaffected; usage-cache and quarantine rows self-heal. Unreadable backup → `ConfigError("Account-<n>'s stored credential could not be read (keychain unavailable?); nothing was changed. Retry once it is readable again.")`.

**Output**: `Swapped Account <a> and Account <b>:` (`Swapped` accented) then `  <n>: <email>` for the two slots in numeric order.

**Errors**: missing positional → argparse `the following arguments are required: ...` (exit 2); `No account found with identifier: <id>` / `Account-<n> does not exist` (exit 1); `ConfigError("No accounts are managed yet")`.

### 8.7 `move`

**Synopsis** (pre-dispatched): `cswap move <NUM|EMAIL|ALIAS> <SLOT> [--debug]`. Description: `Assign an account to a slot number. An empty slot relocates the account there and frees its old slot; an occupied slot swaps the two. Aliases, backups, and session history move with the account.` Examples: `cswap move user@example.com 1   move an account onto shortcut 1`, `cswap move dev 1                by alias`, `cswap move 2 1                  by number (swaps if slot 1 is taken)`.

**Semantics**: `SLOT` stripped; must be all digits and ≥ 1 else `ValidationError("Target slot must be a positive slot number, got: '<v>' (use \`swap\` to trade two accounts by identifier)")`; normalized (`"01"`→`"1"`); cap = `max(99, highest existing slot)`, above it → `ValidationError("Target slot <t> is out of range (1-<cap>): new accounts are numbered from the highest slot, so a large target would inflate future account numbers")`. Same slot → no-op; occupied → swap; empty → relocate. A live session on the source slot → `SessionError(... Exit it first, then retry --move-account.)`.

**Output**: `Already in slot <n>: <email>` (`Already in` dimmed) | `Swapped Account <a> and Account <b>:` + two lines | `Moved <email> to slot <n>` (`Moved` accented).

**Go example**:
```
$ cswap move 1 3
Swapped Account 1 and Account 3:
  1: key@example.com
  3: alice@example.com
```

---

## 9. Session mode: `run`, `env`, `map`, `unmap`, `unclaimed`

### 9.1 `run`

**Synopsis** (pre-dispatched, `prog = "<prog> run"`):
```
cswap run [<NUM|EMAIL|ALIAS>] [--no-share] [--share-history|--no-share-history] [--debug] [-- <claude args>]
```
Description: `[EXPERIMENTAL] Launch Claude Code as a stored account in this terminal only (the default login and other terminals are unaffected).` Epilog examples: `cswap run 2`, `cswap run user@example.com`, `cswap run 2 --no-share`, `cswap run 2 --share-history`, `cswap run 2 -- --resume`.

**Argument split**: everything after the **first** literal `--` is forwarded to the child verbatim and never parsed (`cswap run 2 -- --no-share` forwards `["--no-share"]`). Unknown flag before `--` → `unrecognized arguments: ...`, exit 2.

**Options**:
- `account` positional, optional. `Account to run (number or email). Omit to use the current directory's mapping (see \`cswap map\`).`
- `--no-share` flag: `Don't share settings/keybindings/CLAUDE.md/skills/commands/agents from ~/.claude into the session profile (and remove previously shared items)`.
- `--share-history` / `--no-share-history` (boolean-optional, default false): `Share conversation history (projects/ and history.jsonl) from ~/.claude into the session profile, so every account sees one unified history. History the profile already accumulated is merged into ~/.claude first. --no-share-history restores per-account history (the default). Not supported on Windows.` Independent of `--no-share`.
- `--debug`.

**Semantics**:
1. Root guard.
2. With an account: `SessionManager.run(account, tail, share=not no_share, share_history)`:
   - `claude` not on PATH → `SessionError("'claude' was not found on PATH. Install Claude Code first.")`.
   - `--share-history` on Windows → `SessionError("--share-history is not supported on Windows yet: sharing uses re-synced copies there, which would fork the history instead of sharing it.")`.
   - Resolution via `resolve_account` (hard errors, no prompt).
   - API-key account → `SessionError("Account-<n> (<email>) is an API-key account; 'cswap run' (session mode) does not support API-key accounts yet. Use 'cswap --switch-to' to make it your default login instead.")`.
   - If the chosen account is already the active default login and `CLAUDE_CONFIG_DIR` is not preset → launches the default login directly (no second credential copy). A preset `CLAUDE_CONFIG_DIR` is treated as an override and replaced by the session profile.
   - Prepares the persistent profile `<backup_root>/sessions/<n>-<email with @→_>` (bootstrap, token validate, MCP mirror, credential scrub, share sync), prints `Launching Account-<n> (<email>) [session mode]` (`Launching` accented, `[session mode]` muted), scrubs the auth-override env vars (§14.1), sets `CLAUDE_CONFIG_DIR=<profile>`, then **execs** `claude <tail>` (POSIX `execvpe`, never returns; Windows stays resident and mirrors the exit code).
3. Without an account: `slot_for_directory(cwd)` (nearest-ancestor mapping, §9.3):
   - `(slot, email)` → same as above with that slot.
   - `(None, email)` (mapping exists, account removed) → `warning("Mapped account <email> no longer exists — launching the default account.")` then `exec_default(tail)`.
   - `(None, None)` → dimmed `No account mapped for <cwd> — launching the default account.` then `exec_default(tail)` (plain `claude`, unmodified env, no scrubbing).

**Exit**: child's exit status; 1 on a pre-exec `ClaudeSwitchError` (`Error: <msg>` on stderr); 2 usage; 130 Ctrl-C.

Sharing details (README): user-scope MCP servers are mirrored from the default profile on every launch (definitions copied as-is, OAuth logins not); `--no-share` turns sharing off and removes the mirrored MCP config; with `--share-history` a session under one account appears in `--resume` under the others.

### 9.2 `env` **[go-only; no Python counterpart]**

**Synopsis**: `cswap env [<NUM|EMAIL|ALIAS>] [--no-share] [--share-history] [--shell {sh|fish|pwsh}] [--unset] [--debug]`

Prints shell-evalable lines that pin the current shell to a stored account's session profile without launching the tool; prepares the same profile `run` does. Intended for `eval "$(cswap env 2)"`. `--unset` prints only the unset line (takes no account, skips bootstrap). With no account, resolves from the cwd mapping; unlike `run` there is **no default-login fallback** → exit 1 with `Nothing to prepare an environment for (...). Pass an account ..., map this directory ..., or clear a pinned profile with cswap env --unset.` When the chosen account is already the active default login and `CLAUDE_CONFIG_DIR` is unset: nothing exported, one stderr note `Account-<n> (<email>) is the active default login — an unpinned shell already uses it; nothing exported.`, exit 0. Before the export, one unset line per currently-set auth-override variable (§14.1) is emitted.

| shell | export | unset |
|---|---|---|
| `sh` (default) | `export CLAUDE_CONFIG_DIR='<dir>'` | `unset CLAUDE_CONFIG_DIR` |
| `fish` | `set -gx CLAUDE_CONFIG_DIR '<dir>'` | `set -e CLAUDE_CONFIG_DIR` |
| `pwsh` | `$env:CLAUDE_CONFIG_DIR = '<dir>'` | `Remove-Item Env:CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue` |

stdout carries only eval-able lines; `Prepared Account-<n> (<email>) [session mode]` and all warnings go to stderr. Errors (exit 2): `argument --shell: invalid choice: '<v>' (choose from sh, fish, pwsh)`, `--unset does not take a NUM|EMAIL|ALIAS argument`, `argument --shell: expected one argument`, `unrecognized arguments: <tok>`.

### 9.3 `map`

**Synopsis** (pre-dispatched, `prog = "cswap map"`): `cswap map [<NUM|EMAIL|ALIAS> [PATH]] [--debug]`. Description: `Map a stored account to a directory so \`cswap run\` (with no account) auto-launches it there. With no arguments, lists all mappings.` Examples: `cswap map 2 ~/work/client-app`, `cswap map user@example.com          # map the current directory`, `cswap map                           # list all mappings`.

**Semantics**: root guard. No account → list. With account: `resolve_account` (hard errors); `target = PATH or cwd`; if not an existing directory → `warning("Warning: <target> is not an existing directory (mapping it anyway)")` (stdout) and proceed; `previous = store.get(target)` (exact key) read before `store.set(target, email, orgUuid)`.

**Output**:
- Set: `Mapped <normalized path> → Account-<n> (<email>)` (`Mapped` accented); when a previous mapping pointed at a different email, append ` (was <prev_email>)` (muted).
- List, empty: `No directory mappings yet.` (dimmed) then `Map one with: cswap map <NUM|EMAIL> [PATH]` (muted).
- List, populated: `Directory mappings:` (bolded), then for each path in sorted key order: `  <path> → <n>: <email> [<tag>]` (arrow dimmed, tag muted) or `  <path> → <email> (account removed)` when the identity no longer has a slot. (Go doc prints `~`-abbreviated paths in its example; Python prints the normalized absolute key.)

**Mapping store** `<backup_root>/mappings.json`:
```json
{
  "schemaVersion": 1,
  "mappings": {
    "<normalized absolute path>": { "email": "work@co.com", "organizationUuid": "org-1", "added": "2026-07-17T12:00:00Z" }
  }
}
```
Key normalization: expand `~`, absolutize, **resolve symlinks**, `normcase` (case-fold + `\` on Windows; no-op on POSIX), so `path`, `path/`, `path/.` produce one key. Identity is `(email, organizationUuid)` (never the slot number; `organizationUuid` is `""` never null). Corrupt/missing file reads as empty. Written atomically (tempfile `.mappings-*.tmp` + replace, 0600/0700 on POSIX); write failures propagate.

**Nearest-ancestor lookup** (`resolve(cwd)`): a mapping matches when its directory equals cwd or is a component-wise ancestor (`/foo/bar` does not match `/foo/barbaz`); the longest matching key wins (deepest = most specific); no ties possible. Subfolders inherit the nearest mapped ancestor. Mappings are per-machine (never exported) and are pruned when their account is removed or displaced by `add --slot` (not on `swap`/`move`/`import --force`, which keep identity).

**Errors**: `unrecognized arguments` (2); `No account found with identifier: <id>` (1); ambiguity `ConfigError` (1).

### 9.4 `unmap`

**Synopsis** (pre-dispatched, `prog = "cswap unmap"`): `cswap unmap [PATH] [--debug]`. Description: `Remove a directory → account mapping (default: current directory).`

**Output**: `Unmapped <normalized path>` (`Unmapped` accented) when one was removed, else `No mapping for <normalized path>` (dimmed). Exit 0 either way; 2 on usage error. Exact-key removal only (no ancestor walk).

### 9.5 `unclaimed` **[py-only; not in the Go reference]**

**Synopsis** (pre-dispatched, `prog = "<prog> unclaimed"`): `cswap unclaimed [--purge ID] [--debug]`. Description: `List stashed credential entries, or purge one by id. Purging deletes the bytes — recovery is /login + \`cswap add\`.` `--purge ID` help: `Delete this entry's bytes and manifest row`.

The stash holds credential bytes a switch could not attribute to a slot (reasons such as `displaced-live-login`, `foreign`, `alien`). Rows normally clear themselves.

**Output**: `--purge` with unknown id → stderr `Error: no unclaimed entry <id>`, exit 1; success `Purged <id>` (`Purged` accented). Listing: `No unclaimed credential entries` (dimmed) when empty; else one line per entry sorted by id: `<id>  slot <configSlot or ?>  <reason or "orphaned (no manifest row)">`. `list --json` exposes the ids under `unclaimedCredentials`.

---

## 10. `auto`

**Synopsis** (pre-dispatched, `prog = "cswap auto"`):
```
cswap auto [--once] [--json] [--interval SECONDS] [--threshold PCT] [--cooldown SECONDS]
           [--model NAMES] [--include-api-key-accounts|--no-include-api-key-accounts]
           [--strategy {best,consume-first}] [--dry-run] [--debug]
```
Description: `Automatically switch accounts when the active one nears its 5h/7d rate limit. Runs a foreground polling loop; use --once for a single tick (cron-friendly).`

Epilog (verbatim):
```
Exit codes with --once:
  0  switched to another account
  1  error (network trouble, lock contention, ...)
  2  no action needed
  3  blocked: wanted to switch but no viable target / all exhausted

Examples:
  cswap auto                       # foreground loop, switch at 90% used
  cswap auto --threshold 80        # switch earlier
  cswap auto --model Fable         # also switch when the Fable weekly limit is hit
  cswap auto --json                # one JSON event per line (for scripts)
  cswap auto --once; echo $?       # single tick, outcome in exit code
  cswap auto --dry-run             # log decisions, never actually switch

Defaults live in settings.json in the backup root; flags override them.
```

**Options** (all defaults come from `settings.json`, overridden for this run only, then **re-clamped**):

| flag | type | default (setting) | clamp | help |
|---|---|---|---|---|
| `--once` | flag | off (loop) | — | `Evaluate once, maybe switch, and exit (exit code = outcome)` |
| `--json` | flag | off | — | `Emit one machine-readable JSON event per line on stdout` |
| `--interval SECONDS` | float | `autoswitch.intervalSeconds` = 60 | 15–3600 | `Poll interval in loop mode (min 15; default 60)` |
| `--threshold PCT` | float | `autoswitch.threshold` = 90 | 50–99.9 | `Switch when the active account's binding 5h/7d window reaches this utilization (50-99.9; default 90)` |
| `--cooldown SECONDS` | float | `autoswitch.cooldownSeconds` = 300 | 0–86400 | `Minimum time between proactive switches (default 300)` |
| `--model NAMES` | string | `autoswitch.model` = none | — | `Also switch when a per-model weekly limit is hit, not just the account-wide 5h/7d windows. One name or a comma-separated list (e.g. Fable, Opus, Sonnet, Haiku, or 'Fable,Opus'), or 'all' for every per-model window an account reports` |
| `--include-api-key-accounts` / `--no-include-api-key-accounts` | tri-state bool (`None` = unset) | `autoswitch.includeApiKeyAccounts` = false | — | `Allow switching onto managed API-key accounts as a last resort (they bill per token; default: excluded)` |
| `--strategy` **[py]** | choice `best` \| `consume-first` | `autoswitch.strategy` = `best` | — | `Target selection: 'best' (most quota left; default) or 'consume-first' (proactively use the account whose weekly window resets soonest)` |
| `--dry-run` | flag | off | — | `Evaluate and report, but never switch or write state` |
| `--debug` | flag | off | — | `Enable debug logging` |

Bad numeric → `argument --interval: invalid float value: '<v>'` (exit 2); missing value → `argument --interval: expected one argument`.

**Semantics**: root guard (inlined); settings = `merged_with_cli(load_settings(root), args)` (only non-None CLI values override; result clamped, so `--interval 1` → 15). Engine emits events through `jsonl_emit` (compact `json.dumps(event.to_json())`, flushed) or `human_emit` (`HH:MM:SS  <line>` local time, flushed; color by kind: `switch`→accent, `error`/`account-quarantined`→yellowed, `poll`/`no-switch`/`sleep`→dimmed).
- `--once`: `exit(engine.tick().value)`; `tick()` never raises (a `ClaudeSwitchError` becomes an `error` event, outcome 1).
- Loop: SIGTERM → clean stop, exit 0; if not JSON print the dimmed banner `Auto-switch running: threshold <t:.0f>%, every <i:.0f>s[ (dry-run)] — Ctrl-C to stop`; Ctrl-C → `\nAuto-switch stopped` (stderr in JSON mode), exit 130. A `ClaudeSwitchError` before the loop → compact envelope (JSON) or `Error: <e>`, exit 1.
- Behavior summary: switches proactively when the active account's binding window (max of 5h/7d, plus any `--model` windows) reaches the threshold; a proactive candidate must be below the threshold and beat the active account by `hysteresisPct`; cooldown between proactive switches; unhealthy after `unhealthyTicks` failed polls; dead-token accounts are quarantined (state in `<backup_root>/autoswitch_state.json` under `.autoswitch_state.lock`); disabled accounts never targeted; API-key accounts only with `includeApiKeyAccounts`; freshens the target token before activating; exhausted set → bounded slow cadence until the earliest reset.

**Exit codes** (`TickOutcome`): `SWITCHED=0`, `ERROR=1`, `NO_ACTION=2`, `BLOCKED=3`. Loop returns 0 on clean stop.

**JSON event stream** — every event: `{"schemaVersion": 1, "event": "<kind>", "ts": "<RFC3339 UTC, Z suffix, seconds>", ...}`. Consumers must ignore unknown kinds/fields.

| `event` | extra fields | human line |
|---|---|---|
| `poll` | `active` (`{number,email}` or null), `headroomPct` (`{"<n>": float|null}`), `threshold` (float), `fetchErrors` (`{"<n>": "<cause>"}`, only if non-empty; causes like `http-429`, `http-401`, `timeout`), `windowsPct` (`{"<n>": {"5h": 3.0, "7d": 89.0, "Fable": 21.0}}`, only if non-empty; scoped names only when a model is configured) | `poll: no active account` or `Account-<n> (<email>): <used>% used|usage unknown[ (<err>)] (switch at <thr>%)[ | others: #<n>: <desc>, ...]` where desc = `5h 3% · 7d 89%` / `<used>%` / `? (<err>)` / `?` |
| `switch` | `trigger` (`proactive` \| `at-limit` \| `failover` \| **[py]** `consume-first`), `from` (ref or null), `to` (ref or null), `warnings` (array), `dryRun` (bool) | `Switched Account-<a> -> Account-<b> (<email>) (<trigger>)` or `[dry-run] would switch ...`; src `(none)` / dst `?` when missing |
| `no-switch` | `reason`, `detail` (`""` when none) | `no switch: <reason>[ (<detail>)]` |
| `account-quarantined` | `number` (string), `email`, `reason` (`invalid_grant` \| `identity-conflict`) | `Account-<n> (<email>) quarantined: <reason>. Log in with it and run 'cswap --add-account --slot <n>' to recover.` |
| `account-unquarantined` | `number`, `email`, `reason` (`credentials-replaced` default \| `account-replaced`) | `Account-<n> (<email>) back in rotation (<reason>)` |
| `all-exhausted` | `earliestResetAt` (string or null) | `all accounts exhausted; earliest reset <ts>` / `all accounts exhausted; no reset time known` |
| `sleep` | `seconds` (float, rounded to 1 dp), `until` (string) | `sleeping <m>m (until <until>)` |
| `error` | `message`, `transient` (bool, default true) | `error: <message>[ (will retry)]` |
| `config-warning` | `message` | `warning: <message>` (e.g. `autoswitch.model: Fabel matches no account's usage windows — only the 5h/7d limits are being watched for it (typo?)`) |

`no-switch` reasons (exhaustive): `unmanaged-active-account`, `no-active-account`, `active-api-key`, `below-threshold`, `active-idle`, `active-usage-unknown`, `cooldown`, `no-candidates`, `no-comparison`, `no-qualifying-candidate`, `no-viable-target`, `already-active`.

**Example** (Go doc):
```
$ cswap auto --once --json
{"active":{"email":"alice@example.com","number":1},"event":"poll","fetchErrors":{"1":"http-429","2":"http-429","5":"http-401"},"headroomPct":{"1":null,"2":null,"3":null,"5":null},"schemaVersion":1,"threshold":80.0,"ts":"2026-07-17T20:52:55Z"}
{"detail":"1/3 before failover","event":"no-switch","reason":"active-usage-unknown","schemaVersion":1,"ts":"2026-07-17T20:52:55Z"}
$ echo $?
2
```
Cron idiom: `*/5 * * * * cswap auto --once --json >> cswap-auto.log 2>&1` (cron needs `cswap` on its `PATH`).

---

## 11. `config` and `settings.json`

**Synopsis** (pre-dispatched, `prog = "cswap config"`):
```
cswap config [list] [--json] [--debug]
cswap config get <KEY> [--json] [--debug]
cswap config set <KEY> <VALUE> [--debug]
cswap config unset <KEY> [--debug]
cswap config path [--debug]
```
Description: `Read and edit claude-swap settings (settings.json in the backup root).` Default action = `list`. `--json` is accepted both before and after the verb (`config --json get X` == `config get X --json`); with any other action → `--json can only be used with list or get` (exit 2). Subparser metavar `{list,get,set,unset,path}`; bad action → `argument {list,get,set,unset,path}: invalid choice: '<a>' (...)` (2); missing KEY/VALUE → `the following arguments are required: KEY` / `KEY, VALUE` / `VALUE` (2).

Epilog:
```
Keys:
  <dotted key padded to 34>  <help> (default <formatted default>)
  ... one line per key in registry order ...

Examples:
  cswap config                              # list effective settings
  cswap config get autoswitch.threshold
  cswap config set autoswitch.threshold 80
  cswap config unset autoswitch.threshold   # back to the default
  cswap config path                         # where settings.json lives
```

**Key registry** (`SETTING_SPECS`, registry order; `default` must equal the dataclass default):

| dotted key | kind | range / choices | default | help |
|---|---|---|---|---|
| `autoswitch.threshold` | float | 50.0–99.9 | 90.0 (shown `90`) | `Switch when the binding 5h/7d window reaches this pct` |
| `autoswitch.intervalSeconds` | float | 15.0–3600.0 | 60.0 (`60`) | `Poll interval for the cswap auto loop, in seconds` |
| `autoswitch.cooldownSeconds` | float | 0.0–86400.0 | 300.0 (`300`) | `Minimum seconds between proactive switches` |
| `autoswitch.hysteresisPct` | float | 0.0–50.0 | 10.0 (`10`) | `A target must beat the active account by this many pct` |
| `autoswitch.strategy` | choice | **[py]** `best`, `consume-first` (**[go]** `best`, `soonest-reset`; old 08-doc: `best` only) | `best` | `How auto-switch picks the target account` |
| `autoswitch.includeApiKeyAccounts` | bool | — | false | `Allow rotating onto managed API-key accounts (bill per token)` |
| `autoswitch.unhealthyTicks` | int | 1–100 | 3 | `Consecutive failed polls before an account is unhealthy` |
| `autoswitch.model` | string | non-empty | none (`(none)`) | `Also switch on these models' weekly limits (e.g. Fable, Fable,Opus, or all)` |
| `ui.theme` **[py]** | choice | `dark`, `light`, `auto` | `auto` | `Color theme; auto follows the terminal background` |

File: `<backup_root>/settings.json`, `{"schemaVersion": 1, "autoswitch": {...camelCase...}, "ui": {...}}`. Unknown keys/sections survive round trips. **Reads are forgiving** (missing/corrupt → defaults with a logged warning; numbers clamped into range; bad types → default; unknown choice → default with warning `settings.json: unsupported <key> '<v>'; using '<default>'`). **Writes via `set` are strict.** `set` writes only that one key (+ `schemaVersion` if absent); `unset` removes the key and drops the section when it becomes empty; corrupt file on write → `ConfigError("<path> is not valid JSON (<e>); fix or delete it before changing settings")` / `ConfigError("<path> is not a JSON object; fix or delete it before changing settings")` / `ConfigError("could not read <path>: <e>")`, file left untouched. Atomic write: temp file beside the resolved target, 0600, parent 0700; writes **through** a symlink, never over it.

`is_set` = key present in the raw file (an explicit value equal to the default still counts).

**Value parsing (`set`)** — errors are `ConfigError` (exit 1):
- unknown key → `unknown setting '<key>'\nValid keys: <comma-joined dotted keys>`
- bool words `true/1/yes` → true, `false/0/no` → false (case-insensitive, stripped); else `<key> expects true or false (or 1/0, yes/no), got '<v>'`
- choice → `<key> must be one of: <choices comma-joined>`
- string: stripped, empty → `<key> expects a non-empty value; use 'cswap config unset <key>' to clear it`
- int/float parse failure → `<key> expects an integer, got '<v>'` / `<key> expects a number, got '<v>'`
- out of range → `<key> must be between <lo> and <hi>` (e.g. `between 50 and 99.9`)

`format_setting_value`: `None`→`(none)`; bool→`true`/`false`; integral float→no decimal (`90.0`→`90`); else `str(v)`. The JSON `value` keeps the float (`90.0`).

**Output**:
- `list` human: `<key padded to widest>  <value padded to widest>` per row, plus `  (default)` (dimmed) when not set.
- `list --json`: `{"schemaVersion": 1, "path": "<abs settings.json>", "settings": [{"key": "autoswitch.threshold", "value": 90.0, "isSet": false}, ...]}` (indent 2).
- `get` human: bare formatted value. `get --json`: `{"schemaVersion": 1, "key": "<k>", "value": <v>, "isSet": <bool>}`.
- `set`: `<key> = <formatted value>`.
- `unset`: `<key> unset (default: <formatted default>)`; when not set: stderr (muted) `<key> is not set; nothing to do`, exit 0.
- `path`: the absolute path.

Example:
```
$ cswap config set autoswitch.threshold 80
autoswitch.threshold = 80
$ cswap config get autoswitch.threshold
80
$ cswap config set autoswitch.threshold 40
Error: autoswitch.threshold must be between 50 and 99.9
$ echo $?
1
```
Errors in JSON mode use the indented envelope on stdout (`--json get bogus` → `{"schemaVersion":1,"error":{"type":"ConfigError","message":"unknown setting 'bogus'\nValid keys: ..."}}`).

`parse_model_names`: comma-split, strip, case-insensitive dedupe keeping first spelling and order (`"Opus, opus,Fable"` → `("Opus","Fable")`); shared by `auto` and `switch --strategy`. Model names are the per-model `display_name`s reported by the usage API, matched case-insensitively and exactly; `all` matches every scoped window; an unmatched name is inert (warning only).

Backup root: Linux/WSL `${XDG_DATA_HOME:-~/.local/share}/claude-swap` (`XDG_DATA_HOME` honored only when set, non-empty, absolute after `~` expansion); macOS/Windows `~/.claude-swap-backup`. On Linux/WSL a legacy `~/.claude-swap-backup` is migrated on first use with stderr `claude-swap: migrated data from <legacy> to <root>`.

---

## 12. JSON schema reference (schemaVersion 1)

`SCHEMA_VERSION = 1`, bumped only on a breaking change. All keys camelCase. The CLI serializes each payload exactly once with 2-space indent (compact for `auto`). Key order is not part of the contract; presence/absence of optional keys is.

### 12.1 Account row (`list.accounts[]`; `status.active` adds `managed`)

Always present:

| key | type | meaning |
|---|---|---|
| `number` | int | slot number |
| `email` | string | recorded email |
| `organizationName` | string | org name or `""` |
| `organizationUuid` | string | org uuid or `""` |
| `isOrganization` | bool | `organizationUuid != ""` |
| `active` | bool | is the live default login (`status` uses `managed` instead) |
| `usageStatus` | string enum | see below |
| `usage` | object \| null | populated only when `usageStatus == "ok"` |

Additive (present only when applicable):

| key | type | present when |
|---|---|---|
| `alias` | string | alias set (non-empty) |
| `disabled` | `true` | held out of rotation (absent, never `false`) |
| `usageFetchedAt` | ISO-8601 UTC `Z` | `usage` non-null and fetch time known |
| `usageAgeSeconds` | number (1 dp) | with `usageFetchedAt` |
| `lastGoodUsage` **[py]** | usage object | `usage` is null but a last-good measurement exists (too old to act on, or a non-ok state such as `token_expired`) |
| `lastGoodFetchedAt` / `lastGoodAgeSeconds` **[py]** | as above | with `lastGoodUsage` |
| `atLimit` / `limitingWindows` **[go]** | `true` / string[] | a relevant window is at/over its limit (independent of `usageStatus`) |

`usageStatus` enum: `ok`, `token_expired`, `api_key`, `keychain_unavailable`, `relogin_required`, **[py]** `foreign_credential`, `no_credentials`, `unavailable`. Internal sentinel strings → status: `"token expired"`→`token_expired`, `"api key"`→`api_key`, `"keychain unavailable"`→`keychain_unavailable`, `"re-login needed"`→`relogin_required`, `"foreign credential"`→`foreign_credential`, any other string (`"no credentials"`)→`no_credentials`, `None`→`unavailable`.

Staleness gating: JSON carries the **decision-grade** value: last-good is served as `ok` only while its age ≤ 300 s (`STALE_OK_S`) or trust-extended up to 3600 s (`TRUST_MAX_AGE_S`); beyond that the row is `usageStatus:"unavailable"`, `usage:null`, no `usageFetchedAt` (human view still shows the numbers with an age note).

### 12.2 `usage` projection

```json
{
  "fiveHour": {"pct": 25.0, "resetsAt": "2026-06-22T23:29:59Z", "countdown": "4h 12m", "clock": "02:00"},
  "sevenDay": {"pct": 16.0, "resetsAt": "...", "countdown": "1d 19h", "clock": "Jul 5 08:59",
               "expectedPct": 40.2, "aheadOfPace": false, "projectedExhaustionAt": "...Z", "willLastToReset": true},
  "spend":    {"used": 12.5, "limit": 300.0, "pct": 4.0, "currency": "USD", "resetsAt": "...", "countdown": "...", "clock": "..."},
  "scoped":   [{"name": "Fable", "pct": 100.0, "resetsAt": "...", "countdown": "3h 0m", "clock": "21:59", "expectedPct": ..., "aheadOfPace": ...}]
}
```
- Each sub-key is emitted only when the source has it. `resetsAt` is the raw ISO string; `countdown`/`clock` are **recomputed from `resetsAt` at serialization time** (fallback to cached fetch-time strings when `resetsAt` is absent/unparseable). `countdown` = `Nd Nh` / `Nh Nm` / `Nm`; `clock` local time `HH:MM` same-day else `Mon D HH:MM`.
- **[py]** pace fields (`expectedPct` 1 dp, `aheadOfPace`, optional `projectedExhaustionAt`, `willLastToReset`) appear only on weekly windows (`sevenDay`, `scoped[]`), never `fiveHour`, and only once computable/not suppressed; JSON-only.
- Headroom used by strategies = `100 − max(fiveHour.pct, sevenDay.pct[, named scoped pct])`; `spend` is excluded.

### 12.3 Other payloads

- `list`: `{"schemaVersion", "activeAccountNumber": int|null, "accounts": [...], ["duplicateAccountWarnings"], ["lockstepUsageWarnings"], ["unclaimedCredentials"]}`.
- `status`: `{"schemaVersion", "active": null | {"email","managed":false} | <managed row + "managed":true>, ["totalManagedAccounts": int]}`.
- `switch`/`switch <id>`: §7.4.
- `config list` / `config get`: §11.
- `auto` events: §10.
- Error envelope: §4.4.
- Account ref: `{"number": int|null, "email": string}`.

---

## 13. `export` / `import`

### 13.1 `export`

**Synopsis**: `cswap export <PATH> [--account NUM|EMAIL] [--full] [--debug]` (legacy `--export <path>`). `PATH` `-` = stdout.

**Semantics**: `sequence.json` missing/empty → `TransferError("no accounts to export — run cswap --add-account first")`. `--account` resolved via `_resolve_account_identifier`; unknown → `TransferError("account not found: <id>")`; a missing backup for the named account is a hard failure (`CredentialReadError("no backup credentials found for account <n> (<email>)")` / `ConfigError("no backup config found for account <n> (<email>)")`). Bulk export: slots sorted numerically; a slot with missing creds/config is skipped with stderr `Skipping Account-<n> (<email>): no stored credentials/config — re-add with: cswap --add-account --slot <n>`; all skipped → `TransferError("no exportable accounts — all managed slots are missing stored credentials/config. Re-add with: cswap --add-account --slot <number>")`. The **active** account is read from the live vault/config for the freshest tokens (`CredentialReadError("failed to read live credentials for active account <email>")`, `ConfigError("Claude config file not found")`). Missing `oauthAccount` → `TransferError("config for <email> is missing oauthAccount — cannot export")`.

**Output**: file mode → atomic write (`<path>.<pid>.tmp` + move, 0600), content = JSON indent 2 + `\n`, then stderr `Exported <N> account(s) to <path>`. Stdout mode (`-`) → JSON + `\n` on stdout, **no summary at all**; skip warnings still go to stderr.

**File format** (`.cswap`, plaintext JSON):
```json
{
  "version": 1,
  "exportedAt": "2026-01-01T00:00:00Z",
  "exportedFrom": "linux",
  "swapVersion": "0.25.0",
  "encrypted": false,
  "activeAccountNumber": 2,
  "accounts": [
    {
      "number": 1,
      "email": "alice@example.com",
      "uuid": "acct-uuid",
      "organizationUuid": "org-a",
      "organizationName": "Acme",
      "added": "2024-01-01T00:00:00Z",
      "credentials": { "claudeAiOauth": { "accessToken": "...", "refreshToken": "...", "expiresAt": 0 } },
      "config": { "oauthAccount": { ... } },
      "kind": "api_key",
      "alias": "dev"
    }
  ]
}
```
- `version` always 1. `exportedAt` UTC `%Y-%m-%dT%H:%M:%SZ`. `exportedFrom` ∈ `macos`/`linux`/`wsl`/`windows`/`unknown`. `swapVersion` = tool version. `encrypted` always `false`. `activeAccountNumber` = source active slot only if that slot is in `accounts`, else `null`.
- Per account: `number` (source slot, a hint only), `email`, `uuid`/`organizationUuid`/`organizationName`/`added` (`""` never null), `credentials` = OAuth JSON object **or** raw `sk-ant-api...` string (then `"kind": "api_key"` is present; absent for OAuth), `config` = `{"oauthAccount": ...}` by default or the entire per-account config with `--full`, `alias` only when set.
- No encryption feature; users pipe through their own tool (`cswap export - | gpg -c > backup.gpg`).

### 13.2 `import`

**Synopsis**: `cswap import <PATH> [--force] [--debug]` (legacy `--import <path>`). `PATH` `-` = stdin.

**Envelope validation** (in order): file missing → `TransferError("import file not found: <path>")`; `export file is not valid JSON: <detail>`; `export file must be a JSON object`; `unsupported export version: <v> (expected 1)` (missing version → `None`); `encrypted exports are not supported in this version — decrypt before piping (e.g. gpg -d backup.gpg | cswap --import -)`; `export file has no accounts to import`.

**Pass 1 (validate everything, zero writes)**: per account `account entry must be a JSON object`; `invalid or missing email in imported account: '<v>'`; `invalid slot number in imported account (<email>): <v>` (int ≥ 1, not bool); `<field> for <email> must be a string, got <type>`; `invalid alias for <email>: <e>`; `config for <email> must be a JSON object`; `API-key credentials for <email> must be a raw sk-ant-api… string`; `credentials for <email> must be a JSON object`; `duplicate account in export: <email> (org=<org|personal>)`; `duplicate alias in export: <alias>`; alias already used locally by a different identity → stderr `Warning: alias '<alias>' for <email> already used by an existing account, dropping the imported alias` (not fatal).

**Pass 2 (writes, envelope order)**: match on `(email, organizationUuid)`:
- exists, no `--force` → stderr `Skipped <email> (already exists, use --force)`; if that slot is dead-token quarantined, extra line `  └ currently quarantined — refresh token dead; --force replaces the backup and lifts the old verdict`. (README states a plain import replaces dead-token slots on its own — per the transfer doc the hint is printed and `--force` is needed; treat the doc as authoritative.)
- exists, `--force` → overwrite in place (`Overwrote <email> (slot <n>)`); live session on that slot → stderr `Warning: <email> (slot <n>) has a live session-mode instance (PID ...); its session profile keeps the pre-import credentials until it is restarted via 'cswap run'.`
- new → reuse the exported slot number if free locally, else `max+1`; `Imported <email> → slot <n>`.
Every written slot: credentials + config written, dead-token quarantine cleared, session profile invalidated, roster record `{email, uuid, organizationUuid, organizationName, added, [kind], [alias]}` added, `sequence` kept sorted.
- `activeAccountNumber` seeding: only when the destination has none (`null`/`0`) and the envelope's active account resolved locally.
- Summary (stderr, always): `Done: <i> imported, <o> overwritten, <s> skipped`.
- If the live login's slot was written: stderr `Note: <email> is your current live login — activate the imported credentials with: cswap --switch-to <n> --force`.

**Exit**: 0 (including all-skipped); 1 on `TransferError`/`ConfigError`/`CredentialReadError`.

Example:
```
$ cswap import backup-acct2.cswap
Skipped bob@example.com (already exists, use --force)
Done: 0 imported, 0 overwritten, 1 skipped
```

---

## 14. Remaining commands

### 14.1 Environment variables

| variable | effect |
|---|---|
| `CLAUDE_CONFIG_DIR` | Overrides the tool's config home (`<dir>/.claude.json`, `<dir>/.credentials.json`). `run`/`env` treat a preset value as an override and replace it with the session profile. For other commands a value inside `<backup_root>/sessions/` is a **session pin**: mutating commands refuse (§7.2 `SwitchError`), **[go]** read commands ignore it and print `This shell is pinned via cswap env; operating on the default login.` to stderr; a custom value outside `sessions/` is honored. |
| `XDG_DATA_HOME` | Linux/WSL backup-root parent (§11). |
| `HOME` | base for all `~` paths. |
| `WSL_DISTRO_NAME` | non-empty → platform WSL. |
| `CONTAINER`, `container` | non-empty → root guard bypassed. |
| `NO_COLOR`, `FORCE_COLOR`, `TERM` | color precedence (§4.2). |
| Auth-override vars scrubbed from `run`/`env` session environments: `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR` (not scrubbed on the same-account fast path or the default-login fallback). |

### 14.2 `tui` / `watch`

`cswap tui [--debug]` (legacy `--tui`; also bare `cswap` on a TTY) and `cswap watch [--debug]` (legacy `--watch`, opens on the live watch page). Full-screen dashboard; `--json` not applicable. Exit 0 on clean quit; 1 when stdin/stdout is not a terminal; bare `cswap` from a pipe → `no command given — try 'cswap help'` (2). Keys (Go doc): dashboard `s` switch, `w` watch, `q` quit; switch screen `enter`, `b` best, `esc`; watch `s`/`enter`/`esc`; auto screen `l` live/dry-run, `t` threshold (`←`/`→`, `enter`), `esc`. Menus: Add account…, Disable / enable account…, Remove account… (submenus, `esc`/`←` back).

### 14.3 `menubar`

`cswap menubar` (legacy `--menubar`). **[py]** macOS-only rumps app: non-macOS → stderr `The menu bar is only available on macOS.` exit 1; missing extra → `Menu bar mode requires 'rumps'. Install with: pip install 'claude-swap[menubar]'` exit 1. **[go]** always exit 1: macOS `Menu bar mode is not available in this build.`, elsewhere `The menu bar is only available on macOS.`

### 14.4 `upgrade` / `update`

`cswap upgrade` (legacy `--upgrade`). Runs before switcher construction. **[py]** detects uv (`uv tool upgrade claude-swap`) / pipx (`pipx upgrade claude-swap`) from `sys.prefix`; unknown → stderr block `Could not detect install method (looked for uv tool / pipx).\n  sys.prefix: ...\n  sys.executable: ...\nTo upgrade manually, run one of:\n  uv tool upgrade claude-swap\n  pipx upgrade claude-swap\n  <python> -m pip install --upgrade claude-swap\nIf you installed with \`pip install -e .\`, use \`git pull\` instead.` exit 1; Windows → stdout `To upgrade claude-swap on Windows, run:\n  <cmd>` exit 1 (running exe is locked); manager missing → `Detected <method> install but \`<cmd>\` is not on PATH. Run the upgrade manually from a shell where it is available.` exit 1; else subprocess exit code. **[go]** equivalent uses `go install ...@latest` / GitHub releases. For the Rust port: cargo/brew/release-download detection; keep "guidance only → exit 1" and the Windows print-only rule.

### 14.5 `purge`

`cswap purge [--debug]` (legacy `--purge`). Refuses inside a session shell; refuses when session records are unreadable (`Session records that could not be read: ... Repair or remove them, then retry --purge.`). Prints:
```
This will remove ALL claude-swap data from your system:        (warning, yellow)
  - Backup directory: <backup_root>
  - Legacy backup directory: <legacy>                            (only if distinct and present)
  - All stored account credentials (macOS Keychain and/or files) | - All stored account credential files
  - All session profiles and their Keychain entries              (only if any)

Note: This does NOT affect your current Claude Code login.      (dimmed)

Are you sure you want to purge all data? [y/N]
```
Only `y` (case-insensitive) proceeds, else `Cancelled` (dimmed), exit 0. Then `Removed:` (accent) with `  - <item>` lines, or `No claude-swap data found to remove.` (dimmed), then `Purge complete.` (accent). Passive update notice suppressed. Errors → `Error: <msg>`, exit 1.

### 14.6 `help` / `--version`

`cswap help`, `cswap -h`, `cswap --help` → full help (§2.3), exit 0. `-h`/`--help` after a pre-dispatched verb prints that verb's own help. `cswap --version` → `<prog> <version>` (e.g. `cswap 0.25.0`), exit 0.

### 14.7 Signals

SIGINT → dimmed cancel note (§4.1) + exit 130 everywhere. SIGTERM → `auto` loop stops cleanly (exit 0); other commands default action.

### 14.8 Files in the backup root (shared on-disk contract)

| path | contents |
|---|---|
| `sequence.json` | `{"activeAccountNumber": int|null, "lastUpdated": "<UTC Z>", "sequence": [sorted ints], "accounts": {"<n>": {"email","uuid","organizationUuid","organizationName","added", ["alias"], ["kind":"api_key"], ["disabled":true]}}}` |
| `settings.json` | §11 |
| `mappings.json` | §9.3 |
| `autoswitch_state.json` (+ `.autoswitch_state.lock`) | auto cooldown/quarantine state (delete to reset) |
| `configs/.claude-config-<n>-<email>.json` | per-account config snapshot |
| `credentials/.creds-<n>-<email>.enc` | per-account credential (base64) on file backends; macOS uses Keychain service `claude-swap`, account `account-<n>-<email>` |
| `sessions/<n>-<email@→_>/` | session-mode profiles |
| `cache/usage.json` (+ `.usage.lock`) | usage store, `{"schemaVersion": 2, "accounts": {...}}` |
| `cache/update_check.json` | `{"timestamp", "data"}` |
| `.lock` | account lock (flock, 10 s timeout) |
| `claude-swap.log[.1-3]` | rotating log |

---

## 15. Claude-specific, needs a Codex mapping or omission

Everything below is bound to Claude Code / Anthropic and must be re-mapped to OpenAI Codex equivalents (or dropped) in `cswitch`:

1. **Live login files**: `~/.claude.json` (global config; `oauthAccount{emailAddress, accountUuid, organizationUuid, organizationName}` is the identity) and `~/.claude/.credentials.json` (`claudeAiOauth{accessToken, refreshToken, expiresAt, scopes}`), plus the legacy `<config_home>/.config.json`; env override `CLAUDE_CONFIG_DIR`. → Codex: `~/.codex/auth.json` (+ `CODEX_HOME`), whatever identity fields Codex stores.
2. **macOS Keychain** as the credential backend (service `Claude Code-credentials` for the live login; `claude-swap` for backups; `keychain_unavailable` usageStatus and its human line; `~30 seconds` Keychain-cache followup line; Keychain retry constants; purge/list wording mentioning Keychain). → Codex stores `auth.json` on disk on every platform, so the file-backend followup line (`no restart needed`) likely applies everywhere unless Codex caches; decide on the restart-semantics line for the app-server daemon (the repo already has a "restart the Codex app-server daemon after a credential switch" commit).
3. **Claude Code advisory locks** (`~/.claude.lock`, `~/.claude.json.lock` directory locks, proper-lockfile protocol, `ClaudeCodeLockTimeout`). → Codex has no equivalent lock protocol; keep the backup-root lock, rename/omit the error type or keep the string for envelope compatibility.
4. **Usage API** (`https://api.anthropic.com/api/oauth/usage`, `anthropic-beta: oauth-2025-04-20`, 5h/7d windows, per-model `scoped` weekly windows, `spend`), OAuth profile endpoint, token refresh at `platform.claude.com`, `OAUTH_EXPIRY_BUFFER_MS`. → Codex rate-limit windows (5h / weekly) come from a different endpoint/shape; the `fiveHour`/`sevenDay`/`scoped`/`spend` JSON keys, `--model` folding, `at <label> limit` labels and model display names (`Fable`, `Opus`, `Sonnet`, `Haiku`) need remapping. Pace fields likewise.
5. **Token kinds**: setup-token (`sk-ant-oat01-...`, from `claude setup-token`), managed API key (`sk-ant-api03-...`, `kind: api_key`, `API key (no quota)`), `user:inference` scope, `add-token` synthesized emails `setup-token-<n>@token.local` / `api-key-<n>@token.local`, `_reject_live_api_key_capture`. → Codex has API-key auth (`OPENAI_API_KEY`) vs ChatGPT OAuth; decide whether `add-token` survives and what prefixes identify each.
6. **Session mode** (`cswap run`/`env`): execs `claude`, `CLAUDE_CONFIG_DIR` pin, shares `settings.json`/`keybindings.json`/`CLAUDE.md`/`skills`/`commands`/`agents`, history = `projects/` + `history.jsonl`, user-scope MCP mirroring (`claude mcp add -s user`), `authMethod == "claude.ai"` validation, `.cswap-stale-credentials` sentinel, forwarded example `-- --resume`. → Codex: exec `codex`, pin `CODEX_HOME`, share `config.toml`/`AGENTS.md`/skills, history `sessions/` + `history.jsonl`; re-evaluate what is worth sharing.
7. **Process detection** (`Running instances:` block; entrypoints `cli`, `claude-vscode`, `claude-desktop`, `sdk-*`, `mcp`, `local-agent`, `remote`; IDE `Visual Studio Code`; IDE lock files) and the "owner running" gating of token refresh (`_active_cc_running`). → Codex CLI / VS Code extension / app-server processes; likely a reduced version or omission.
8. **Auth-override env vars** scrubbed in session mode (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`). → `OPENAI_API_KEY`, `CODEX_API_KEY`, etc.
9. **VS Code extension / Desktop / Chrome mentions** in README tips ("reopen the VS Code extension tab") and in the process labels. → Codex VS Code extension / Codex app.
10. **macOS menu bar app** (`menubar`, rumps, `claude-swap[menubar]` extra). → omit (Go did) or keep as exit-1 stub.
11. **Self-upgrade / update check** via PyPI + uv/pipx (`https://pypi.org/pypi/claude-swap/json`). → cargo/brew/GitHub releases.
12. **Names/paths**: program name `cswap`/`claude-swap`, backup roots `~/.claude-swap-backup` / `claude-swap` XDG dir, log `claude-swap.log`, Keychain service `claude-swap`, export extension `.cswap`, `swapVersion` field, error-type strings (`ClaudeSwitchError`, `ClaudeCodeLockTimeout`), strings like `Claude config file not found`, `Restart Claude Code ...`, `log in with Claude Code`, `No active Claude account`, `'claude' was not found on PATH. Install Claude Code first.`, help header `Multi-Account Switcher for Claude Code`. → rename to `cswitch`/Codex equivalents; keep `schemaVersion` and field names stable where scripts might be shared.
13. **Org identity** (`organizationUuid`/`organizationName`, `[personal]` tag, ambiguous-email-across-orgs handling). → Codex accounts may carry a workspace/org id; keep the composite identity concept.
14. **`/login` / `/logout` advice** (README: do not `/logout` before `add`, recovery is `/login` + `cswap add`). → Codex `codex login` / `codex logout`.
15. **Identity oracle / unclaimed stash** (`foreign credential` status, `_classify_outgoing_credential`, `unclaimed` command) depends on the OAuth profile endpoint to resolve who a live token belongs to. → needs a Codex profile call or a simpler fingerprint-only design.
16. **TLS truststore workaround** for `platform.claude.com` on Windows. → not needed in Rust (`rustls-native-certs` / native TLS).

---

## 16. Discrepancies between the Python source (v0.25.0) and the Go docs

| topic | Python **[py]** | Go docs **[go]** |
|---|---|---|
| `autoswitch.strategy` choices | `best`, `consume-first` (`auto --strategy` flag exists) | `best`, `soonest-reset` (no `auto --strategy` flag in reference.md synopsis); 08-cli-infra lists `("best",)` only |
| settings key count | 9 (adds `ui.theme`) | 8 |
| `list --json` extras | `duplicateAccountWarnings`, `lockstepUsageWarnings`, `unclaimedCredentials`, `lastGood*`, pace fields, `foreign_credential` status | `atLimit`, `limitingWindows` |
| `list` human | no at-limit marker; `SENTINEL_NOTES` token-expired text `token expired — refresh deferred this pass; retries automatically` | `at limit: <windows>` marker; `token expired — Claude Code refreshes the active account` |
| `env` command | absent | present |
| `unclaimed` command | present | absent |
| `menubar` | real app (macOS + rumps) | stub, always exit 1 |
| `upgrade` | uv/pipx | `go install` |
| `remove` output | `Removed Account-<n> (<email>)` | `Removed Account-<n> (<email>).` |
| `SwitchEvent.trigger` | adds `consume-first` | `proactive`/`at-limit`/`failover` |
| `map` list path | normalized absolute key | `~`-abbreviated in example |
| Corrupt-roster refusal (`ConfigError` naming the file) | not verified in Python source | documented for alias/run/env/map/export |

Recommendation for `cswitch`: follow the Python source for grammar, messages, settings and JSON (it is the living reference), adopt the Go additions that are clearly useful and additive (`env`, `atLimit`/`limitingWindows`, corrupt-roster refusal), and keep the `--json` schemaVersion-1 rule: only add optional keys, never change existing ones.
