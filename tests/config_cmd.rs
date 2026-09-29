//! The `config` verb through `cswitch::cli::config::run_with`, with stdout and
//! stderr captured and the store in a temp dir.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use cswitch::cli::config::run_with;
use cswitch::paths::Paths;

const KEYS: [&str; 9] = [
    "autoswitch.threshold",
    "autoswitch.intervalSeconds",
    "autoswitch.cooldownSeconds",
    "autoswitch.hysteresisPct",
    "autoswitch.strategy",
    "autoswitch.includeApiKeyAccounts",
    "autoswitch.unhealthyTicks",
    "autoswitch.model",
    "ui.theme",
];
const DEFAULTS: [&str; 9] = [
    "90", "60", "300", "10", "best", "false", "3", "(none)", "auto",
];

struct Fx {
    _dir: tempfile::TempDir,
    paths: Paths,
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_values(
        Some(dir.path().join("store")),
        Some(dir.path().join("codex")),
        dir.path(),
    )
    .unwrap();
    Fx { paths, _dir: dir }
}

fn run(fx: &Fx, args: &[&str]) -> (i32, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_with(
        args.iter().map(|arg| arg.to_string()).collect(),
        &fx.paths,
        &mut out,
        &mut err,
    );
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn settings_path(fx: &Fx) -> PathBuf {
    std::path::absolute(fx.paths.settings_file()).unwrap()
}

fn raw_settings(fx: &Fx) -> Value {
    serde_json::from_slice(&fs::read(fx.paths.settings_file()).unwrap()).unwrap()
}

#[test]
fn list_shows_every_key_with_defaults_marked() {
    let fx = fixture();
    let (code, out, err) = run(&fx, &["list"]);
    assert_eq!(code, 0);
    assert_eq!(err, "");
    let expected: Vec<String> = KEYS
        .iter()
        .zip(DEFAULTS)
        .map(|(key, value)| format!("{key:<32}  {value:<6}  (default)"))
        .collect();
    assert_eq!(out.lines().collect::<Vec<_>>(), expected);
    assert!(out.contains("autoswitch.includeApiKeyAccounts  false   (default)\n"));
    assert_eq!(run(&fx, &[]).1, out, "list is the default action");

    run(&fx, &["set", "autoswitch.threshold", "80"]);
    let (_, out, _) = run(&fx, &["list"]);
    let first = out.lines().next().unwrap();
    assert_eq!(
        first,
        format!("{:<32}  {:<6}", "autoswitch.threshold", "80")
    );
    assert!(out.lines().nth(1).unwrap().ends_with("(default)"));
}

#[test]
fn list_json_payload() {
    let fx = fixture();
    let (code, out, err) = run(&fx, &["list", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(err, "");
    assert!(
        out.starts_with("{\n  \"schemaVersion\": 1,\n  \"path\": "),
        "{out}"
    );
    assert!(out.ends_with("}\n"));
    let value: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        value["path"].as_str().unwrap(),
        settings_path(&fx).to_str().unwrap()
    );
    let settings = value["settings"].as_array().unwrap();
    assert_eq!(settings.len(), 9);
    assert_eq!(
        settings[0],
        json!({"key": "autoswitch.threshold", "value": 90.0, "isSet": false})
    );
    assert_eq!(
        settings[7],
        json!({"key": "autoswitch.model", "value": null, "isSet": false})
    );
    assert_eq!(settings[6]["value"], 3);
    assert_eq!(
        out.matches("\"value\": 90.0").count(),
        1,
        "floats keep their decimal"
    );
    for args in [&["--json"][..], &["--json", "list"], &["list", "--json"]] {
        assert_eq!(run(&fx, args), (0, out.clone(), String::new()), "{args:?}");
    }

    run(&fx, &["set", "ui.theme", "dark"]);
    let (_, out, _) = run(&fx, &["--json"]);
    let value: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        value["settings"][8],
        json!({"key": "ui.theme", "value": "dark", "isSet": true})
    );
}

#[test]
fn get_prints_the_bare_value_or_json() {
    let fx = fixture();
    assert_eq!(
        run(&fx, &["get", "autoswitch.threshold"]),
        (0, "90\n".into(), String::new())
    );
    assert_eq!(run(&fx, &["get", "autoswitch.model"]).1, "(none)\n");
    let (code, out, err) = run(&fx, &["get", "autoswitch.model", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(err, "");
    assert_eq!(
        out,
        "{\n  \"schemaVersion\": 1,\n  \"key\": \"autoswitch.model\",\n  \"value\": null,\n  \"isSet\": false\n}\n"
    );

    run(&fx, &["set", "autoswitch.model", "all"]);
    assert_eq!(run(&fx, &["get", "autoswitch.model"]).1, "all\n");
    let (_, out, _) = run(&fx, &["--json", "get", "autoswitch.model"]);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        json!({"schemaVersion": 1, "key": "autoswitch.model", "value": "all", "isSet": true})
    );

    // The effective (clamped) value is reported, and a present key counts as set.
    fs::write(
        fx.paths.settings_file(),
        r#"{"autoswitch": {"threshold": 5}}"#,
    )
    .unwrap();
    let (_, out, _) = run(&fx, &["get", "autoswitch.threshold", "--json"]);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        json!({"schemaVersion": 1, "key": "autoswitch.threshold", "value": 50.0, "isSet": true})
    );
    assert_eq!(run(&fx, &["get", "autoswitch.threshold"]).1, "50\n");
}

#[test]
fn set_writes_the_key_and_echoes_it() {
    let fx = fixture();
    assert_eq!(
        run(&fx, &["set", "autoswitch.threshold", "80"]),
        (0, "autoswitch.threshold = 80\n".into(), String::new())
    );
    assert_eq!(
        run(&fx, &["set", "autoswitch.includeApiKeyAccounts", "yes"]).1,
        "autoswitch.includeApiKeyAccounts = true\n"
    );
    assert_eq!(
        run(&fx, &["set", "autoswitch.threshold", "99.9"]).1,
        "autoswitch.threshold = 99.9\n"
    );
    assert_eq!(
        raw_settings(&fx),
        json!({"schemaVersion": 1, "autoswitch": {"threshold": 99.9, "includeApiKeyAccounts": true}})
    );
}

#[test]
fn handled_errors_exit_1_with_the_message() {
    let fx = fixture();
    assert_eq!(
        run(&fx, &["set", "autoswitch.threshold", "40"]),
        (
            1,
            String::new(),
            "Error: autoswitch.threshold must be between 50 and 99.9\n".into()
        )
    );
    let (code, out, err) = run(&fx, &["get", "bogus"]);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(
        err.starts_with("Error: unknown setting 'bogus'\nValid keys: autoswitch.threshold, "),
        "{err}"
    );
    assert_eq!(run(&fx, &["unset", "bogus"]).0, 1);
    assert_eq!(
        run(&fx, &["set", "ui.theme", "neon"]).2,
        "Error: ui.theme must be one of: dark, light, auto\n"
    );
    assert!(!fx.paths.settings_file().exists(), "nothing was written");
}

#[test]
fn json_errors_use_the_envelope_on_stdout() {
    let fx = fixture();
    let (code, out, err) = run(&fx, &["get", "bogus", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(err, "");
    assert!(
        out.starts_with(
            "{\n  \"schemaVersion\": 1,\n  \"error\": {\n    \"type\": \"ConfigError\",\n    \"message\": \"unknown setting 'bogus'\\nValid keys: "
        ),
        "{out}"
    );
    let value: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["error"]["type"], "ConfigError");
    assert!(out.ends_with("}\n"));
}

#[test]
fn unset_restores_the_default_or_says_so() {
    let fx = fixture();
    assert_eq!(
        run(&fx, &["unset", "autoswitch.threshold"]),
        (
            0,
            String::new(),
            "autoswitch.threshold is not set; nothing to do\n".into()
        )
    );
    run(&fx, &["set", "autoswitch.threshold", "80"]);
    run(&fx, &["set", "autoswitch.model", "all"]);
    assert_eq!(
        run(&fx, &["unset", "autoswitch.threshold"]),
        (
            0,
            "autoswitch.threshold unset (default: 90)\n".into(),
            String::new()
        )
    );
    assert_eq!(
        run(&fx, &["unset", "autoswitch.model"]).1,
        "autoswitch.model unset (default: (none))\n"
    );
    assert_eq!(run(&fx, &["get", "autoswitch.threshold"]).1, "90\n");
    assert_eq!(raw_settings(&fx), json!({"schemaVersion": 1}));
}

#[test]
fn path_prints_the_absolute_settings_path() {
    let fx = fixture();
    assert_eq!(
        run(&fx, &["path"]),
        (
            0,
            format!("{}\n", settings_path(&fx).display()),
            String::new()
        )
    );
    assert!(!fx.paths.settings_file().exists());
}

#[test]
fn json_is_only_for_list_and_get() {
    let fx = fixture();
    for args in [
        &["set", "ui.theme", "dark", "--json"][..],
        &["--json", "set", "ui.theme", "dark"],
        &["unset", "ui.theme", "--json"],
        &["--json", "path"],
    ] {
        let (code, out, err) = run(&fx, args);
        assert_eq!(code, 2, "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert!(
            err.contains("--json can only be used with list or get"),
            "{args:?}: {err}"
        );
    }
    assert!(!fx.paths.settings_file().exists());
}

#[test]
fn usage_errors_exit_2() {
    let fx = fixture();
    let (code, out, err) = run(&fx, &["bogus"]);
    assert_eq!(code, 2);
    assert_eq!(out, "");
    assert!(err.contains("bogus"), "{err}");
    let (code, _, err) = run(&fx, &["get"]);
    assert_eq!(code, 2);
    assert!(err.contains("KEY"), "{err}");
    let (code, _, err) = run(&fx, &["set", "ui.theme"]);
    assert_eq!(code, 2);
    assert!(err.contains("VALUE"), "{err}");
    assert_eq!(run(&fx, &["--nope"]).0, 2);
    assert_eq!(run(&fx, &["path", "extra"]).0, 2);
}

#[test]
fn help_lists_the_keys_and_examples() {
    let fx = fixture();
    for flag in ["-h", "--help"] {
        let (code, out, err) = run(&fx, &[flag]);
        assert_eq!(code, 0, "{flag}");
        assert_eq!(err, "");
        assert!(
            out.contains("Read and edit cswitch settings (settings.json in the backup root)."),
            "{out}"
        );
        assert!(out.contains("\nKeys:\n"), "{out}");
        for (key, help, default) in [
            (
                "autoswitch.threshold",
                "Switch when the binding 5h/7d window reaches this pct",
                "90",
            ),
            (
                "autoswitch.model",
                "Also switch on these models' weekly limits (e.g. Fable, Fable,Opus, or all)",
                "(none)",
            ),
            (
                "ui.theme",
                "Color theme; auto follows the terminal background",
                "auto",
            ),
        ] {
            let line = format!("\n  {key:<34}  {help} (default {default})\n");
            assert!(out.contains(&line), "{key}: {out}");
        }
        assert!(
            out.contains(
                "\nExamples:\n  cswitch config                              # list effective settings\n  cswitch config get autoswitch.threshold\n  cswitch config set autoswitch.threshold 80\n  cswitch config unset autoswitch.threshold   # back to the default\n  cswitch config path                         # where settings.json lives"
            ),
            "{out}"
        );
        for verb in ["list", "get", "set", "unset", "path"] {
            assert!(out.contains(&format!("\n  {verb} ")), "{verb}: {out}");
        }
        assert!(out.contains("--json"));
        assert!(out.contains("--debug"));
    }
}

#[test]
fn debug_flag_is_accepted_anywhere() {
    let fx = fixture();
    assert_eq!(run(&fx, &["--debug", "list"]).0, 0);
    assert_eq!(run(&fx, &["list", "--debug"]).0, 0);
    assert_eq!(run(&fx, &["--debug"]).0, 0);
    assert_eq!(run(&fx, &["get", "ui.theme", "--debug", "--json"]).0, 0);
}
