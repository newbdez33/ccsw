//! Everything that touches Claude Code: config files, the macOS Keychain,
//! Claude Code's lock protocol, token refresh and the usage API (spec
//! `docs/specs/2026-10-07-cswitch-claude-provider-design.md` §4).

pub mod credentials;
pub mod keychain;
pub mod live;
pub mod locks;
pub mod oauth;
pub mod usage;
