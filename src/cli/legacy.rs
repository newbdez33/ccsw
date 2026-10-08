//! The cswap argv contract: verb → legacy-flag translation, the main parser,
//! cross-flag validation and the help text.
//!
//! The parser is hand-written so every message matches argparse's wording;
//! clap would phrase them differently.

use crate::VERSION;
use crate::provider::Provider;

pub const PROG: &str = "cswitch";
pub const USAGE_LINE: &str = "usage: cswitch <command> [args] [options]";

/// Verbs rewritten to their legacy flag. `switch` is special-cased in [`translate`].
const VERB_FLAGS: &[(&str, &str)] = &[
    ("help", "--help"),
    ("list", "--list"),
    ("ls", "--list"),
    ("status", "--status"),
    ("add", "--add-account"),
    ("add-token", "--add-token"),
    ("remove", "--remove-account"),
    ("rm", "--remove-account"),
    ("disable", "--disable-account"),
    ("enable", "--enable-account"),
    ("export", "--export"),
    ("import", "--import"),
    ("purge", "--purge"),
    ("upgrade", "--upgrade"),
    ("update", "--upgrade"),
    ("tui", "--tui"),
    ("watch", "--watch"),
    ("menubar", "--menubar"),
];

/// Rewrite a leading verb to its legacy flag; everything after it passes through.
pub fn translate(argv: Vec<String>) -> Vec<String> {
    let Some(first) = argv.first() else {
        return argv;
    };
    if first == "switch" {
        let rest = &argv[1..];
        return match rest.first() {
            Some(word) if Provider::parse_selector(word).is_some() => {
                let mut out = vec![
                    "--switch".to_string(),
                    "--provider".to_string(),
                    word.to_ascii_lowercase(),
                ];
                out.extend(rest[1..].iter().cloned());
                out
            }
            Some(target) if !target.starts_with('-') => {
                let mut out = vec!["--switch-to".to_string(), target.clone()];
                out.extend(rest[1..].iter().cloned());
                out
            }
            _ => {
                let mut out = vec!["--switch".to_string()];
                out.extend(rest.iter().cloned());
                out
            }
        };
    }
    match VERB_FLAGS.iter().find(|(verb, _)| verb == first) {
        Some((verb, flag)) => {
            let mut out = vec![flag.to_string()];
            let mut rest = argv[1..].iter();
            if matches!(*verb, "add" | "list" | "ls" | "status")
                && let Some(word) = argv.get(1)
                && Provider::parse_selector(word).is_some()
            {
                out.push("--provider".to_string());
                out.push(word.to_ascii_lowercase());
                rest.next();
            }
            out.extend(rest.cloned());
            out
        }
        None => argv,
    }
}

/// The selected command (the mutually exclusive legacy group).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    AddAccount,
    /// `Some("")` means "prompt for the token".
    AddToken(String),
    RemoveAccount(String),
    DisableAccount(String),
    EnableAccount(String),
    List,
    Switch,
    SwitchTo(String),
    Status,
    Purge,
    Export(String),
    Import(String),
    Tui,
    Watch,
    Menubar,
    Upgrade,
}

