//! `ccsw auto` — parses its own arguments (everything after the verb).

use crate::switcher::{SilentUi, Switcher};

/// `argv` excludes the program name and the `auto` verb. Returns the exit status.
pub fn run(argv: Vec<String>) -> i32 {
    match Switcher::from_env() {
        Ok(mut switcher) => {
            crate::logging::init(&switcher.store.paths, argv.iter().any(|a| a == "--debug"));
            // The engine reports every switch itself (warnings ride in the
            // `switch` event), and JSON mode owns stdout.
            switcher.ui = Box::new(SilentUi);
            crate::autoswitch::run_cli(argv, &mut switcher)
        }
        Err(err) => {
            eprintln!("Error: {err}");
            1
        }
    }
}
