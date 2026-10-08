//! `settings.json`, the setting registry, and the `config` command helpers.
//!
//! Reads are forgiving (a broken file or value falls back to the default, clamped);
//! `config set` is strict and writes only the one key it was given.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::errors::{CcswError, Result};
use crate::fsutil::{read_json, write_json_private};
use crate::paths::Paths;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingKind {
    Float {
        lo: f64,
        hi: f64,
        default: f64,
    },
    Int {
        lo: i64,
        hi: i64,
        default: i64,
    },
    Bool {
        default: bool,
    },
    Choice {
        choices: &'static [&'static str],
        default: &'static str,
    },
    /// Free text; unset by default.
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingSpec {
    /// Dotted key: `<section>.<field>`.
    pub key: &'static str,
    pub kind: SettingKind,
    pub help: &'static str,
}

pub const STRATEGY_CHOICES: &[&str] = &["best", "consume-first"];
pub const THEME_CHOICES: &[&str] = &["dark", "light", "auto"];

/// The nine settings, in registry (help) order.
pub const SETTING_SPECS: &[SettingSpec] = &[
    SettingSpec {
        key: "autoswitch.threshold",
        kind: SettingKind::Float {
            lo: 50.0,
            hi: 99.9,
            default: 90.0,
        },
        help: "Switch when the binding 5h/7d window reaches this pct",
    },
    SettingSpec {
        key: "autoswitch.intervalSeconds",
        kind: SettingKind::Float {
            lo: 15.0,
            hi: 3600.0,
            default: 60.0,
        },
        help: "Poll interval for the ccsw auto loop, in seconds",
    },
    SettingSpec {
        key: "autoswitch.cooldownSeconds",
        kind: SettingKind::Float {
            lo: 0.0,
            hi: 86400.0,
            default: 300.0,
        },
        help: "Minimum seconds between proactive switches",
    },
    SettingSpec {
        key: "autoswitch.hysteresisPct",
        kind: SettingKind::Float {
            lo: 0.0,
            hi: 50.0,
            default: 10.0,
        },
        help: "A target must beat the active account by this many pct",
    },
    SettingSpec {
        key: "autoswitch.strategy",
        kind: SettingKind::Choice {
            choices: STRATEGY_CHOICES,
            default: "best",
        },
        help: "How auto-switch picks the target account",
    },
    SettingSpec {
        key: "autoswitch.includeApiKeyAccounts",
        kind: SettingKind::Bool { default: false },
        help: "Allow rotating onto managed API-key accounts (bill per token)",
    },
    SettingSpec {
        key: "autoswitch.unhealthyTicks",
        kind: SettingKind::Int {
            lo: 1,
            hi: 100,
            default: 3,
        },
        help: "Consecutive failed polls before an account is unhealthy",
    },
    SettingSpec {
        key: "autoswitch.model",
        kind: SettingKind::Text,
        help: "Also switch on these models' weekly limits (e.g. Fable, Fable,Opus, or all)",
    },
    SettingSpec {
        key: "ui.theme",
        kind: SettingKind::Choice {
            choices: THEME_CHOICES,
            default: "auto",
        },
        help: "Color theme; auto follows the terminal background",
    },
];