impl Command {
    /// The flag spelling, for `not allowed with` messages.
    pub fn flag(&self) -> &'static str {
        match self {
            Self::AddAccount => "--add-account",
            Self::AddToken(_) => "--add-token",
            Self::RemoveAccount(_) => "--remove-account",
            Self::DisableAccount(_) => "--disable-account",
            Self::EnableAccount(_) => "--enable-account",
            Self::List => "--list",
            Self::Switch => "--switch",
            Self::SwitchTo(_) => "--switch-to",
            Self::Status => "--status",
            Self::Purge => "--purge",
            Self::Export(_) => "--export",
            Self::Import(_) => "--import",
            Self::Tui => "--tui",
            Self::Watch => "--watch",
            Self::Menubar => "--menubar",
            Self::Upgrade => "--upgrade",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options {
    pub command: Option<Command>,
    pub debug: bool,
    pub token_status: bool,
    pub json: bool,
    pub strategy: Option<String>,
    pub model: Option<String>,
    pub provider: Option<Provider>,
    pub slot: Option<i64>,
    pub email: Option<String>,
    pub account: Option<String>,
    pub alias: Option<String>,
    pub force: bool,
    pub full: bool,
    pub version: bool,
    pub help: bool,
}

const STRATEGIES: &[&str] = &["best", "next-available"];

/// Parse the translated argv. `Err` carries an argparse-style message (exit 2).
pub fn parse(argv: &[String]) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut unrecognized: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let token = &argv[i];
        let (name, inline_value) = match token.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_string())),
            _ => (token.as_str(), None),
        };
        // Take the value of a value-taking option: inline, else the next token
        // (which must not look like an option, as in argparse).
        let take_value = |i: &mut usize, allow_negative: bool| -> Result<String, String> {
            if let Some(value) = &inline_value {
                return Ok(value.clone());
            }
            let next = argv.get(*i + 1);
            match next {
                Some(value) if !looks_like_option(value, allow_negative) => {
                    *i += 1;
                    Ok(value.clone())
                }
                _ => Err(format!("argument {name}: expected one argument")),
            }
        };
        match name {
            "-h" | "--help" => opts.help = true,
            "--version" => opts.version = true,
            "--debug" => opts.debug = true,
            "--token-status" => opts.token_status = true,
            "--json" => opts.json = true,
            "--force" => opts.force = true,
            "--full" => opts.full = true,
            "--strategy" => {
                let value = take_value(&mut i, false)?;
                if !STRATEGIES.contains(&value.as_str()) {
                    return Err(format!(
                        "argument --strategy: invalid choice: '{value}' (choose from 'best', 'next-available')"
                    ));
                }
                opts.strategy = Some(value);
            }
            "--model" => opts.model = Some(take_value(&mut i, false)?),
            "--provider" => {
                let value = take_value(&mut i, false)?;
                opts.provider = Some(Provider::parse_selector(&value).ok_or_else(|| {
                    format!(
                        "argument --provider: invalid choice: '{value}' (choose from 'codex', 'claude')"
                    )
                })?);
            }
            "--slot" => {
                let value = take_value(&mut i, true)?;
                let slot: i64 = value
                    .trim()
                    .parse()
                    .map_err(|_| format!("argument --slot: invalid int value: '{value}'"))?;
                opts.slot = Some(slot);
            }
            "--email" => opts.email = Some(take_value(&mut i, false)?),
            "--account" => opts.account = Some(take_value(&mut i, false)?),
            "--alias" => opts.alias = Some(take_value(&mut i, false)?),
            "--add-account" => select(&mut opts, Command::AddAccount)?,
            "--add-token" => {
                let value = match &inline_value {
                    Some(value) => value.clone(),
                    None => match argv.get(i + 1) {
                        Some(next) if !looks_like_option(next, false) => {
                            i += 1;
                            next.clone()
                        }
                        _ => String::new(),
                    },
                };
                select(&mut opts, Command::AddToken(value))?;
            }
            "--remove-account" => {
                let value = take_value(&mut i, false)?;
                select(&mut opts, Command::RemoveAccount(value))?;
            }
            "--disable-account" => {
                let value = take_value(&mut i, false)?;
                select(&mut opts, Command::DisableAccount(value))?;
            }
            "--enable-account" => {
                let value = take_value(&mut i, false)?;
                select(&mut opts, Command::EnableAccount(value))?;
            }
            "--list" => select(&mut opts, Command::List)?,
            "--switch" => select(&mut opts, Command::Switch)?,
            "--switch-to" => {
                let value = take_value(&mut i, false)?;
                select(&mut opts, Command::SwitchTo(value))?;
            }
            "--status" => select(&mut opts, Command::Status)?,
            "--purge" => select(&mut opts, Command::Purge)?,
            "--export" => {
                let value = take_value(&mut i, false)?;
                select(&mut opts, Command::Export(value))?;
            }
            "--import" => {
                let value = take_value(&mut i, false)?;
                select(&mut opts, Command::Import(value))?;
            }
            "--tui" => select(&mut opts, Command::Tui)?,
            "--watch" => select(&mut opts, Command::Watch)?,
            "--menubar" => select(&mut opts, Command::Menubar)?,
            "--upgrade" => select(&mut opts, Command::Upgrade)?,
            _ => unrecognized.push(token.clone()),
        }
        i += 1;
    }
    if !unrecognized.is_empty() {
        return Err(format!(
            "unrecognized arguments: {}",
            unrecognized.join(" ")
        ));
    }
    Ok(opts)
}

