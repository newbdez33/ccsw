# Installation and upgrades

[Back to overview](../README.md)

Install a release binary or build ccsw from source.

## Install

Download an archive from the [releases page](https://github.com/newbdez33/ccsw/releases),
extract it, and put `ccsw` on your `PATH`.

| Platform | Target |
| --- | --- |
| macOS, Apple Silicon | `aarch64-apple-darwin` |
| macOS, Intel | `x86_64-apple-darwin` |
| Linux, x86_64 | `x86_64-unknown-linux-gnu` |
| Linux or WSL, x86_64, static build | `x86_64-unknown-linux-musl` |
| Windows, x86_64 | `x86_64-pc-windows-msvc` |

Choose the static musl build if your Linux distribution has an older glibc.
Releases include `SHA256SUMS` for the archives. To install from source, use
Rust 1.88 or newer:

```bash
cargo install --git https://github.com/newbdez33/ccsw --locked
```

For a local branch or an unreleased feature, [build that checkout](development.md#build-from-source).

## Provider setup

Codex must keep its credentials in `auth.json` (the default). If your
`~/.codex/config.toml` sets `cli_auth_credentials_store`, it must be `"file"`.

Claude Code's login is read from the macOS Keychain (service "Claude Code-credentials") or
from `~/.claude/.credentials.json`, and the account identity from `~/.claude.json`;
`CLAUDE_CONFIG_DIR` is honoured. Set `CCSW_KEYCHAIN=off` to use the file backend only.

## Upgrade

`ccsw --version` prints the installed version; the [CHANGELOG](../CHANGELOG.md) lists what
each release changed. Upgrading never touches `~/.ccsw`: accounts, settings and the usage
cache carry over. Stop a running dashboard, `ccsw watch`, `ccsw auto`, or
`ccsw serve` before replacing the binary.

Release binary — fetch the current release and replace the `ccsw` already on your `PATH`
(`SHA256SUMS` next to the archives lists their checksums):

```bash
VERSION=v0.9.0; TARGET=aarch64-apple-darwin   # or x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl
curl -fsSL "https://github.com/newbdez33/ccsw/releases/download/$VERSION/ccsw-$VERSION-$TARGET.tar.gz" | tar xz
install "ccsw-$VERSION-$TARGET/ccsw" "$(command -v ccsw)"
```

On Windows, download `ccsw-v0.9.0-x86_64-pc-windows-msvc.zip` from the
[releases page](https://github.com/newbdez33/ccsw/releases) and replace `ccsw.exe`.

From source:

```bash
cargo install --git https://github.com/newbdez33/ccsw --tag v0.9.0 --locked --force
```

`ccsw upgrade` only prints these instructions; ccsw does not update itself.