impl SettingSpec {
    pub fn find(key: &str) -> Option<&'static SettingSpec> {
        SETTING_SPECS.iter().find(|spec| spec.key == key)
    }

    pub fn section_and_field(&self) -> (&'static str, &'static str) {
        self.key.split_once('.').unwrap_or((self.key, ""))
    }

    pub fn default_value(&self) -> Value {
        match self.kind {
            SettingKind::Float { default, .. } => Value::from(default),
            SettingKind::Int { default, .. } => Value::from(default),
            SettingKind::Bool { default } => Value::from(default),
            SettingKind::Choice { default, .. } => Value::from(default),
            SettingKind::Text => Value::Null,
        }
    }

    /// Strict parsing for `config set`.
    pub fn parse(&self, raw: &str) -> Result<Value> {
        let key = self.key;
        let text = raw.trim();
        let value = match self.kind {
            SettingKind::Float { lo, hi, .. } => {
                let number: f64 = text.parse().map_err(|_| {
                    CcswError::config(format!("{key} expects a number, got '{text}'"))
                })?;
                if !(number >= lo && number <= hi) {
                    return Err(out_of_range(key, lo, hi));
                }
                Value::from(number)
            }
            SettingKind::Int { lo, hi, .. } => {
                let number: i64 = text.parse().map_err(|_| {
                    CcswError::config(format!("{key} expects an integer, got '{text}'"))
                })?;
                if number < lo || number > hi {
                    return Err(out_of_range(key, lo as f64, hi as f64));
                }
                Value::from(number)
            }
            SettingKind::Bool { .. } => match text.to_lowercase().as_str() {
                "true" | "1" | "yes" => Value::Bool(true),
                "false" | "0" | "no" => Value::Bool(false),
                _ => {
                    return Err(CcswError::config(format!(
                        "{key} expects true or false (or 1/0, yes/no), got '{text}'"
                    )));
                }
            },
            SettingKind::Choice { choices, .. } => {
                if !choices.contains(&text) {
                    return Err(CcswError::config(format!(
                        "{key} must be one of: {}",
                        choices.join(", ")
                    )));
                }
                Value::from(text)
            }
            SettingKind::Text => {
                if text.is_empty() {
                    return Err(CcswError::config(format!(
                        "{key} expects a non-empty value; use 'ccsw config unset {key}' to clear it"
                    )));
                }
                Value::from(text)
            }
        };
        Ok(value)
    }

    /// Forgiving coercion for reads: bad types and unknown choices fall back to the
    /// default, numbers are clamped into range.
    pub fn coerce(&self, raw: Option<&Value>) -> Value {
        match self.kind {
            SettingKind::Float { lo, hi, default } => match raw.and_then(Value::as_f64) {
                Some(n) if n.is_finite() => Value::from(n.clamp(lo, hi)),
                _ => Value::from(default),
            },
            SettingKind::Int { lo, hi, default } => match raw.and_then(Value::as_f64) {
                Some(n) if n.is_finite() => Value::from(n.clamp(lo as f64, hi as f64) as i64),
                _ => Value::from(default),
            },
            SettingKind::Bool { default } => Value::Bool(raw.map_or(default, truthy)),
            SettingKind::Choice { choices, default } => match raw {
                None => Value::from(default),
                Some(Value::String(s)) if choices.contains(&s.as_str()) => Value::from(s.as_str()),
                Some(other) => {
                    tracing::warn!(
                        "settings.json: unsupported {} {}; using '{default}'",
                        self.key,
                        python_repr(other)
                    );
                    Value::from(default)
                }
            },
            SettingKind::Text => match raw {
                Some(Value::String(s)) if !s.is_empty() => Value::from(s.as_str()),
                _ => Value::Null,
            },
        }
    }
}

fn out_of_range(key: &str, lo: f64, hi: f64) -> CcswError {
    CcswError::config(format!(
        "{key} must be between {} and {}",
        format_number(lo),
        format_number(hi)
    ))
}

// Python truthiness, which is what cswap applies to bool settings on read.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn python_repr(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{s}'"),
        other => other.to_string(),
    }
}

/// Whole numbers print without decimals (`90.0` → `90`).
fn format_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// `(none)`, `true`/`false`, whole floats without decimals, else the plain text.
pub fn format_setting_value(value: &Value) -> String {
    match value {
        Value::Null => "(none)".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), format_number),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Comma-split, trim, drop empties, dedupe case-insensitively keeping the first
/// spelling and order.
pub fn parse_model_names(raw: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut names = Vec::new();
    for part in raw.split(',') {
        let name = part.trim();
        if name.is_empty() {
            continue;
        }
        let folded = name.to_lowercase();
        if seen.contains(&folded) {
            continue;
        }
        seen.push(folded);
        names.push(name.to_string());
    }
    names
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoSwitchSettings {
    pub threshold: f64,
    pub interval_seconds: f64,
    pub cooldown_seconds: f64,
    pub hysteresis_pct: f64,
    pub strategy: String,
    pub include_api_key_accounts: bool,
    pub unhealthy_ticks: u32,
    pub model: Option<String>,
}

/// `ccsw auto` flags; only `Some` values override the file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AutoSwitchOverrides {
    pub threshold: Option<f64>,
    pub interval_seconds: Option<f64>,
    pub cooldown_seconds: Option<f64>,
    pub include_api_key_accounts: Option<bool>,
    pub model: Option<String>,
    pub strategy: Option<String>,
}

