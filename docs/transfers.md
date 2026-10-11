# Export, import, and migration

[Back to overview](../README.md)

Move saved accounts between machines or bring an existing claude-swap roster into ccsw.

## Export and import

```bash
ccsw export backup.ccsw
ccsw export work.ccsw --account 2
ccsw import backup.ccsw
ccsw import backup.ccsw --force
ccsw import backup.cswap
```

`ccsw export` writes a version-2 `.ccsw` file with every Codex and Claude account
(`--account` limits it to one). The active account of each provider is exported from its
live login when that is the same account, and a token Claude Code rotated inside a session
profile is folded into the slot first, so an export can refresh a stored snapshot.
`ccsw import` reads those files, version-1 `.ccsw` files from earlier releases (all Codex)
and `cswap` exports (`.cswap`, all Claude Code), matching accounts on provider, email and
organization; `--force` overwrites matches in place and a quarantined dead-token slot is
replaced without it. Importing over a Claude account whose session profile is running keeps
that session on its old login until it restarts. Exports hold credentials in plain JSON:
keep them private.

## Migrate a claude-swap store

```bash
ccsw import --from-cswap
ccsw import --from-cswap /path/to/store --retire
ccsw import --from-cswap --json
```

`ccsw import --from-cswap` reads a claude-swap store in place, with no export step: the
roster from `sequence.json`, each account's credentials from the macOS Keychain (service
`claude-swap`) or its base64 `.enc` file, and the `.claude.json` snapshot from `configs/`.
`DIR` defaults to claude-swap's location (`~/.claude-swap-backup`, or
`$XDG_DATA_HOME/claude-swap` on Linux). Reads are `.enc`-wins, as in claude-swap: the
Keychain item is consulted only when the file is absent or corrupt. Add `--retire` to
rename the store to `<dir>.migrated-<stamp>` after a successful run (every readable account
it held is in ccsw by then, including ones that were already managed), so a leftover
claude-swap cannot keep refreshing the same tokens. An account whose credentials cannot be
read is announced on stderr (`Skipping Account-N …`) and counted as skipped; `--json` prints
the report (`imported`, `overwritten`, `skipped`, `replaced`, `retired`). Exit 2 means there
was nothing to import (no store, an empty roster, or an already-migrated one).
