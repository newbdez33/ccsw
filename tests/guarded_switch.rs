mod support;

use ccsw::errors::CcswError;
use ccsw::paths::Paths;
use ccsw::provider::Provider;
use ccsw::store::Store;
use ccsw::switcher::{SilentUi, Switcher};
use support::Cli;

fn switcher(cli: &Cli) -> Switcher {
    let paths = Paths::from_values(
        Some(cli.ccsw_home.clone()),
        Some(cli.codex_home.clone()),
        Some(cli.claude_home.clone()),
        cli.root.path(),
    )
    .unwrap();
    let mut switcher = Switcher::open(Store::open(paths));
    switcher.ui = Box::new(SilentUi);
    switcher
}

#[test]
fn a_guard_rejects_slot_reuse_without_changing_the_live_login() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "bob", "rt-b");
    let mut switcher = switcher(&cli);
    let revision = switcher.switch_state().unwrap().revision;
    assert_eq!(cli.run(&["swap", "1", "2"]).status, 0);
    let before = cli.live();
    let error = switcher
        .switch_guarded(1, Provider::Codex, &revision, true)
        .unwrap_err();
    assert_eq!(error, CcswError::Conflict("state_changed"));
    assert_eq!(cli.live(), before);
}

#[test]
fn a_guard_requires_acknowledgment_and_rejects_disabled_targets() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "bob", "rt-b");
    let mut switcher = switcher(&cli);
    let revision = switcher.switch_state().unwrap().revision;
    assert_eq!(
        switcher.switch_guarded(1, Provider::Codex, &revision, false),
        Err(CcswError::Conflict("interruption_required"))
    );
    assert_eq!(cli.run(&["disable", "1"]).status, 0);
    let revision = switcher.switch_state().unwrap().revision;
    assert_eq!(
        switcher.switch_guarded(1, Provider::Codex, &revision, true),
        Err(CcswError::Conflict("account_disabled"))
    );
    assert_eq!(cli.live(), cli.credential(2));
}

#[test]
fn a_guard_does_not_replace_an_unmanaged_login() {
    let cli = Cli::new();
    cli.add_claude("alice@example.com", "alice", "Alice", "rt-a");
    cli.write_claude_live("outside@example.com", "outside", "Outside", "rt-outside");
    let mut switcher = switcher(&cli);
    let revision = switcher.switch_state().unwrap().revision;
    let before = cli.claude_credentials();
    assert_eq!(
        switcher.switch_guarded(1, Provider::Claude, &revision, false),
        Err(CcswError::Conflict("unmanaged_login"))
    );
    assert_eq!(cli.claude_credentials(), before);
}

#[test]
fn guarded_claude_switch_uses_the_existing_writer() {
    let cli = Cli::new();
    cli.add_claude("alice@example.com", "alice", "Alice", "rt-a");
    cli.add_claude("bob@example.com", "bob", "Bob", "rt-b");
    let mut switcher = switcher(&cli);
    let revision = switcher.switch_state().unwrap().revision;
    let result = switcher
        .switch_guarded(1, Provider::Claude, &revision, false)
        .unwrap();
    assert_eq!(result.outcome.to.unwrap().number, Some(1));
    assert_eq!(cli.claude_backups().len(), 1);
    assert_ne!(switcher.switch_state().unwrap().revision, revision);
}

#[test]
fn a_guard_rejects_credentials_from_another_account_kind() {
    let cli = Cli::new();
    cli.add_chatgpt("alice@example.com", "alice", "rt-a");
    cli.add_chatgpt("bob@example.com", "bob", "rt-b");
    let mut switcher = switcher(&cli);
    let revision = switcher.switch_state().unwrap().revision;
    std::fs::write(
        cli.credential_path(1),
        support::api_key_auth("unexpected-key").to_string(),
    )
    .unwrap();
    assert_eq!(
        switcher.switch_guarded(1, Provider::Codex, &revision, true),
        Err(CcswError::Conflict("credentials_unavailable"))
    );
    assert_eq!(cli.live(), cli.credential(2));
}

#[test]
fn a_guard_rejects_expired_claude_credentials() {
    let cli = Cli::new();
    cli.add_claude("alice@example.com", "alice", "Alice", "rt-a");
    cli.add_claude("bob@example.com", "bob", "Bob", "rt-b");
    let mut switcher = switcher(&cli);
    let revision = switcher.switch_state().unwrap().revision;
    let mut file = ccsw::claude::credentials::SlotFile::from_value(&cli.credential(1)).unwrap();
    file.credential.0["claudeAiOauth"]["expiresAt"] = serde_json::json!(1);
    std::fs::write(cli.credential_path(1), file.to_value().to_string()).unwrap();
    let before = cli.claude_credentials();
    assert_eq!(
        switcher.switch_guarded(1, Provider::Claude, &revision, false),
        Err(CcswError::Conflict("token_expired"))
    );
    assert_eq!(cli.claude_credentials(), before);
}

#[test]
fn dead_tokens_are_rejected_but_rotation_keeps_the_revision() {
    let cli = Cli::new();
    cli.add_claude("alice@example.com", "alice", "Alice", "rt-a");
    cli.add_claude("bob@example.com", "bob", "Bob", "rt-b");
    let mut switcher = switcher(&cli);
    let revision = switcher.switch_state().unwrap().revision;
    let mut file = ccsw::claude::credentials::SlotFile::from_value(&cli.credential(1)).unwrap();
    let fingerprint = file.credential.fingerprint().unwrap();
    let cache = switcher.store.paths.usage_file();
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(
        cache,
        serde_json::json!({"schemaVersion": 2, "accounts": {"1": {
            "email": "alice@example.com", "accountId": "alice",
            "authDeadStrikes": 1, "struckFingerprint": fingerprint
        }}})
        .to_string(),
    )
    .unwrap();
    assert_eq!(switcher.switch_state().unwrap().revision, revision);
    assert_eq!(
        switcher.switch_guarded(1, Provider::Claude, &revision, false),
        Err(CcswError::Conflict("relogin_required"))
    );
    file.credential.0["claudeAiOauth"]["refreshToken"] = serde_json::json!("rotated");
    std::fs::write(cli.credential_path(1), file.to_value().to_string()).unwrap();
    assert_eq!(switcher.switch_state().unwrap().revision, revision);
    assert!(
        switcher
            .switch_guarded(1, Provider::Claude, &revision, false)
            .unwrap()
            .outcome
            .switched
    );
}
