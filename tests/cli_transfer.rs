//! Export / import through the built binary: a mixed roster round trip and a
//! cswap export piped through stdin.

mod support;

use serde_json::json;
use support::{Cli, read_json};

#[test]
fn export_and_import_a_mixed_roster_through_the_binary() {
    let source = Cli::new();
    source.add_chatgpt("alice@example.com", "acct-alice", "rt-a");
    source.add_claude("one@example.com", "org-1", "Acme", "crt-1");
    let path = source.root.path().join("backup.ccsw");
    let path_str = path.to_str().unwrap();

    let run = source.run(&["export", path_str]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert_eq!(
        run.stderr,
        format!("Exported 2 account(s) to {}\n", path.display())
    );
    let value = read_json(&path);
    assert_eq!(value["version"], 2);
    assert_eq!(value["activeByProvider"], json!({"claude": 2, "codex": 1}));
    assert_eq!(value["accounts"][0]["provider"], "codex");
    assert_eq!(value["accounts"][1]["provider"], "claude");
    assert_eq!(
        value["accounts"][1]["credentials"]["claudeAiOauth"]["refreshToken"],
        "crt-1"
    );
    assert!(
        value["accounts"][1]["credentials"]
            .get("mcpOAuth")
            .is_none(),
        "machine-scoped siblings never travel"
    );
    assert_eq!(
        value["accounts"][1]["credentials"]["oauthAccount"]["emailAddress"],
        "one@example.com"
    );

    let target = Cli::new();
    let run = target.run(&["import", path_str]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "Imported alice@example.com → slot 1\n\
         Imported one@example.com → slot 2\n\
         Done: 2 imported, 0 overwritten, 0 skipped\n"
    );
    let roster = target.roster();
    assert_eq!(roster["accounts"]["2"]["provider"], "claude");
    assert_eq!(roster["activeByProvider"], json!({"claude": 2, "codex": 1}));
    assert_eq!(target.credential(2), source.credential(2));

    // The imported Claude login is switchable on the target.
    let run = target.run(&["switch", "2"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        target.claude_credentials()["claudeAiOauth"]["refreshToken"],
        "crt-1"
    );
    assert_eq!(
        target.claude_config()["oauthAccount"]["organizationName"],
        "Acme"
    );

    // Importing again skips both; `--force` overwrites both.
    let run = target.run(&["import", path_str]);
    assert_eq!(run.status, 0);
    assert!(
        run.stderr
            .ends_with("Done: 0 imported, 0 overwritten, 2 skipped\n"),
        "{}",
        run.stderr
    );
    let run = target.run(&["--import", path_str, "--force"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("Done: 0 imported, 2 overwritten, 0 skipped\n"),
        "{}",
        run.stderr
    );
}

#[test]
fn imports_a_cswap_export_from_stdin() {
    let cli = Cli::new();
    let cswap = json!({
        "version": 1,
        "exportedAt": "2026-01-01T00:00:00Z",
        "exportedFrom": "macos",
        "swapVersion": "0.25.0",
        "encrypted": false,
        "activeAccountNumber": 1,
        "accounts": [
            {
                "number": 1, "email": "Alice@Example.com", "uuid": "acct-uuid",
                "organizationUuid": "org-a", "organizationName": "Acme",
                "added": "2024-01-01T00:00:00Z",
                "credentials": {"claudeAiOauth": {"accessToken": "at", "refreshToken": "crt-a",
                                "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]}},
                "config": {"oauthAccount": {"emailAddress": "Alice@Example.com", "accountUuid": "acct-uuid",
                           "organizationUuid": "org-a", "organizationName": "Acme"}}
            },
            {
                "number": 2, "email": "api-key-2@token.local", "uuid": "", "organizationUuid": "",
                "organizationName": "", "added": "2024-01-01T00:00:00Z",
                "credentials": "sk-ant-api03-key", "kind": "api_key",
                "config": {"oauthAccount": {"emailAddress": "api-key-2@token.local", "accountUuid": "",
                           "organizationUuid": null, "organizationName": null}}
            }
        ]
    });
    let run = cli.run_with_stdin(&["import", "-"], &cswap.to_string());
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "Imported alice@example.com → slot 1\n\
         Imported api-key-2@token.local → slot 2\n\
         Done: 2 imported, 0 overwritten, 0 skipped\n"
    );
    let roster = cli.roster();
    assert_eq!(roster["accounts"]["1"]["provider"], "claude");
    assert_eq!(roster["accounts"]["1"]["email"], "alice@example.com");
    assert_eq!(roster["accounts"]["2"]["kind"], "api_key");
    assert_eq!(roster["activeByProvider"], json!({"claude": 1}));
    assert_eq!(cli.credential(2)["primaryApiKey"], "sk-ant-api03-key");

    // The same login captured live refreshes slot 1 instead of adding a twin.
    cli.write_claude_live("Alice@Example.com", "org-a", "Acme", "crt-a2");
    let run = cli.run(&["add", "claude"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .starts_with("Updated credentials for Account 1 (alice@example.com [Acme])"),
        "{}",
        run.stdout
    );
    assert_eq!(cli.roster()["sequence"], json!([1, 2]));
    assert_eq!(cli.credential(1)["claudeAiOauth"]["refreshToken"], "crt-a2");

    let run = cli.run(&["list"]);
    assert_eq!(run.status, 0);
    assert!(run.stdout.contains("alice@example.com"), "{}", run.stdout);
}

// ── import --from-cswap ─────────────────────────────────────────────────

/// A claude-swap store at the platform default under `root` (the test HOME
/// is the Cli root, `XDG_DATA_HOME` is unset): `~/.claude-swap-backup` on
/// macOS/Windows, `~/.local/share/claude-swap` on Linux. One `.enc` + config
/// per account. `emails`: (slot, email, refresh token).
fn cswap_store(root: &std::path::Path, emails: &[(u32, &str, &str)]) -> std::path::PathBuf {
    use base64::Engine as _;
    let dir = if cfg!(target_os = "linux") {
        root.join(".local/share/claude-swap")
    } else {
        root.join(".claude-swap-backup")
    };
    std::fs::create_dir_all(dir.join("credentials")).unwrap();
    std::fs::create_dir_all(dir.join("configs")).unwrap();
    let mut accounts = serde_json::Map::new();
    for (slot, email, refresh) in emails {
        accounts.insert(
            slot.to_string(),
            json!({"email": email, "organizationUuid": "org-t", "organizationName": "Team",
                   "uuid": format!("u-{slot}"), "added": "2026-01-01T00:00:00Z"}),
        );
        let creds = json!({"claudeAiOauth": {"accessToken": "at", "refreshToken": refresh,
                           "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]}});
        std::fs::write(
            dir.join("credentials")
                .join(format!(".creds-{slot}-{email}.enc")),
            base64::engine::general_purpose::STANDARD.encode(creds.to_string()),
        )
        .unwrap();
        std::fs::write(
            dir.join("configs")
                .join(format!(".claude-config-{slot}-{email}.json")),
            json!({"oauthAccount": {"emailAddress": email, "organizationUuid": "org-t",
                   "organizationName": "Team", "accountUuid": format!("u-{slot}")}})
            .to_string(),
        )
        .unwrap();
    }
    std::fs::write(
        dir.join("sequence.json"),
        json!({"accounts": accounts, "activeAccountNumber": emails.first().map(|e| e.0)})
            .to_string(),
    )
    .unwrap();
    dir
}

#[test]
fn import_from_cswap_reads_the_default_store_and_retires_it() {
    let cli = Cli::new();
    let dir = cswap_store(
        cli.root.path(),
        &[
            (1, "one@example.com", "crt-1"),
            (2, "two@example.com", "crt-2"),
        ],
    );
    let run = cli.run(&["import", "--from-cswap", "--retire", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let report = run.json();
    assert_eq!(report["schemaVersion"], 2);
    assert_eq!(report["imported"], 2);
    assert_eq!(report["skipped"], 0);
    let retired = report["retired"].as_str().expect("retired path");
    assert!(retired.contains(".migrated-"), "{retired}");
    assert!(!dir.exists(), "the store was renamed");
    assert!(
        std::path::Path::new(retired)
            .join("sequence.json")
            .is_file()
    );
    let roster = cli.roster();
    assert_eq!(roster["accounts"]["1"]["provider"], "claude");
    assert_eq!(roster["accounts"]["1"]["email"], "one@example.com");
    assert_eq!(roster["accounts"]["2"]["email"], "two@example.com");
    assert_eq!(cli.credential(1)["claudeAiOauth"]["refreshToken"], "crt-1");
    // No live Claude login in the test home: the store's active slot seeds the roster.
    assert_eq!(roster["activeByProvider"]["claude"], 1);
}

#[test]
fn a_second_run_after_retire_exits_2() {
    let cli = Cli::new();
    cswap_store(cli.root.path(), &[(1, "one@example.com", "crt-1")]);
    assert_eq!(cli.run(&["import", "--from-cswap", "--retire"]).status, 0);
    let again = cli.run(&["import", "--from-cswap", "--retire", "--json"]);
    assert_eq!(again.status, 2, "{}", again.stderr);
    let report = again.json();
    assert_eq!(report["imported"], 0);
    assert_eq!(report["reason"], "retired");
    assert_eq!(report["retired"], serde_json::Value::Null);
    assert_eq!(
        cli.roster()["accounts"].as_object().unwrap().len(),
        1,
        "nothing imported twice"
    );
}

#[test]
fn import_from_cswap_without_a_store_exits_2_and_says_so() {
    let cli = Cli::new();
    let run = cli.run(&["import", "--from-cswap"]);
    assert_eq!(run.status, 2, "{}", run.stderr);
    assert!(
        run.stderr.contains("no claude-swap store"),
        "{}",
        run.stderr
    );
    let run = cli.run(&["import", "--from-cswap", "--json"]);
    assert_eq!(run.status, 2);
    assert_eq!(run.json()["reason"], "no-store");
}

#[test]
fn import_from_cswap_accepts_an_explicit_directory_and_keeps_it_without_retire() {
    let cli = Cli::new();
    let dir = cswap_store(
        &cli.root.path().join("elsewhere"),
        &[(3, "three@example.com", "crt-3")],
    );
    let run = cli.run(&["import", "--from-cswap", dir.to_str().unwrap()]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    // The importer keeps the store's slot number when it is free, as for
    // any cswap bundle.
    assert!(
        run.stderr.contains("Imported three@example.com → slot 3"),
        "{}",
        run.stderr
    );
    assert!(
        dir.join("sequence.json").is_file(),
        "no --retire → untouched"
    );
}

#[test]
fn all_accounts_already_present_imports_nothing_and_keeps_the_store() {
    let cli = Cli::new();
    let dir = cswap_store(cli.root.path(), &[(1, "one@example.com", "crt-1")]);
    assert_eq!(cli.run(&["import", "--from-cswap"]).status, 0);
    // Same identity again, no --force: the importer skips it.
    let run = cli.run(&["import", "--from-cswap", "--retire", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let report = run.json();
    assert_eq!(report["imported"], 0);
    assert_eq!(report["skipped"], 1);
    assert_eq!(
        report["retired"],
        serde_json::Value::Null,
        "imported 0 → not retired"
    );
    assert!(dir.join("sequence.json").is_file());
}

#[test]
fn a_corrupt_slot_is_reported_and_the_rest_imported() {
    let cli = Cli::new();
    let dir = cswap_store(
        cli.root.path(),
        &[
            (1, "one@example.com", "crt-1"),
            (2, "two@example.com", "crt-2"),
        ],
    );
    std::fs::write(dir.join("credentials/.creds-2-two@example.com.enc"), "@@@").unwrap();
    let run = cli.run(&["import", "--from-cswap", "--json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.json()["imported"], 1);
    assert!(
        run.stderr.contains("Skipping Account-2 (two@example.com)"),
        "{}",
        run.stderr
    );
}
