//! Command-line front controller: grammar, legacy flags, validation, dispatch.
//!
//! Pre-dispatched verbs own their own parsers, as in cswap: `auto` lives in
//! `cli::auto` (Task E), `run` / `env` / `map` / `unmap` in `cli::session`
//! (Task E), `config` in `cli::config` (Task T), `tui` / `watch` in
//! `cli::tui` (Task F); `alias`, `swap` and `move` are in `cli::misc`.

pub mod accounts;
pub mod auto;
pub mod config;
pub mod legacy;
pub mod list;
pub mod misc;
pub mod session;
pub mod switch;
pub mod tui;

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::errors::{CswitchError, Result};
use crate::jsonout;
use crate::logging;
use crate::paths::Paths;
use crate::printer;
use crate::switcher::{SilentUi, Switcher};

use legacy::{Command, PROG, USAGE_LINE};
use tui::TuiStart;

/// Set in `--json` mode so the Ctrl-C note goes to stderr.
static JSON_MODE: AtomicBool = AtomicBool::new(false);

/// Entry point used by `main`; returns the process exit status.
pub fn run() -> i32 {
    run_with(std::env::args().skip(1).collect())
}

/// The whole dispatch for an argv without the program name.
pub fn run_with(argv: Vec<String>) -> i32 {
    if let Some(first) = argv.first() {
        let rest = argv[1..].to_vec();
        match first.as_str() {
            "run" => return session::run_cmd(rest),
            "env" => return session::env_cmd(rest),
            "auto" => return auto::run(rest),
            "config" => return config::run(rest),
            "map" => return session::map_cmd(rest),
            "unmap" => return session::unmap_cmd(rest),
            "alias" => return misc::alias_cmd(rest),
            "swap" => return misc::swap_cmd(rest),
            "move" => return misc::move_cmd(rest),
            _ => {}
        }
    }
    let argv =
        if argv.is_empty() && std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
            vec!["--tui".to_string()]
        } else {
            argv
        };
    let argv = legacy::translate(argv);
    let opts = match legacy::parse(&argv) {
        Ok(opts) => opts,
        Err(message) => return usage_error(&message),
    };
    if opts.help {
        print!("{}", legacy::help_text());
        return 0;
    }
    if opts.version {
        println!("{}", legacy::version_line());
        return 0;
    }
    if let Err(message) = legacy::validate(&opts) {
        return usage_error(&message);
    }
    let command = opts.command.clone().expect("validated");
    if command == Command::Upgrade {
        return misc::upgrade();
    }
    let json = opts.json;
    with_switcher(opts.debug, json, |switcher| match command {
        Command::Menubar => Ok(misc::menubar()),
        Command::Upgrade => Ok(misc::upgrade()),
        Command::Tui => Ok(tui::run(TuiStart::Dashboard)),
        Command::Watch => Ok(tui::run(TuiStart::Watch)),
        Command::AddAccount => {
            accounts::add(switcher, opts.provider, opts.slot, opts.alias.as_deref())
        }
        Command::AddToken(token) => {
            accounts::add_token(switcher, &token, opts.email.as_deref(), opts.slot)
        }
        Command::RemoveAccount(id) => accounts::remove(switcher, &id),
        Command::DisableAccount(id) => accounts::set_disabled(switcher, &id, true),
        Command::EnableAccount(id) => accounts::set_disabled(switcher, &id, false),
        Command::List => list::list_cmd(switcher, json, opts.token_status, opts.provider),
        Command::Status => list::status_cmd(switcher, json, opts.provider),
        Command::Switch => switch::rotate_cmd(
            switcher,
            opts.provider,
            opts.strategy.as_deref(),
            opts.model.as_deref(),
            json,
        ),
        Command::SwitchTo(id) => switch::direct_cmd(switcher, &id, opts.force, json),
        Command::Purge => misc::purge(switcher),
        Command::Export(path) => Ok(crate::transfer::export_cmd(
            &switcher.store.paths,
            &path,
            opts.account.as_deref(),
            opts.full,
        )),
        Command::Import(path) => Ok(crate::transfer::import_cmd(
            &switcher.store.paths,
            &path,
            opts.force,
        )),
    })
}