impl AutoSwitchSettings {
    pub fn merged_with_cli(&self, overrides: &AutoSwitchOverrides) -> Self {
        Self {
            threshold: overrides.threshold.unwrap_or(self.threshold),
            interval_seconds: overrides.interval_seconds.unwrap_or(self.interval_seconds),
            cooldown_seconds: overrides.cooldown_seconds.unwrap_or(self.cooldown_seconds),
            hysteresis_pct: self.hysteresis_pct,
            strategy: overrides
                .strategy
                .clone()
                .unwrap_or_else(|| self.strategy.clone()),
            include_api_key_accounts: overrides
                .include_api_key_accounts
                .unwrap_or(self.include_api_key_accounts),
            unhealthy_ticks: self.unhealthy_ticks,
            model: overrides.model.clone().or_else(|| self.model.clone()),
        }
        .clamped()
    }

    /// Re-apply the registry's ranges and choices.
    pub fn clamped(self) -> Self {
        let raw = self.to_values();
        Settings::from_effective(&coerce_all(|spec| raw.get(spec.key).cloned())).autoswitch
    }

    /// The configured model pools (`parse_model_names` of `model`).
    pub fn model_names(&self) -> Vec<String> {
        parse_model_names(self.model.as_deref().unwrap_or(""))
    }

    fn to_values(&self) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("autoswitch.threshold", Value::from(self.threshold)),
            (
                "autoswitch.intervalSeconds",
                Value::from(self.interval_seconds),
            ),
            (
                "autoswitch.cooldownSeconds",
                Value::from(self.cooldown_seconds),
            ),
            ("autoswitch.hysteresisPct", Value::from(self.hysteresis_pct)),
            ("autoswitch.strategy", Value::from(self.strategy.as_str())),
            (
                "autoswitch.includeApiKeyAccounts",
                Value::from(self.include_api_key_accounts),
            ),
            (
                "autoswitch.unhealthyTicks",
                Value::from(self.unhealthy_ticks),
            ),
            (
                "autoswitch.model",
                self.model.as_deref().map_or(Value::Null, Value::from),
            ),
        ])
    }
}

impl Default for AutoSwitchSettings {
    fn default() -> Self {
        Settings::default().autoswitch
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub autoswitch: AutoSwitchSettings,
    /// `dark`, `light` or `auto`.
    pub theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_effective(&coerce_all(|_| None))
    }
}

impl Settings {
    /// Forgiving load: a missing or corrupt file, section or value yields defaults.
    pub fn load(paths: &Paths) -> Self {
        let root = read_root_forgiving(paths);
        Self::from_effective(&coerce_all(|spec| raw_value(&root, spec).cloned()))
    }

    fn from_effective(values: &BTreeMap<&'static str, Value>) -> Self {
        let f64_of = |key: &str| values.get(key).and_then(Value::as_f64).unwrap_or_default();
        let str_of = |key: &str| {
            values
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Self {
            autoswitch: AutoSwitchSettings {
                threshold: f64_of("autoswitch.threshold"),
                interval_seconds: f64_of("autoswitch.intervalSeconds"),
                cooldown_seconds: f64_of("autoswitch.cooldownSeconds"),
                hysteresis_pct: f64_of("autoswitch.hysteresisPct"),
                strategy: str_of("autoswitch.strategy"),
                include_api_key_accounts: values
                    .get("autoswitch.includeApiKeyAccounts")
                    .and_then(Value::as_bool)
                    .unwrap_or_default(),
                unhealthy_ticks: values
                    .get("autoswitch.unhealthyTicks")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as u32,
                model: values
                    .get("autoswitch.model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            },
            theme: str_of("ui.theme"),
        }
    }
}

fn coerce_all(raw: impl Fn(&SettingSpec) -> Option<Value>) -> BTreeMap<&'static str, Value> {
    SETTING_SPECS
        .iter()
        .map(|spec| (spec.key, spec.coerce(raw(spec).as_ref())))
        .collect()
}

fn raw_value<'a>(root: &'a Map<String, Value>, spec: &SettingSpec) -> Option<&'a Value> {
    let (section, field) = spec.section_and_field();
    root.get(section)?.get(field)
}

