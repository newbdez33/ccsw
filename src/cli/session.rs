//! `cswitch run`, `env`, `map`, `unmap` — each parses its own arguments. (Task E)

/// `argv` excludes the program name and the verb. Returns the exit status.
pub fn run_cmd(argv: Vec<String>) -> i32 {
    let _ = argv;
    eprintln!("cswitch run: not implemented yet");
    1
}

pub fn env_cmd(argv: Vec<String>) -> i32 {
    let _ = argv;
    eprintln!("cswitch env: not implemented yet");
    1
}

pub fn map_cmd(argv: Vec<String>) -> i32 {
    let _ = argv;
    eprintln!("cswitch map: not implemented yet");
    1
}

pub fn unmap_cmd(argv: Vec<String>) -> i32 {
    let _ = argv;
    eprintln!("cswitch unmap: not implemented yet");
    1
}
