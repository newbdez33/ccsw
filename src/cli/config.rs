//! `ccsw config` — a pre-dispatched verb with its own parser: `list` (the
//! default), `get KEY`, `set KEY VALUE`, `unset KEY`, `path`.
//!
//! The settings themselves live in `store::settings`; this file owns the
//! grammar and the output shapes (research notes `cswap-cli-contract.md` §11).
//! `--debug` is accepted for parity; logging is configured by the front controller.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde::Serialize;
use serde_json::Value;

use crate::errors::{CcswError, Result};
use crate::model::SCHEMA_VERSION;
use crate::paths::Paths;
use crate::printer;
use crate::store::settings::{self, SETTING_SPECS, SettingSpec, format_setting_value};

const JSON_ONLY_WITH_LIST_OR_GET: &str = "--json can only be used with list or get";

/// Entry point for the front controller. `argv` excludes the program name and
/// the `config` word. Returns the exit status.
pub fn run(argv: Vec<String>) -> i32 {
    let paths = match Paths::from_env() {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("{}", printer::reddened(&format!("Error: {err}")));
            return 1;
        }
    };
    let mut out = io::stdout().lock();
    let mut err = io::stderr().lock();
    run_with(argv, &paths, &mut out, &mut err)
}

/// `run` with an explicit store location and output streams.
pub fn run_with(argv: Vec<String>, paths: &Paths, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let matches = match command().try_get_matches_from(argv) {
        Ok(matches) => matches,
        Err(clap_err) => return report_clap(&clap_err, out, err),
    };
    let json = matches.get_flag("json");
    let action = Action::from_matches(&matches);
    if json && !matches!(action, Action::List | Action::Get(_)) {
        let clap_err = command().error(ErrorKind::ArgumentConflict, JSON_ONLY_WITH_LIST_OR_GET);
        return report_clap(&clap_err, out, err);
    }
    match execute(&action, json, paths, out, err) {
        Ok(()) => 0,
        Err(error) => {
            if json {
                write_json(out, &ErrorPayload::new(&error));
            } else {
                let _ = writeln!(err, "{}", printer::reddened(&format!("Error: {error}")));
            }
            1
        }
    }
}

enum Action {
    List,
    Get(String),
    Set(String, String),
    Unset(String),
    Path,
}

impl Action {
    fn from_matches(matches: &ArgMatches) -> Self {
        let text =
            |sub: &ArgMatches, name: &str| sub.get_one::<String>(name).cloned().unwrap_or_default();
        match matches.subcommand() {
            Some(("get", sub)) => Self::Get(text(sub, "key")),
            Some(("set", sub)) => Self::Set(text(sub, "key"), text(sub, "value")),
            Some(("unset", sub)) => Self::Unset(text(sub, "key")),
            Some(("path", _)) => Self::Path,
            _ => Self::List,
        }
    }
}

fn command() -> Command {
    let key = || Arg::new("key").value_name("KEY").required(true);
    Command::new("ccsw config")
        .bin_name("ccsw config")
        .no_binary_name(true)
        .about("Read and edit ccsw settings (settings.json in the backup root).")
        .after_help(epilog())
        .disable_help_subcommand(true)
        .arg(
            Arg::new("json")
                .long("json")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Emit machine-readable JSON to stdout (use with 'list' or 'get')"),
        )
        .arg(
            Arg::new("debug")
                .long("debug")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Enable debug logging"),
        )
        .subcommand(
            Command::new("list").about("Show every setting with its effective value (the default)"),
        )
        .subcommand(
            Command::new("get")
                .about("Print one setting's effective value")
                .arg(key()),
        )
        .subcommand(
            Command::new("set")
                .about("Change one setting")
                .arg(key())
                .arg(Arg::new("value").value_name("VALUE").required(true)),
        )
        .subcommand(
            Command::new("unset")
                .about("Remove one setting so its default applies")
                .arg(key()),
        )
        .subcommand(Command::new("path").about("Print where settings.json lives"))
}

fn epilog() -> String {
    let mut text = String::from("Keys:\n");
    for spec in SETTING_SPECS {
        text.push_str(&format!(
            "  {:<34}  {} (default {})\n",
            spec.key,
            spec.help,
            format_setting_value(&spec.default_value())
        ));
    }
    text.push_str(
        "\nExamples:\n  \
         ccsw config                              # list effective settings\n  \
         ccsw config get autoswitch.threshold\n  \
         ccsw config set autoswitch.threshold 80\n  \
         ccsw config unset autoswitch.threshold   # back to the default\n  \
         ccsw config path                         # where settings.json lives",
    );
    text
}

/// Help goes to stdout with status 0; usage errors to stderr with status 2.
fn report_clap(error: &clap::Error, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let text = error.render().to_string();
    let stream: &mut dyn Write = if error.use_stderr() { err } else { out };
    let _ = write!(stream, "{text}");
    error.exit_code()
}