fn read_root_forgiving(paths: &Paths) -> Map<String, Value> {
    let path = paths.settings_file();
    match read_json(&path) {
        Ok(Some(Value::Object(map))) => map,
        Ok(Some(_)) => {
            tracing::warn!("{}: not a JSON object; using defaults", path.display());
            Map::new()
        }
        Ok(None) => Map::new(),
        Err(err) => {
            tracing::warn!("{}: {err}; using defaults", path.display());
            Map::new()
        }
    }
}

/// Strict read for writers: the file is left untouched when it cannot be trusted.
fn read_root_strict(paths: &Paths) -> Result<Map<String, Value>> {
    let path = paths.settings_file();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(err) => {
            return Err(CcswError::config(format!(
                "could not read {}: {err}",
                path.display()
            )));
        }
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|err| {
        CcswError::config(format!(
            "{} is not valid JSON ({err}); fix or delete it before changing settings",
            path.display()
        ))
    })?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(CcswError::config(format!(
            "{} is not a JSON object; fix or delete it before changing settings",
            path.display()
        ))),
    }
}

fn write_root(paths: &Paths, root: Map<String, Value>) -> Result<()> {
    let path = paths.settings_file();
    write_json_private(&path, &Value::Object(root))
        .map_err(|err| CcswError::config(format!("{}: {err}", path.display())))
}

fn spec_or_unknown(key: &str) -> Result<&'static SettingSpec> {
    SettingSpec::find(key).ok_or_else(|| {
        let keys: Vec<&str> = SETTING_SPECS.iter().map(|spec| spec.key).collect();
        CcswError::config(format!(
            "unknown setting '{key}'\nValid keys: {}",
            keys.join(", ")
        ))
    })
}

pub fn config_path(paths: &Paths) -> PathBuf {
    paths.settings_file()
}

/// Effective value and whether the key is literally present in the file.
pub fn config_get(paths: &Paths, key: &str) -> Result<(Value, bool)> {
    let spec = spec_or_unknown(key)?;
    let root = read_root_forgiving(paths);
    let raw = raw_value(&root, spec);
    Ok((spec.coerce(raw), raw.is_some()))
}

/// Every setting in registry order: `(key, effective value, is_set)`.
pub fn config_list(paths: &Paths) -> Result<Vec<(&'static str, Value, bool)>> {
    let root = read_root_forgiving(paths);
    Ok(SETTING_SPECS
        .iter()
        .map(|spec| {
            let raw = raw_value(&root, spec);
            (spec.key, spec.coerce(raw), raw.is_some())
        })
        .collect())
}

/// Validate and write one key (plus `schemaVersion`), preserving everything else.
/// Returns the formatted value.
pub fn config_set(paths: &Paths, key: &str, value: &str) -> Result<String> {
    let spec = spec_or_unknown(key)?;
    let parsed = spec.parse(value)?;
    let mut root = read_root_strict(paths)?;
    root.entry("schemaVersion").or_insert(Value::from(1));
    let (section, field) = spec.section_and_field();
    let section_value = root
        .entry(section)
        .or_insert_with(|| Value::Object(Map::new()));
    if !section_value.is_object() {
        *section_value = Value::Object(Map::new());
    }
    if let Value::Object(section_map) = section_value {
        section_map.insert(field.to_string(), parsed.clone());
    }
    write_root(paths, root)?;
    Ok(format_setting_value(&parsed))
}

