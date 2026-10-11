# Development

[Back to overview](../README.md)

Build, test, and maintain ccsw from a source checkout.

## Build from source

Use Rust 1.88 or newer. From the branch or tag you want to use:

```bash
cargo build --locked
cargo run -- serve
```

To install that checkout on your `PATH`:

```bash
cargo install --path . --locked
```

## Checks

```bash
cargo build --locked
env -u CODEX_HOME cargo test --all
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The integration tests run the built binary against fake `codex` and `claude` executables
and a local mock of the usage and token endpoints (`CCSW_USAGE_URL`, `CCSW_TOKEN_URL`, and
`CCSW_CLAUDE_USAGE_URL`, `CCSW_CLAUDE_TOKEN_URL` for Claude); tests set
`CCSW_KEYCHAIN=off`, and `CCSW_CLAUDE_LOCK_BUDGET_MS` shortens the Claude lock
wait. These tests use isolated homes and local mocks instead of your provider accounts.
Changes are listed in the [changelog](../CHANGELOG.md).

## Release maintenance

Tagging `v*` builds archives for macOS, Linux, and Windows through the
[release workflow](https://github.com/newbdez33/ccsw/blob/main/.github/workflows/release.yml).
The archives include this overview, user guides, and screenshots.

When preparing a release, update `Cargo.toml`, `Cargo.lock`, the changelog, and
the versioned examples in [Upgrade](installation.md#upgrade). The
`docs_release` test checks the upgrade examples against the crate version.
Update the Claude Code fallback version in `src/claude/usage.rs` and remove
unreleased notices for features included in the release.

## Design notes

The repository keeps [specifications](https://github.com/newbdez33/ccsw/tree/main/docs/specs),
[implementation plans](https://github.com/newbdez33/ccsw/tree/main/docs/plans), and
[research notes](https://github.com/newbdez33/ccsw/tree/main/docs/research)
separate from the user guides.

## Origins

`ccsw` is a Rust port of [claude-swap (`cswap`)](https://github.com/realiti4/claude-swap)
that manages Claude Code and Codex accounts in one roster: the commands, options, JSON
output, settings, export format and full-screen dashboard follow `cswap`, so `cswap list`
becomes `ccsw list` and a `.cswap` export imports as is.

The Claude Code mechanics (Keychain and credentials file, `oauthAccount` identity, Claude
Code's lock files, OAuth refresh, the usage API and
[session profiles](https://github.com/realiti4/claude-swap/blob/3a4e5c14873eb5b32f182d55c68da98ac8c0db45/src/claude_swap/session.py))
are ported from `cswap` at commit `3a4e5c1`, with the credential and process guards adapted
to Rust and the shared store. The Codex mechanics (credential file, identity, usage API,
token refresh, app-server daemon) come from
[codex-switch](https://github.com/xjoker/codex-switch). Both are MIT licensed; see
[NOTICE](../NOTICE).
