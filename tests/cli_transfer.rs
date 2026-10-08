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