fn looks_like_option(token: &str, allow_negative: bool) -> bool {
    if token == "-" || !token.starts_with('-') {
        return false;
    }
    !(allow_negative && token[1..].parse::<i64>().is_ok())
}

fn select(opts: &mut Options, command: Command) -> Result<(), String> {
    if let Some(existing) = &opts.command {
        return Err(format!(
            "argument {}: not allowed with argument {}",
            command.flag(),
            existing.flag()
        ));
    }
    opts.command = Some(command);
    Ok(())
}

/// The twelve cross-flag checks, in cswap's order.
pub fn validate(opts: &Options) -> Result<(), String> {
    use Command::*;
    let command = opts.command.as_ref();
    let is = |f: fn(&Command) -> bool| command.is_some_and(f);
    if command.is_none() {
        return Err(format!("no command given — try '{PROG} help'"));
    }
    if opts.token_status && !is(|c| matches!(c, List)) {
        return Err("--token-status can only be used with 'list'".into());
    }
    if opts.json && !is(|c| matches!(c, List | Status | Switch | SwitchTo(_))) {
        return Err("--json can only be used with 'list', 'status', or 'switch'".into());
    }
    if opts.json && opts.token_status {
        return Err("--token-status cannot be combined with --json".into());
    }
    if opts.strategy.is_some() && !is(|c| matches!(c, Switch)) {
        return Err("--strategy can only be used with bare 'switch'".into());
    }
    if opts.model.is_some() && opts.strategy.is_none() {
        return Err(
            "--model can only be used with 'switch --strategy best' or 'switch --strategy next-available'"
                .into(),
        );
    }
    if opts.slot.is_some() && !is(|c| matches!(c, AddAccount | AddToken(_))) {
        return Err("--slot can only be used with 'add' or 'add-token'".into());
    }
    if opts.email.is_some() && !is(|c| matches!(c, AddToken(_))) {
        return Err("--email can only be used with 'add-token'".into());
    }
    if opts.account.is_some() && !is(|c| matches!(c, Export(_))) {
        return Err("--account can only be used with 'export'".into());
    }
    if opts.alias.is_some() && !is(|c| matches!(c, AddAccount)) {
        return Err("--alias can only be used with 'add'".into());
    }
    if opts.force && !is(|c| matches!(c, Import(_) | SwitchTo(_))) {
        return Err("--force can only be used with 'import' or 'switch <num|email>'".into());
    }
    if opts.full && !is(|c| matches!(c, Export(_))) {
        return Err("--full can only be used with 'export'".into());
    }
    if opts.provider.is_some() && !is(|c| matches!(c, Switch | AddAccount | List | Status)) {
        return Err("--provider can only be used with 'switch', 'add', 'list', or 'status'".into());
    }
    Ok(())
}

pub fn version_line() -> String {
    format!("{PROG} {VERSION}")
}