/// Remove one key (and its section when that becomes empty). Returns whether it was set.
pub fn config_unset(paths: &Paths, key: &str) -> Result<bool> {
    let spec = spec_or_unknown(key)?;
    let mut root = read_root_strict(paths)?;
    let (section, field) = spec.section_and_field();
    let Some(Value::Object(section_map)) = root.get_mut(section) else {
        return Ok(false);
    };
    if section_map.remove(field).is_none() {
        return Ok(false);
    }
    if section_map.is_empty() {
        root.remove(section);
    }
    write_root(paths, root)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::temp_store;
    use serde_json::json;

    fn write_settings(paths: &Paths, value: &Value) {
        fs::create_dir_all(&paths.backup_root).unwrap();
        fs::write(paths.settings_file(), value.to_string()).unwrap();
    }

    fn read_settings(paths: &Paths) -> Value {
        read_json(&paths.settings_file()).unwrap().unwrap()
    }

    #[test]
    fn registry_matches_the_contract() {
        let keys: Vec<&str> = SETTING_SPECS.iter().map(|s| s.key).collect();
        assert_eq!(
            keys,
            [
                "autoswitch.threshold",
                "autoswitch.intervalSeconds",
                "autoswitch.cooldownSeconds",
                "autoswitch.hysteresisPct",
                "autoswitch.strategy",
                "autoswitch.includeApiKeyAccounts",
                "autoswitch.unhealthyTicks",
                "autoswitch.model",
                "ui.theme",
            ]
        );
        let defaults: Vec<String> = SETTING_SPECS
            .iter()
            .map(|s| format_setting_value(&s.default_value()))
            .collect();
        assert_eq!(
            defaults,
            [
                "90", "60", "300", "10", "best", "false", "3", "(none)", "auto"
            ]
        );
        assert_eq!(
            SettingSpec::find("ui.theme").unwrap().section_and_field(),
            ("ui", "theme")
        );
        assert!(SettingSpec::find("nope").is_none());
    }

    #[test]
    fn defaults_and_forgiving_load() {
        let (_dir, store) = temp_store();
        let defaults = Settings::load(&store.paths);
        assert_eq!(defaults, Settings::default());
        assert_eq!(defaults.autoswitch.threshold, 90.0);
        assert_eq!(defaults.autoswitch.interval_seconds, 60.0);
        assert_eq!(defaults.autoswitch.cooldown_seconds, 300.0);
        assert_eq!(defaults.autoswitch.hysteresis_pct, 10.0);
        assert_eq!(defaults.autoswitch.strategy, "best");
        assert!(!defaults.autoswitch.include_api_key_accounts);
        assert_eq!(defaults.autoswitch.unhealthy_ticks, 3);
        assert_eq!(defaults.autoswitch.model, None);
        assert_eq!(defaults.theme, "auto");
        assert_eq!(AutoSwitchSettings::default(), defaults.autoswitch);

        write_settings(
            &store.paths,
            &json!({"autoswitch": "nope", "ui": {"theme": "neon"}}),
        );
        assert_eq!(Settings::load(&store.paths), Settings::default());

        fs::write(store.paths.settings_file(), "{broken").unwrap();
        assert_eq!(Settings::load(&store.paths), Settings::default());

        write_settings(
            &store.paths,
            &json!({
                "autoswitch": {
                    "threshold": 20, "intervalSeconds": 99999, "cooldownSeconds": "x",
                    "hysteresisPct": true, "strategy": "soonest-reset",
                    "includeApiKeyAccounts": "yes", "unhealthyTicks": 7.9, "model": ""
                },
                "ui": {"theme": "light"}
            }),
        );
        let loaded = Settings::load(&store.paths);
        assert_eq!(loaded.autoswitch.threshold, 50.0, "clamped up");
        assert_eq!(loaded.autoswitch.interval_seconds, 3600.0, "clamped down");
        assert_eq!(
            loaded.autoswitch.cooldown_seconds, 300.0,
            "bad type → default"
        );
        assert_eq!(loaded.autoswitch.hysteresis_pct, 10.0, "bool → default");
        assert_eq!(
            loaded.autoswitch.strategy, "best",
            "unknown choice → default"
        );
        assert!(loaded.autoswitch.include_api_key_accounts, "truthy string");
        assert_eq!(loaded.autoswitch.unhealthy_ticks, 7, "int cast");
        assert_eq!(loaded.autoswitch.model, None, "empty string → unset");
        assert_eq!(loaded.theme, "light");
    }

    #[test]
    fn merged_with_cli_overlays_and_reclamps() {
        let base = AutoSwitchSettings::default();
        assert_eq!(base.merged_with_cli(&AutoSwitchOverrides::default()), base);
        let merged = base.merged_with_cli(&AutoSwitchOverrides {
            threshold: Some(120.0),
            interval_seconds: Some(1.0),
            cooldown_seconds: None,
            include_api_key_accounts: Some(true),
            model: Some("Spark, spark,Codex".into()),
            strategy: Some("bogus".into()),
        });
        assert_eq!(merged.threshold, 99.9);
        assert_eq!(merged.interval_seconds, 15.0);
        assert_eq!(merged.cooldown_seconds, 300.0);
        assert!(merged.include_api_key_accounts);
        assert_eq!(merged.strategy, "best");
        assert_eq!(merged.model_names(), vec!["Spark", "Codex"]);
        let mut nan = base.clone();
        nan.threshold = f64::NAN;
        nan.model = Some("  ".into());
        let clamped = nan.clamped();
        assert_eq!(clamped.threshold, 90.0);
        assert_eq!(
            clamped.model,
            Some("  ".to_string()),
            "whitespace is kept; only empty is unset"
        );
    }

    #[test]
    fn model_name_parsing() {
        assert_eq!(parse_model_names("Opus, opus,Fable"), vec!["Opus", "Fable"]);
        assert_eq!(parse_model_names(""), Vec::<String>::new());
        assert_eq!(parse_model_names(" , ,all"), vec!["all"]);
    }

    #[test]
    fn format_values_like_cswap() {
        assert_eq!(format_setting_value(&Value::Null), "(none)");
        assert_eq!(format_setting_value(&json!(true)), "true");
        assert_eq!(format_setting_value(&json!(90.0)), "90");
        assert_eq!(format_setting_value(&json!(99.9)), "99.9");
        assert_eq!(format_setting_value(&json!(3)), "3");
        assert_eq!(format_setting_value(&json!("best")), "best");
    }

    #[test]
    fn strict_parse_messages() {
        let spec = |key: &str| SettingSpec::find(key).unwrap();
        assert_eq!(
            spec("autoswitch.threshold").parse(" 80 ").unwrap(),
            json!(80.0)
        );
        assert_eq!(
            spec("autoswitch.threshold").parse("99.9").unwrap(),
            json!(99.9)
        );
        let cases = [
            (
                "autoswitch.threshold",
                "40",
                "autoswitch.threshold must be between 50 and 99.9",
            ),
            (
                "autoswitch.threshold",
                "abc",
                "autoswitch.threshold expects a number, got 'abc'",
            ),
            (
                "autoswitch.intervalSeconds",
                "5",
                "autoswitch.intervalSeconds must be between 15 and 3600",
            ),
            (
                "autoswitch.cooldownSeconds",
                "-1",
                "autoswitch.cooldownSeconds must be between 0 and 86400",
            ),
            (
                "autoswitch.hysteresisPct",
                "51",
                "autoswitch.hysteresisPct must be between 0 and 50",
            ),
            (
                "autoswitch.unhealthyTicks",
                "1.5",
                "autoswitch.unhealthyTicks expects an integer, got '1.5'",
            ),
            (
                "autoswitch.unhealthyTicks",
                "0",
                "autoswitch.unhealthyTicks must be between 1 and 100",
            ),
            (
                "autoswitch.includeApiKeyAccounts",
                "maybe",
                "autoswitch.includeApiKeyAccounts expects true or false (or 1/0, yes/no), got 'maybe'",
            ),
            (
                "autoswitch.strategy",
                "Best",
                "autoswitch.strategy must be one of: best, consume-first",
            ),
            (
                "ui.theme",
                "neon",
                "ui.theme must be one of: dark, light, auto",
            ),
            (
                "autoswitch.model",
                "  ",
                "autoswitch.model expects a non-empty value; use 'ccsw config unset autoswitch.model' to clear it",
            ),
        ];
        for (key, raw, message) in cases {
            let err = spec(key).parse(raw).unwrap_err();
            assert_eq!(err.type_name(), "ConfigError");
            assert_eq!(err.to_string(), message, "{key}={raw}");
        }
        for (raw, expected) in [
            ("TRUE", true),
            ("1", true),
            ("Yes", true),
            ("false", false),
            ("0", false),
            ("no", false),
        ] {
            assert_eq!(
                spec("autoswitch.includeApiKeyAccounts").parse(raw).unwrap(),
                json!(expected)
            );
        }
        assert_eq!(
            spec("autoswitch.unhealthyTicks").parse("7").unwrap(),
            json!(7)
        );
        assert_eq!(
            spec("autoswitch.model").parse(" all ").unwrap(),
            json!("all")
        );
    }

    #[test]
    fn config_set_writes_only_that_key_and_preserves_unknowns() {
        let (_dir, store) = temp_store();
        let paths = &store.paths;
        assert_eq!(
            config_set(paths, "autoswitch.threshold", "80").unwrap(),
            "80"
        );
        assert_eq!(
            read_settings(paths),
            json!({"schemaVersion": 1, "autoswitch": {"threshold": 80.0}})
        );
        write_settings(
            paths,
            &json!({"schemaVersion": 1, "vendor": {"x": 1}, "autoswitch": {"threshold": 80.0, "future": true}, "ui": "broken"}),
        );
        assert_eq!(config_set(paths, "ui.theme", "dark").unwrap(), "dark");
        assert_eq!(
            config_set(paths, "autoswitch.includeApiKeyAccounts", "yes").unwrap(),
            "true"
        );
        let raw = read_settings(paths);
        assert_eq!(raw["vendor"], json!({"x": 1}));
        assert_eq!(raw["autoswitch"]["future"], true);
        assert_eq!(raw["autoswitch"]["threshold"], 80.0);
        assert_eq!(raw["autoswitch"]["includeApiKeyAccounts"], true);
        assert_eq!(
            raw["ui"],
            json!({"theme": "dark"}),
            "a non-object section is replaced"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(paths.settings_file())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let err = config_set(paths, "bogus", "1").unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown setting 'bogus'\nValid keys: autoswitch.threshold, autoswitch.intervalSeconds, autoswitch.cooldownSeconds, autoswitch.hysteresisPct, autoswitch.strategy, autoswitch.includeApiKeyAccounts, autoswitch.unhealthyTicks, autoswitch.model, ui.theme"
        );
        assert_eq!(
            config_set(paths, "autoswitch.threshold", "40")
                .unwrap_err()
                .to_string(),
            "autoswitch.threshold must be between 50 and 99.9"
        );
    }

    #[test]
    fn config_set_refuses_to_touch_a_corrupt_file() {
        let (_dir, store) = temp_store();
        let paths = &store.paths;
        fs::create_dir_all(&paths.backup_root).unwrap();
        fs::write(paths.settings_file(), "{nope").unwrap();
        let msg = config_set(paths, "ui.theme", "dark")
            .unwrap_err()
            .to_string();
        assert!(msg.starts_with(&format!(
            "{} is not valid JSON (",
            paths.settings_file().display()
        )));
        assert!(msg.ends_with("); fix or delete it before changing settings"));
        assert_eq!(fs::read_to_string(paths.settings_file()).unwrap(), "{nope");
        fs::write(paths.settings_file(), "[]").unwrap();
        assert_eq!(
            config_unset(paths, "ui.theme").unwrap_err().to_string(),
            format!(
                "{} is not a JSON object; fix or delete it before changing settings",
                paths.settings_file().display()
            )
        );
    }

    #[test]
    fn config_get_list_and_unset() {
        let (_dir, store) = temp_store();
        let paths = &store.paths;
        assert_eq!(
            config_get(paths, "autoswitch.threshold").unwrap(),
            (json!(90.0), false)
        );
        assert!(
            config_get(paths, "nope")
                .unwrap_err()
                .to_string()
                .starts_with("unknown setting 'nope'")
        );
        assert!(!config_unset(paths, "autoswitch.threshold").unwrap());
        assert!(
            !paths.settings_file().exists(),
            "unset of an unset key writes nothing"
        );

        config_set(paths, "autoswitch.threshold", "90").unwrap();
        assert_eq!(
            config_get(paths, "autoswitch.threshold").unwrap(),
            (json!(90.0), true),
            "explicit default still counts as set"
        );
        write_settings(
            paths,
            &json!({"schemaVersion": 1, "autoswitch": {"threshold": 5, "model": "Spark"}, "keep": 1}),
        );
        assert_eq!(
            config_get(paths, "autoswitch.threshold").unwrap(),
            (json!(50.0), true),
            "get reports the clamped effective value"
        );
        let listed = config_list(paths).unwrap();
        assert_eq!(listed.len(), 9);
        assert_eq!(listed[0], ("autoswitch.threshold", json!(50.0), true));
        assert_eq!(listed[7], ("autoswitch.model", json!("Spark"), true));
        assert_eq!(listed[8], ("ui.theme", json!("auto"), false));

        assert!(config_unset(paths, "autoswitch.threshold").unwrap());
        assert_eq!(
            read_settings(paths),
            json!({"schemaVersion": 1, "autoswitch": {"model": "Spark"}, "keep": 1})
        );
        assert!(config_unset(paths, "autoswitch.model").unwrap());
        assert_eq!(
            read_settings(paths),
            json!({"schemaVersion": 1, "keep": 1}),
            "empty section dropped"
        );
        assert!(!config_unset(paths, "autoswitch.model").unwrap());
        assert_eq!(config_path(paths), paths.settings_file());
    }
}