/// Shared prologue of every command that touches the store: logging, root
/// guard, Ctrl-C note, the switcher, and the error → exit-status mapping.
pub(crate) fn with_switcher(
    debug: bool,
    json: bool,
    body: impl FnOnce(&mut Switcher) -> Result<i32>,
) -> i32 {
    // A failing `Paths::from_env` is reported by `Switcher::from_env` below.
    if let Ok(paths) = Paths::from_env() {
        logging::init(&paths, debug);
    }
    if let Some(status) = root_guard() {
        return status;
    }
    JSON_MODE.store(json, Ordering::Relaxed);
    install_sigint_note();
    let result = Switcher::from_env().and_then(|mut switcher| {
        // JSON mode: stdout is the one document and stderr stays empty.
        if json {
            switcher.ui = Box::new(SilentUi);
        }
        body(&mut switcher)
    });
    match result {
        Ok(status) => status,
        Err(err) => report_error(&err, json),
    }
}

/// `Error: <msg>` on stderr, or the JSON envelope on stdout; exit 1.
pub(crate) fn report_error(err: &CswitchError, json: bool) -> i32 {
    if json {
        print!(
            "{}",
            jsonout::render_document(&jsonout::error_envelope(err))
        );
    } else {
        printer::error(&format!("Error: {err}"));
    }
    1
}

/// argparse-style usage error for the main parser; exit 2.
pub(crate) fn usage_error(message: &str) -> i32 {
    eprintln!("{USAGE_LINE}\n{PROG}: error: {message}");
    2
}

/// argparse-style usage error for a pre-dispatched verb; exit 2.
pub(crate) fn usage_error_for(verb: &str, usage: &str, message: &str) -> i32 {
    eprintln!("{usage}\n{PROG} {verb}: error: {message}");
    2
}

/// The argv of a pre-dispatched verb: positionals plus `--debug`, `-h` and
/// the verb's own flags.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct VerbArgs {
    pub positionals: Vec<String>,
    pub flags: Vec<String>,
    pub debug: bool,
    pub help: bool,
}

impl VerbArgs {
    pub fn parse(argv: &[String], allowed: &[&str]) -> std::result::Result<Self, String> {
        let mut args = Self::default();
        let mut unrecognized = Vec::new();
        for token in argv {
            match token.as_str() {
                "-h" | "--help" => args.help = true,
                "--debug" => args.debug = true,
                flag if allowed.contains(&flag) => args.flags.push(flag.to_string()),
                other if other.starts_with('-') && other != "-" => unrecognized.push(token.clone()),
                _ => args.positionals.push(token.clone()),
            }
        }
        if unrecognized.is_empty() {
            Ok(args)
        } else {
            Err(format!(
                "unrecognized arguments: {}",
                unrecognized.join(" ")
            ))
        }
    }

    pub fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }
}

/// Refuse to run as root outside a container (exit 1).
#[cfg(unix)]
fn root_guard() -> Option<i32> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 || in_container() {
        return None;
    }
    eprintln!("Error: Do not run this script as root (unless running in a container)");
    Some(1)
}

#[cfg(not(unix))]
fn root_guard() -> Option<i32> {
    None
}

#[cfg(unix)]
fn in_container() -> bool {
    let env_set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if env_set("CONTAINER") || env_set("container") || std::path::Path::new("/.dockerenv").exists()
    {
        return true;
    }
    std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|cgroup| {
        ["docker", "lxc", "containerd", "kubepods"]
            .iter()
            .any(|hint| cgroup.contains(hint))
    })
}

/// Ctrl-C prints `Operation cancelled` and exits 130. Only async-signal-safe
/// calls run in the handler, so the note is unstyled.
#[cfg(unix)]
fn install_sigint_note() {
    extern "C" fn on_sigint(_signal: libc::c_int) {
        const NOTE: &[u8] = b"\nOperation cancelled\n";
        let fd = if JSON_MODE.load(Ordering::Relaxed) {
            2
        } else {
            1
        };
        // SAFETY: write and _exit are async-signal-safe; the buffer outlives the call.
        unsafe {
            libc::write(fd, NOTE.as_ptr().cast(), NOTE.len());
            libc::_exit(130);
        }
    }
    // SAFETY: installing a plain handler for SIGINT.
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}

#[cfg(not(unix))]
fn install_sigint_note() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn verb_args_split_flags_and_positionals() {
        let args = VerbArgs::parse(&argv(&["2", "--unset", "--debug"]), &["--unset"]).unwrap();
        assert_eq!(args.positionals, ["2"]);
        assert!(args.has("--unset") && args.debug && !args.help);
        assert!(VerbArgs::parse(&argv(&["-h"]), &[]).unwrap().help);
        assert_eq!(
            VerbArgs::parse(&argv(&["1", "--bogus"]), &[]).unwrap_err(),
            "unrecognized arguments: --bogus"
        );
        let args = VerbArgs::parse(&argv(&["-"]), &[]).unwrap();
        assert_eq!(args.positionals, ["-"]);
    }
}
