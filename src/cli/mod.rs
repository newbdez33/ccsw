//! Command-line front controller: grammar, legacy flags, validation, dispatch. (Task D)
//!
//! Pre-dispatched verbs own their own parsers, as in cswap: `auto` lives in
//! `cli::auto` (Task E), `run` / `env` / `map` / `unmap` in `cli::session`
//! (Task E), and `tui` / `watch` in `cli::tui` (Task F).

pub mod auto;
pub mod session;
pub mod tui;

/// Entry point used by `main`; returns the process exit status.
pub fn run() -> i32 {
    eprintln!("cswitch: command layer not implemented yet");
    2
}
