//! `ccsw run`, `env`, `map`, `unmap` — each parses its own arguments
//! (contract §9). `argv` excludes the program name and the verb.

use std::path::PathBuf;

use clap::Parser;

use crate::autoswitch::root_guard;
use crate::errors::Result;
use crate::model::Roster;
use crate::session::{self, EnvPlan, EnvRequest, HostEnv, ShareOptions, Shell};
use crate::store::{Store, roster};

const RUN_EPILOG: &str = "Examples:
  ccsw run 2
  ccsw run user@example.com
  ccsw run 2 --no-share
  ccsw run 2 --share-history
  ccsw run 2 -- --resume";

#[derive(Debug, Parser)]
#[command(
    name = "ccsw run",
    about = "[EXPERIMENTAL] Launch Codex as a stored account in this terminal only (the default login and other terminals are unaffected).",
    after_help = RUN_EPILOG,
    disable_version_flag = true
)]
struct RunArgs {
    /// Account to run (number or email). Omit to use the current directory's mapping (see `ccsw map`).
    account: Option<String>,
    /// Don't share AGENTS.md/prompts/skills from the Codex home into the session profile (and remove previously shared items)
    #[arg(long)]
    no_share: bool,
    /// Share conversation history (sessions/ and history.jsonl) from the Codex home into the session profile, so every account sees one unified history. --no-share-history restores per-account history (the default). Not supported on Windows.
    #[arg(long, overrides_with = "no_share_history")]
    share_history: bool,
    #[arg(long, overrides_with = "share_history", hide = true)]
    no_share_history: bool,
    /// Enable debug logging
    #[arg(long)]
    debug: bool,
}

#[derive(Debug, Parser)]
#[command(
    name = "ccsw env",
    about = "Print shell lines that pin this shell to a stored account's session profile (eval \"$(ccsw env 2)\").",
    disable_version_flag = true
)]
struct EnvArgs {
    /// Account to prepare (number or email). Omit to use the current directory's mapping.
    account: Option<String>,
    /// Don't share AGENTS.md/prompts/skills into the session profile
    #[arg(long)]
    no_share: bool,
    /// Share sessions/ and history.jsonl into the session profile (not on Windows)
    #[arg(long)]
    share_history: bool,
    /// Shell dialect of the printed lines
    #[arg(long, default_value = "sh", value_parser = clap::builder::PossibleValuesParser::new(Shell::CHOICES))]
    shell: String,
    /// Print only the line that unpins the shell
    #[arg(long)]
    unset: bool,
    /// Enable debug logging
    #[arg(long)]
    debug: bool,
}

const MAP_EPILOG: &str = "Examples:
  ccsw map 2 ~/work/client-app
  ccsw map user@example.com          # map the current directory
  ccsw map                           # list all mappings";

#[derive(Debug, Parser)]
#[command(
    name = "ccsw map",
    about = "Map a stored account to a directory so `ccsw run` (with no account) auto-launches it there. With no arguments, lists all mappings.",
    after_help = MAP_EPILOG,
    disable_version_flag = true
)]
struct MapArgs {
    /// Account to map (number, email or alias)
    account: Option<String>,
    /// Directory to map (default: current directory)
    path: Option<PathBuf>,
    /// Enable debug logging
    #[arg(long)]
    debug: bool,
}

#[derive(Debug, Parser)]
#[command(
    name = "ccsw unmap",
    about = "Remove a directory → account mapping (default: current directory).",
    disable_version_flag = true
)]
struct UnmapArgs {
    /// Directory to unmap (default: current directory)
    path: Option<PathBuf>,
    /// Enable debug logging
    #[arg(long)]
    debug: bool,
}

/// Everything after the first literal `--` is forwarded to the child verbatim.
fn split_tail(argv: Vec<String>) -> (Vec<String>, Vec<String>) {
    match argv.iter().position(|arg| arg == "--") {
        Some(index) => (argv[..index].to_vec(), argv[index + 1..].to_vec()),
        None => (argv, Vec::new()),
    }
}

fn parse<T: Parser>(prog: &str, argv: Vec<String>) -> std::result::Result<T, i32> {
    T::try_parse_from(std::iter::once(prog.to_string()).chain(argv)).map_err(|err| {
        let _ = err.print();
        err.exit_code()
    })
}

/// The log file in the backup root, plus the stderr mirror with `--debug`.
fn enable_debug(debug: bool) {
    if let Ok(paths) = crate::paths::Paths::from_env() {
        crate::logging::init(&paths, debug);
    }
}

fn open_store(check_credential_store: bool) -> Result<(Store, Roster)> {
    let store = Store::from_env()?;
    if check_credential_store {
        store.paths.validate_credential_store()?;
    }
    let roster = roster::read_or_empty(&store.paths)?;
    Ok((store, roster))
}

fn cwd() -> Result<PathBuf> {
    std::env::current_dir().map_err(|err| {
        crate::errors::CcswError::session(format!("could not read the current directory: {err}"))
    })
}

fn report(err: &crate::errors::CcswError) -> i32 {
    eprintln!("Error: {err}");
    1
}

