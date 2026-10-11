# Account and CLI guide

[Back to overview](../README.md)

Save and switch accounts, inspect quota, and automate Claude Code switching.
Complete the [provider setup](installation.md#provider-setup) first.

## Add your first account

Run `ccsw` and open **Add account…**. **From current logins** saves the logins Codex and
Claude Code already hold. **Add new account** signs a new Codex account in through your
browser and activates it (the Codex CLI must be on your `PATH`; press `Esc` to cancel).
**From a token…** registers an OpenAI API key or an Anthropic setup-token / API key.

From the command line, `ccsw add` snapshots the logins you already have:

```bash
ccsw add            # the current Codex login and the current Claude Code login
ccsw add claude     # only the Claude Code login
ccsw add codex      # only the Codex login
```

`ccsw add` captures whichever of the two logins exist.

## Add more accounts

For Codex, select **Add account… → Add new account** again and sign in with the next
account. The login runs in a temporary Codex home, so it does not revoke the previous login.
Signing into a managed account again updates its stored credentials in place.

Do **not** run `codex logout` first: it can revoke a saved refresh token. Recent
Codex versions can also clear the previous login when you run `codex login`
directly. Use the dashboard to add another account.

For Claude Code, log in with `claude` (`/login`), then run `ccsw add claude`. Do not
run `/logout` first.

Use `ccsw alias 2 work` to give a saved account a short name.

An API key can be registered without touching the current login:

```bash
ccsw add-token sk-...                        # OpenAI API key
ccsw add-token - --slot 3 < key.txt          # read the key from stdin
ccsw add-token sk-ant-oat01-...              # Anthropic setup-token
ccsw add-token sk-ant-api03-...              # Anthropic API key
```

## Switch accounts

```bash
ccsw switch                  # rotate to the next account (say codex|claude when both are managed)
ccsw switch 2                # by slot number
ccsw switch 5                # by slot: slots are one list across Codex and Claude
ccsw switch claude           # rotate among the Claude accounts
ccsw switch claude --strategy best
ccsw switch user@example.com # by email
ccsw switch work             # by alias
ccsw switch --strategy best  # the account with the most quota left
ccsw switch --strategy next-available   # rotate, skipping rate-limited accounts
```

A running Claude Code picks up the switched login after about 30 seconds when its
credentials live in the Keychain, and immediately when they live in the file.

Codex 0.157 and newer runs interactive sessions through a shared local app-server
daemon that loads `auth.json` once. When that daemon is running, `ccsw switch`
restarts it (`codex app-server daemon restart`) so new and reconnecting Codex sessions use
the selected account; a turn that was in progress is interrupted. `codex exec` and
`codex --no-daemon` sessions read the file when they start, so restart those yourself.

## See every account's usage

```bash
ccsw list                # a Codex block and a Claude block: 5h / 7d / per-model pools with reset times
ccsw list claude         # only the Claude accounts
ccsw status              # the active account of each provider
ccsw list --token-status # add stored-token expiry diagnostics
ccsw list --fetch-all    # measure every stale account in this one pass
```

Usage is fetched on demand (the active account plus one other account per command) and
cached for three minutes, so run `ccsw list` again to fill in the remaining rows, or pass
`--fetch-all` to measure every stale row at once. Each account's poll budget still applies.
Claude rows add a `$$` line for extra-usage spend and per-model windows such as
`Fable: 62%`. The dashboard shows an account's saved limit resets (Codex's reset credits,
Claude Code's `/limit-reset` grants) as a red heart with a green count (`♥ n`); when a credit or grant
has an end date, the soonest one follows as `(in 10d)`. Anthropic lists those grants only
for the Claude Code CLI, so Claude usage requests identify as the installed Claude Code
(`claude-cli/<version> (external, cli) ccsw/<version>`, the version read from the `claude`
on your `PATH`).

## JSON output for scripting

`--json` works with `list`, `status` and `switch`; stdout carries exactly one JSON
document (`schemaVersion: 2`) and nothing is printed to stderr. A handled error becomes
`{"schemaVersion": 2, "error": {"type": "...", "message": "..."}}` with exit 1.

Account rows and switch references carry `provider`. `active` is
`{"codex": n, "claude": n}` on `list` and a per-provider object on `status`.

```bash
ccsw list --json                     # accounts[] with usage.fiveHour / sevenDay / scoped[], plus credits (Codex) or spend (Claude)
ccsw list claude --json --fetch-all  # collectors: every stale row measured in this pass (not one candidate per call); `claude` filters the output
ccsw status --json                   # {"active": {"codex": row|null, "claude": row|null}}
ccsw switch --strategy best --json   # {"switched": true, "from": ..., "to": ..., "reason": "switched"}
ccsw auto --once --json              # one compact JSON event per line
```

## Automatic switching (Claude Code)

```bash
ccsw auto                    # foreground loop, switch Claude Code accounts at 90% used
ccsw auto --threshold 80     # switch earlier
ccsw auto --once             # one tick, outcome in the exit code (0 switched, 1 error, 2 nothing to do, 3 blocked)
ccsw auto --dry-run          # log what it would do, never switch
ccsw auto --json             # one JSON event per line (schemaVersion 2, provider "claude")
```

Auto-switch covers Claude Code accounts only. A running Claude Code session picks the new
login up by itself (on the next message, or within about 30 seconds with the macOS Keychain),
so switching early keeps you working. Codex sessions keep the account they started with
until they restart — Codex CLI's app-server daemon loads `auth.json` once and re-reads it
only for the account it already holds — so there is no Codex auto-switch; use `ccsw
switch` and restart the session. `ccsw auto` on a roster without a Claude Code account
exits 1 and says so. While the Keychain is locked, the engine holds (`active-idle`) rather
than failing over.

Defaults live in `settings.json`; change them with `ccsw config set autoswitch.threshold 80`.
`autoswitch.model` names that no account reports produce one `config-warning` event.

## Dashboard (TUI)

```bash
ccsw          # or: ccsw tui
ccsw watch    # straight to the live monitor
```

The dashboard shows the active account as a card with 5h, 7d and per-model bars, the
other accounts as one-line summaries, and the menu; when the terminal is short the
accounts stay whole and the menu scrolls, keeping its title and the highlighted entry
in view. Rows wider than the terminal wrap onto the next line, as in `cswap`. Accounts
that still do not fit scroll behind a scrollbar (mouse wheel or `PgUp`/`PgDn`), and so do
the switch and watch lists. These screens list a `codex` section and then a
`claude` section when both are present. `ccsw watch` shows every account as a
live card. **Remote Console** starts a browser console, generates pairing links,
and opens the browser with automatic pairing. See the
[console guide](remote-console.md#from-the-tui) and
[dashboard screenshot](../README.md#terminal-dashboard).
The live monitor is shown below.

<img src="tui-watch.png" width="760" alt="ccsw watch showing live quota, reset times, and the active account for each provider">

## Account maintenance

```bash
ccsw remove 2
ccsw disable 2
ccsw enable 2
ccsw alias 2 dev
ccsw alias 2 --unset
ccsw alias
ccsw move 2 1
ccsw swap 1 2
ccsw config list
ccsw config get autoswitch.threshold
ccsw config set autoswitch.threshold 80
ccsw config unset autoswitch.threshold
ccsw config path
ccsw purge  # Remove all ccsw data
```

Disabled accounts stay in the roster but are excluded from automatic rotation.
Every command accepts `--help`; `ccsw help` lists all commands. The `cswap` flag
spellings (`ccsw --list`, `ccsw --switch-to 2`, and others) also work.

## Data locations

Everything ccsw stores lives in `~/.ccsw` (override with `CCSW_HOME`):
the account roster (`sequence.json`), credential snapshots (`credentials/`),
`settings.json`, directory mappings, the usage cache, auto-switch state, session
profiles and a rotating log (`ccsw.log`, 1 MiB, three backups; `--debug` mirrors it
to stderr). The live Codex login is `$CODEX_HOME/auth.json` (default
`~/.codex/auth.json`); `ccsw` backs it up as `auth.json.bak.<timestamp>` (three
kept) before every switch. The outgoing Claude login is backed up under `backups/claude/`
(three kept). During a switch ccsw creates and removes short-lived lock directories
inside `~/.claude`. `CLAUDE_CONFIG_DIR` moves the live Claude location and
`CCSW_KEYCHAIN=off` skips the Keychain.

Keep `~/.ccsw`, login files, tokens, and unredacted debug output private.
