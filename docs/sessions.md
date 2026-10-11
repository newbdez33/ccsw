# Session profiles

[Back to overview](../README.md)

Run different Codex or Claude Code accounts in separate terminals, with a persistent
profile for each account.

## Launch and map accounts

```bash
ccsw run 2                       # launch the provider that owns account 2
ccsw run work -- --resume        # pass arguments to the selected CLI
ccsw run 2 --require-session     # refuse a plain default-login launch
ccsw map 2 ~/work/client         # use account 2 in this directory tree
ccsw run claude                  # use this provider's nearest directory mapping
ccsw unmap ~/work/client claude  # remove one provider's mapping
ccsw unmap ~/work/client         # remove both providers' mappings
eval "$(ccsw env 2)"             # pin this shell without starting the CLI
eval "$(ccsw env claude --unset)" # unpin one provider
eval "$(ccsw env --unset)"        # unpin both (--shell fish|pwsh also supported)
```

Each account has a persistent profile under `~/.ccsw/sessions/`. Codex uses
`CODEX_HOME`; Claude Code uses `CLAUDE_CONFIG_DIR` and a separate macOS Keychain
item. An account number or alias selects its provider. A directory can map one
account per provider; when both apply, pass `codex`, `claude`, or an account.

## Shared settings and history

Codex copies `config.toml` and shares `AGENTS.md`, `prompts/`, and `skills/`.
Claude Code shares `settings.json`, `keybindings.json`, `CLAUDE.md`, `skills/`,
`commands/`, and `agents/` from the default `~/.claude` directory. These are
symlinks on macOS/Linux and copies updated at launch on Windows. `--no-share`
removes managed shares and keeps local profile files.

History stays separate by default. `--share-history` shares it on macOS/Linux;
for Claude Code, existing profile transcripts and prompt history are merged
before linking `projects/` and `history.jsonl`. This flag is independent of
`--no-share`. Windows does not support history sharing.

## Login and profile lifecycle

Like cswap, selecting the active default login without a preset provider home
launches the CLI directly. `--require-session` refuses that fast path. Isolated
Claude sessions require an OAuth login or setup token; managed API keys are not
supported. Authentication override variables are removed for an isolated launch.

Claude Code owns token refresh while its profile is running. ccsw reads rotated
profile credentials before later usage, refresh, switching, and launch commands;
Windows also captures them after the child exits. A running profile cannot be
switched into the default login, removed, or moved. Unreadable credentials or PID
records block those operations until they can be checked. If `/login` changes a
profile's identity, save that login separately before reusing its original slot.
`run` reserves the profile before the CLI starts and waits for any in-flight
credential refresh. `env` only prepares the profile and prints shell commands;
it does not reserve the shell. After a direct CLI launch, ownership checks depend
on the CLI's PID records. Prefer `run` when other commands can change the account.
Saving a new login invalidates the old profile credentials after its sessions
close. `purge` also refuses running profiles and removes their managed Keychain items.
