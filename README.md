# cswitch

Multi-account switcher for the [OpenAI Codex CLI](https://github.com/openai/codex).
Keep several Codex logins on one machine, switch between them without logging in again,
watch every account's 5-hour and weekly usage in a live dashboard, let it switch for you
before you hit a rate limit, and run two accounts side by side in different terminals.

`cswitch` is [claude-swap (`cswap`)](https://github.com/realiti4/claude-swap) for Codex:
the commands, options, JSON output, settings and full-screen dashboard follow `cswap`,
so `cswap list` becomes `cswitch list`. The Codex-specific mechanics (credential file,
identity, usage API, token refresh, app-server daemon) come from
[codex-switch](https://github.com/xjoker/codex-switch). Both are MIT licensed; see
`NOTICE`.

> `cswitch` manages local credential files. Never publish `~/.cswitch`, `auth.json`,
> tokens, or unredacted debug output.

## Install

Requires Rust 1.88 or newer until binaries are published:

```bash
cargo install --git https://github.com/newbdez33/cswitch --locked
```

Codex must keep its credentials in `auth.json` (the default). If your
`~/.codex/config.toml` sets `cli_auth_credentials_store`, it must be `"file"`.

## Usage

### Add your first account

Log into Codex as usual (`codex login`), then snapshot that login:

```bash
cswitch add
```

### Add more accounts

Log in with the next account and run `cswitch add` again. Do **not** run
`codex logout` first: it can revoke the refresh token that `cswitch` just saved. Just run
`codex login` for the next account, then `cswitch add`.

```bash
codex login
cswitch add
cswitch add --alias work        # give it a short name
```

An API key can be registered without touching the current login:

```bash
cswitch add-token sk-...                        # OpenAI API key
cswitch add-token - --slot 3 < key.txt          # read the key from stdin
```

### Switch accounts

```bash
cswitch switch                  # rotate to the next account
cswitch switch 2                # by slot number
cswitch switch user@example.com # by email
cswitch switch work             # by alias
cswitch switch --strategy best  # the account with the most quota left
cswitch switch --strategy next-available   # rotate, skipping rate-limited accounts
```

Codex 0.157 and newer runs interactive sessions through a shared local app-server
daemon that loads `auth.json` once. When that daemon is running, `cswitch switch`
restarts it (`codex app-server daemon restart`) so new and reconnecting Codex sessions use
the selected account; a turn that was in progress is interrupted. `codex exec` and
`codex --no-daemon` sessions read the file when they start, so restart those yourself.

### See every account's usage

```bash
cswitch list        # 5h / 7d / per-model pools with reset times
cswitch status      # the active account
cswitch list --json # for scripts
```

### Automatic switching

```bash
cswitch auto                    # foreground loop, switch at 90% used
cswitch auto --threshold 80     # switch earlier
cswitch auto --once             # one tick, outcome in the exit code (0 switched, 1 error, 2 nothing to do, 3 blocked)
cswitch auto --dry-run          # log what it would do, never switch
cswitch auto --json             # one JSON event per line
```

Defaults live in `settings.json`; change them with `cswitch config set autoswitch.threshold 80`.

### Run two accounts at once (session mode)

```bash
cswitch run 2                   # start Codex as account 2 in this terminal only
cswitch run work -- resume      # everything after -- goes to codex
cswitch map 2 ~/work/client     # bare `cswitch run` in that tree uses account 2
eval "$(cswitch env 2)"         # pin this shell to account 2 without starting Codex
```

Session mode gives the account a private `CODEX_HOME` under `~/.cswitch/sessions/`,
shares your `config.toml`, `AGENTS.md`, `prompts/` and `skills/`, and keeps each
account's session history separate (`--share-history` shares it).

### Dashboard (TUI)

```bash
cswitch          # or: cswitch tui
cswitch watch    # straight to the live monitor
```

### Other commands

```bash
cswitch remove 2
cswitch disable 2 / cswitch enable 2     # hold out of / return to auto-rotation
cswitch alias 2 dev / cswitch alias 2 --unset / cswitch alias
cswitch move 2 1 / cswitch swap 1 2
cswitch config [get|set|unset KEY [VALUE]|path]
cswitch export backup.cswitch [--account 2] / cswitch import backup.cswitch [--force]
cswitch purge
```

The `cswap` flag spellings (`cswitch --list`, `cswitch --switch-to 2`, …) keep working.

## Data locations

Everything cswitch stores lives in `~/.cswitch` (override with `CSWITCH_HOME`):
the account roster (`sequence.json`), credential snapshots (`credentials/`),
`settings.json`, directory mappings, the usage cache, auto-switch state, session
profiles and a rotating log. The live Codex login is `$CODEX_HOME/auth.json`
(default `~/.codex/auth.json`); `cswitch` backs it up as `auth.json.bak.<timestamp>`
(three kept) before every switch.

## Development

```bash
cargo build
cargo test --all
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Design: `docs/specs/2026-09-29-cswitch-design.md`. Plan: `docs/plans/`. The research
notes that pin the `cswap` contract and the Codex mechanics are in `docs/research/`.

## License

MIT. See `LICENSE` and `NOTICE` for the claude-swap and codex-switch attributions.