fn execute(
    action: &Action,
    json: bool,
    paths: &Paths,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    match action {
        Action::List => {
            let rows = settings::config_list(paths)?;
            if json {
                write_json(
                    out,
                    &ListPayload {
                        schema_version: SCHEMA_VERSION,
                        path: absolute(&settings::config_path(paths))
                            .display()
                            .to_string(),
                        settings: rows
                            .iter()
                            .map(|(key, value, is_set)| SettingRow {
                                key,
                                value: value.clone(),
                                is_set: *is_set,
                            })
                            .collect(),
                    },
                );
            } else {
                for line in list_lines(&rows) {
                    let _ = writeln!(out, "{line}");
                }
            }
        }
        Action::Get(key) => {
            let (value, is_set) = settings::config_get(paths, key)?;
            if json {
                write_json(
                    out,
                    &GetPayload {
                        schema_version: SCHEMA_VERSION,
                        key: key.clone(),
                        value,
                        is_set,
                    },
                );
            } else {
                let _ = writeln!(out, "{}", format_setting_value(&value));
            }
        }
        Action::Set(key, value) => {
            let formatted = settings::config_set(paths, key, value)?;
            let _ = writeln!(out, "{key} = {formatted}");
        }
        Action::Unset(key) => {
            if settings::config_unset(paths, key)? {
                let default = SettingSpec::find(key)
                    .map(|spec| format_setting_value(&spec.default_value()))
                    .unwrap_or_default();
                let _ = writeln!(out, "{key} unset (default: {default})");
            } else {
                let _ = writeln!(
                    err,
                    "{}",
                    printer::muted(&format!("{key} is not set; nothing to do"))
                );
            }
        }
        Action::Path => {
            let _ = writeln!(out, "{}", absolute(&settings::config_path(paths)).display());
        }
    }
    Ok(())
}

/// `<key padded>  <value padded>` plus a dimmed `  (default)` when the key is not set.
fn list_lines(rows: &[(&'static str, Value, bool)]) -> Vec<String> {
    let formatted: Vec<(&str, String, bool)> = rows
        .iter()
        .map(|(key, value, is_set)| (*key, format_setting_value(value), *is_set))
        .collect();
    let key_width = formatted
        .iter()
        .map(|(key, _, _)| key.len())
        .max()
        .unwrap_or(0);
    let value_width = formatted
        .iter()
        .map(|(_, value, _)| value.chars().count())
        .max()
        .unwrap_or(0);
    formatted
        .iter()
        .map(|(key, value, is_set)| {
            let mut line = format!("{key:<key_width$}  {value:<value_width$}");
            if !is_set {
                line.push_str("  ");
                line.push_str(&printer::dimmed("(default)"));
            }
            line
        })
        .collect()
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn write_json<T: Serialize>(out: &mut dyn Write, payload: &T) {
    if let Ok(text) = serde_json::to_string_pretty(payload) {
        let _ = writeln!(out, "{text}");
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingRow {
    key: &'static str,
    value: Value,
    is_set: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListPayload {
    schema_version: u32,
    path: String,
    settings: Vec<SettingRow>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GetPayload {
    schema_version: u32,
    key: String,
    value: Value,
    is_set: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorPayload {
    schema_version: u32,
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    #[serde(rename = "type")]
    kind: &'static str,
    message: String,
}

impl ErrorPayload {
    fn new(error: &CcswError) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            error: ErrorBody {
                kind: error.type_name(),
                message: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_pad_to_the_widest_key_and_value() {
        let rows = vec![
            ("a.b", json!(90.0), false),
            ("a.longer", json!("value"), true),
        ];
        assert_eq!(
            list_lines(&rows),
            vec![
                format!("a.b       90     {}", printer::dimmed("(default)")),
                "a.longer  value".to_string(),
            ]
        );
    }

    #[test]
    fn epilog_lists_every_key_in_registry_order() {
        let text = epilog();
        let keys: Vec<&str> = text
            .lines()
            .skip(1)
            .take(SETTING_SPECS.len())
            .map(|line| line.trim_start().split("  ").next().unwrap())
            .collect();
        let expected: Vec<&str> = SETTING_SPECS.iter().map(|spec| spec.key).collect();
        assert_eq!(keys, expected);
        assert!(text.contains("\nExamples:\n"));
    }

    #[test]
    fn error_envelope_shape() {
        let text = serde_json::to_string(&ErrorPayload::new(&CcswError::config("x"))).unwrap();
        assert_eq!(
            text,
            r#"{"schemaVersion":2,"error":{"type":"ConfigError","message":"x"}}"#
        );
    }
}