pub fn run_cmd(argv: Vec<String>) -> i32 {
    let (head, tail) = split_tail(argv);
    let args: RunArgs = match parse("ccsw run", head) {
        Ok(args) => args,
        Err(code) => return code,
    };
    if let Some(code) = root_guard() {
        return code;
    }
    enable_debug(args.debug);
    let opts = ShareOptions {
        share: !args.no_share,
        share_history: args.share_history,
    };
    let planned = (|| -> Result<(Store, session::Launch)> {
        let (store, roster) = open_store(true)?;
        let host = HostEnv::detect();
        let target =
            session::resolve_run_target(&store, &roster, args.account.as_deref(), &cwd()?)?;
        let launch = session::plan_launch(&store, &roster, &host, target, tail, opts)?;
        Ok((store, launch))
    })();
    let (store, launch) = match planned {
        Ok(planned) => planned,
        Err(err) => return report(&err),
    };
    for line in &launch.notices {
        println!("{line}");
    }
    match session::exec_or_wait(&store, launch) {
        Ok(code) => code,
        Err(err) => report(&err),
    }
}

pub fn env_cmd(argv: Vec<String>) -> i32 {
    let args: EnvArgs = match parse("ccsw env", argv) {
        Ok(args) => args,
        Err(code) => return code,
    };
    if args.unset && args.account.is_some() {
        eprintln!("ccsw env: error: --unset does not take a NUM|EMAIL|ALIAS argument");
        return 2;
    }
    if let Some(code) = root_guard() {
        return code;
    }
    enable_debug(args.debug);
    let shell = Shell::parse(&args.shell).unwrap_or(Shell::Sh);
    let opts = ShareOptions {
        share: !args.no_share,
        share_history: args.share_history,
    };
    let plan = (|| -> Result<EnvPlan> {
        let (store, roster) = if args.unset {
            (Store::from_env()?, Roster::empty())
        } else {
            open_store(true)?
        };
        let host = if args.unset {
            HostEnv::default()
        } else {
            HostEnv::detect()
        };
        let cwd = cwd()?;
        session::plan_env(
            &store,
            &roster,
            &host,
            EnvRequest {
                account: args.account.as_deref(),
                cwd: &cwd,
                shell,
                unset: args.unset,
                opts,
            },
        )
    })();
    match plan {
        Ok(EnvPlan::Lines { lines, notices }) => {
            for notice in notices {
                eprintln!("{notice}");
            }
            for line in lines {
                println!("{line}");
            }
            0
        }
        Ok(EnvPlan::Note(note)) => {
            eprintln!("{note}");
            0
        }
        Err(err) => report(&err),
    }
}

pub fn map_cmd(argv: Vec<String>) -> i32 {
    let args: MapArgs = match parse("ccsw map", argv) {
        Ok(args) => args,
        Err(code) => return code,
    };
    if let Some(code) = root_guard() {
        return code;
    }
    enable_debug(args.debug);
    let lines = (|| -> Result<Vec<String>> {
        let (store, roster) = open_store(false)?;
        session::map(
            &store,
            &roster,
            args.account.as_deref(),
            args.path.as_deref(),
            &cwd()?,
        )
    })();
    match lines {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            0
        }
        Err(err) => report(&err),
    }
}

pub fn unmap_cmd(argv: Vec<String>) -> i32 {
    let args: UnmapArgs = match parse("ccsw unmap", argv) {
        Ok(args) => args,
        Err(code) => return code,
    };
    if let Some(code) = root_guard() {
        return code;
    }
    enable_debug(args.debug);
    let line = (|| -> Result<String> {
        let store = Store::from_env()?;
        session::unmap(&store, args.path.as_deref(), &cwd()?)
    })();
    match line {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(err) => report(&err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tail_splits_at_the_first_double_dash() {
        assert_eq!(
            split_tail(argv(&["2", "--no-share", "--", "--resume", "--", "x"])),
            (argv(&["2", "--no-share"]), argv(&["--resume", "--", "x"]))
        );
        assert_eq!(split_tail(argv(&["2"])), (argv(&["2"]), vec![]));
        assert_eq!(split_tail(argv(&["--"])), (vec![], vec![]));
    }

    #[test]
    fn run_flags_parse_with_last_history_flag_winning() {
        let args: RunArgs = parse(
            "ccsw run",
            argv(&["2", "--share-history", "--no-share-history"]),
        )
        .unwrap();
        assert_eq!(args.account.as_deref(), Some("2"));
        assert!(!args.share_history);
        let args: RunArgs = parse(
            "ccsw run",
            argv(&["--no-share-history", "--share-history", "--no-share"]),
        )
        .unwrap();
        assert!(args.share_history && args.no_share);
        assert_eq!(
            parse::<RunArgs>("ccsw run", argv(&["--bogus"])).unwrap_err(),
            2
        );
        assert_eq!(
            parse::<RunArgs>("ccsw run", argv(&["--help"])).unwrap_err(),
            0
        );
    }

    #[test]
    fn env_and_map_flags_parse() {
        let args: EnvArgs = parse("ccsw env", argv(&["--shell", "fish", "--unset"])).unwrap();
        assert_eq!(args.shell, "fish");
        assert!(args.unset);
        assert_eq!(
            parse::<EnvArgs>("ccsw env", argv(&["--shell", "zsh"])).unwrap_err(),
            2
        );
        assert_eq!(env_cmd(argv(&["--unset", "2"])), 2);
        let args: MapArgs = parse("ccsw map", argv(&["dev", "/tmp/x"])).unwrap();
        assert_eq!(args.account.as_deref(), Some("dev"));
        assert_eq!(args.path.as_deref(), Some(Path::new("/tmp/x")));
        let args: UnmapArgs = parse("ccsw unmap", argv(&[])).unwrap();
        assert!(args.path.is_none());
        assert_eq!(
            parse::<UnmapArgs>("ccsw unmap", argv(&["a", "b"])).unwrap_err(),
            2
        );
    }
}
