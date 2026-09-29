//! cswitch — multi-account switcher for the OpenAI Codex CLI.
//!
//! Layering (see `docs/specs/2026-09-29-cswitch-design.md` §14): `cli` and `tui`
//! sit on the `switcher` façade and the `autoswitch` engine; those sit on the
//! `store` and `codex` layers; everything shares `model`, `paths`, `fsutil`,
//! `errors`, `printer`, and `jsonout`.

pub mod errors;
pub mod fsutil;
pub mod model;
pub mod paths;

pub mod jsonout;
pub mod printer;

pub mod codex;
pub mod store;

pub mod autoswitch;
pub mod collect;
pub mod session;
pub mod switcher;
pub mod transfer;

pub mod cli;
pub mod tui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