pub fn help_text() -> String {
    format!(
        "{USAGE_LINE}

Multi-Account Switcher for OpenAI Codex and Claude Code

Commands:
  cswitch help                       show this help
  cswitch list [codex|claude]        list managed accounts (both providers by default)
  cswitch status [codex|claude]      show the active account of each provider
  cswitch switch [codex|claude]      rotate to the next account of one provider
  cswitch switch <num|email>         switch to a specific account (Codex or Claude)
  cswitch add [codex|claude]         add the current login(s)
  cswitch add-token [TOKEN|-]        register an OpenAI API key, or an Anthropic API key / setup-token (sk-ant-…)
  cswitch remove <num|email>         remove an account
  cswitch disable <num|email>        hold an account out of auto-rotation
  cswitch enable <num|email>         return a disabled account to rotation
  cswitch run <num|email> [-- ...]   run as an account, this terminal only
  cswitch run                        run the current dir's mapped account
  cswitch env <num|email>            print the CODEX_HOME export for an account
  cswitch map <num|email> [path]     map a directory to an account
  cswitch map                        list directory mappings
  cswitch unmap [path]               remove a directory mapping
  cswitch alias <num|email> <name>   set a short alias for an account
  cswitch alias <num|email> --unset  remove an account's alias
  cswitch alias                      list all aliases
  cswitch swap <a> <b>               exchange two accounts' slot numbers
  cswitch move <a> <slot>            assign an account to a slot (swaps if taken)
  cswitch auto                       auto-switch when nearing rate limits
  cswitch config [set KEY VALUE]     show or change settings (settings.json)
  cswitch export <path>              export accounts
  cswitch import <path>              import accounts
  cswitch tui                        interactive dashboard (also: bare cswitch)
  cswitch watch                      dashboard, opened on the live watch page
  cswitch upgrade                    how to upgrade to the latest release
  cswitch purge                      remove all cswitch data

Aliases: ls=list  rm=remove  update=upgrade

options:
  -h, --help            show this help message and exit
  --version             show program's version number and exit
  --debug               Enable debug logging
  --token-status        Show stored-token expiry diagnostics (use with 'list')
  --json                Emit machine-readable JSON to stdout (use with 'list',
                        'status', or 'switch'). See README 'JSON output for
                        scripting'.
  --strategy {{best,next-available}}
                        With bare 'switch': pick the target by remaining 5h/7d
                        quota. 'best' jumps to the account with the most
                        headroom; 'next-available' rotates to the next account,
                        skipping any at their limit
  --model NAMES         With 'switch --strategy': also count these model pools'
                        weekly limits when comparing accounts (comma-separated
                        pool names, or 'all'). Defaults to the autoswitch.model
                        setting
  --slot NUM            Specify slot number when adding account (use with 'add'
                        or 'add-token')
  --email EMAIL         Email address for the account. Optional with
                        'add-token'; defaults to api-key-{{slot}}@token.local
                        (setup-token-{{slot}}@token.local for a Claude
                        setup-token) since tokens carry no email metadata.
  --account NUM|EMAIL   Limit export to one account (use with 'export')
  --alias NAME          Set a short display alias for the account (use with
                        'add')
  --force               Overwrite existing accounts during import; with 'switch
                        <num|email>', activate the stored credentials without
                        backing up the current login first
  --full                Accepted for compatibility (use with 'export'); Codex
                        has no per-account config to include
  --provider {{codex,claude}}
                        Act on one provider; the same as the word after
                        'switch', 'add', 'list' or 'status'

Flags combine with subcommands:
  cswitch switch --strategy best           # pick the account with most quota left
  cswitch switch --strategy next-available # rotate, skipping rate-limited accounts
  cswitch switch claude                     # rotate among the Claude accounts
  cswitch switch user@example.com
  cswitch list --token-status
  cswitch list --json
  cswitch add --slot 3                      # add to a specific slot
  cswitch add-token sk-... --email me@example.com
  cswitch add-token sk-ant-oat01-... --email me@example.com
  cswitch run 2 -- resume                   # forward args after '--' to codex
  cswitch auto --once                       # single auto-switch tick (cron-friendly)
  cswitch config set autoswitch.threshold 80

The original flag spellings (cswitch --switch, cswitch --list, ...) keep working.
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn translation_worked_examples() {
        let cases: &[(&[&str], &[&str])] = &[
            (&[], &[]),
            (&["--list"], &["--list"]),
            (&["switch"], &["--switch"]),
            (
                &["switch", "--strategy", "best"],
                &["--switch", "--strategy", "best"],
            ),
            (&["switch", "2"], &["--switch-to", "2"]),
            (&["switch", "claude"], &["--switch", "--provider", "claude"]),
            (
                &["switch", "Codex", "--strategy", "best"],
                &["--switch", "--provider", "codex", "--strategy", "best"],
            ),
            (
                &["add", "claude", "--slot", "3"],
                &["--add-account", "--provider", "claude", "--slot", "3"],
            ),
            (
                &["list", "codex", "--json"],
                &["--list", "--provider", "codex", "--json"],
            ),
            (&["ls", "claude"], &["--list", "--provider", "claude"]),
            (&["status", "claude"], &["--status", "--provider", "claude"]),
            (&["switch", "dev"], &["--switch-to", "dev"]),
            (&["remove", "claude"], &["--remove-account", "claude"]),
            (
                &["switch", "u@x.com", "--json"],
                &["--switch-to", "u@x.com", "--json"],
            ),
            (&["ls"], &["--list"]),
            (&["rm", "2"], &["--remove-account", "2"]),
            (&["update"], &["--upgrade"]),
            (
                &["export", "b.cswitch", "--full"],
                &["--export", "b.cswitch", "--full"],
            ),
            (&["bogus"], &["bogus"]),
            (&["help"], &["--help"]),
            (&["add-token", "sk-x"], &["--add-token", "sk-x"]),
        ];
        for (input, expected) in cases {
            assert_eq!(translate(argv(input)), argv(expected), "{input:?}");
        }
    }

    #[test]
    fn parse_selects_commands_and_options() {
        let opts = parse(&argv(&["--switch-to", "2", "--json", "--force"])).unwrap();
        assert_eq!(opts.command, Some(Command::SwitchTo("2".into())));
        assert!(opts.json && opts.force);
        let opts = parse(&argv(&["--add-token"])).unwrap();
        assert_eq!(opts.command, Some(Command::AddToken(String::new())));
        let opts = parse(&argv(&[
            "--add-token",
            "-",
            "--email=a@b.co",
            "--slot",
            "3",
        ]))
        .unwrap();
        assert_eq!(opts.command, Some(Command::AddToken("-".into())));
        assert_eq!(opts.email.as_deref(), Some("a@b.co"));
        assert_eq!(opts.slot, Some(3));
        let opts = parse(&argv(&["--add-token", "--slot", "-1"])).unwrap();
        assert_eq!(opts.command, Some(Command::AddToken(String::new())));
        assert_eq!(opts.slot, Some(-1));
        let opts = parse(&argv(&["--switch", "--strategy", "best", "--model", "all"])).unwrap();
        assert_eq!(opts.strategy.as_deref(), Some("best"));
        assert_eq!(opts.model.as_deref(), Some("all"));
        assert!(parse(&argv(&["--version"])).unwrap().version);
        assert!(parse(&argv(&["-h"])).unwrap().help);
        let opts = parse(&argv(&["--switch", "--provider", "claude"])).unwrap();
        assert_eq!(opts.provider, Some(Provider::Claude));
        assert_eq!(
            parse(&argv(&["--list", "--provider", "gemini"])).unwrap_err(),
            "argument --provider: invalid choice: 'gemini' (choose from 'codex', 'claude')"
        );
    }

    #[test]
    fn parse_errors_read_like_argparse() {
        let cases = [
            (
                &["--list", "--switch"][..],
                "argument --switch: not allowed with argument --list",
            ),
            (&["bogus"], "unrecognized arguments: bogus"),
            (&["--list", "x", "y"], "unrecognized arguments: x y"),
            (
                &["--strategy", "bogus"],
                "argument --strategy: invalid choice: 'bogus' (choose from 'best', 'next-available')",
            ),
            (
                &["--switch-to"],
                "argument --switch-to: expected one argument",
            ),
            (
                &["--switch-to", "--json"],
                "argument --switch-to: expected one argument",
            ),
            (
                &["--slot", "abc"],
                "argument --slot: invalid int value: 'abc'",
            ),
        ];
        for (input, message) in cases {
            assert_eq!(parse(&argv(input)).unwrap_err(), message, "{input:?}");
        }
    }

    #[test]
    fn cross_flag_validation_in_order() {
        let check = |args: &[&str]| validate(&parse(&argv(args)).unwrap()).unwrap_err();
        assert_eq!(check(&[]), "no command given — try 'cswitch help'");
        assert_eq!(check(&["--json"]), "no command given — try 'cswitch help'");
        assert_eq!(
            check(&["--status", "--token-status"]),
            "--token-status can only be used with 'list'"
        );
        assert_eq!(
            check(&["--purge", "--json"]),
            "--json can only be used with 'list', 'status', or 'switch'"
        );
        assert_eq!(
            check(&["--list", "--json", "--token-status"]),
            "--token-status cannot be combined with --json"
        );
        assert_eq!(
            check(&["--switch-to", "2", "--strategy", "best"]),
            "--strategy can only be used with bare 'switch'"
        );
        assert_eq!(
            check(&["--switch", "--model", "x"]),
            "--model can only be used with 'switch --strategy best' or 'switch --strategy next-available'"
        );
        assert_eq!(
            check(&["--list", "--slot", "2"]),
            "--slot can only be used with 'add' or 'add-token'"
        );
        assert_eq!(
            check(&["--add-account", "--email", "a@b.co"]),
            "--email can only be used with 'add-token'"
        );
        assert_eq!(
            check(&["--list", "--account", "1"]),
            "--account can only be used with 'export'"
        );
        assert_eq!(
            check(&["--add-token", "--alias", "x"]),
            "--alias can only be used with 'add'"
        );
        assert_eq!(
            check(&["--switch", "--force"]),
            "--force can only be used with 'import' or 'switch <num|email>'"
        );
        assert_eq!(
            check(&["--import", "f", "--full"]),
            "--full can only be used with 'export'"
        );
        assert_eq!(
            check(&["--remove-account", "2", "--provider", "codex"]),
            "--provider can only be used with 'switch', 'add', 'list', or 'status'"
        );
        for ok in [
            &["--switch-to", "2", "--json", "--force"][..],
            &["--switch", "--provider", "claude", "--strategy", "best"],
            &["--status", "--provider", "codex", "--json"],
            &["--list", "--token-status"],
            &["--switch", "--strategy", "best", "--model", "all", "--json"],
            &["--add-account", "--slot", "3", "--alias", "dev"],
            &["--add-token", "tok", "--email", "a@b.co", "--slot", "3"],
            &["--export", "p", "--account", "1", "--full"],
            &["--import", "p", "--force"],
        ] {
            validate(&parse(&argv(ok)).unwrap()).unwrap();
        }
    }

    #[test]
    fn help_and_version() {
        let help = help_text();
        assert!(help.starts_with("usage: cswitch <command> [args] [options]\n"));
        assert!(help.contains("Multi-Account Switcher for OpenAI Codex and Claude Code"));
        assert!(help.contains("Aliases: ls=list  rm=remove  update=upgrade"));
        assert!(help.contains("keep working"));
        assert!(help.contains("cswitch switch [codex|claude]"));
        assert!(help.contains("sk-ant-"));
        assert!(!help.contains("cswap "));
        assert_eq!(version_line(), format!("cswitch {VERSION}"));
    }
}
