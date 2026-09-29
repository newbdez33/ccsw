//! `cswitch auto` — parses its own arguments (everything after the verb).

/// `argv` excludes the program name and the `auto` verb. Returns the exit status.
pub fn run(argv: Vec<String>) -> i32 {
    match crate::switcher::Switcher::from_env() {
        Ok(mut switcher) => crate::autoswitch::run_cli(argv, &mut switcher),
        Err(err) => {
            eprintln!("Error: {err}");
            1
        }
    }
}
