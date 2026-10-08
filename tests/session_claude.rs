use ccsw::claude::credentials::{ClaudeCredential, OauthAccount, SlotFile};
use ccsw::fsutil::{read_json, write_json_private};
use ccsw::model::{AccountRecord, Roster};
use ccsw::paths::Paths;
use ccsw::provider::Provider;
use ccsw::session::{ShareOptions, prepare_profile};
use ccsw::store::{Store, credentials, roster};
use serde_json::json;

fn fixture() -> (tempfile::TempDir, Store, Roster) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_values(
        Some(dir.path().join("store")),
        Some(dir.path().join("codex")),
        Some(dir.path().join("claude")),
        dir.path(),
    )
    .unwrap();
    let store = Store::open(paths);
    let mut roster = Roster::empty();
    let mut record = AccountRecord::new("session@example.com");
    record.provider = Provider::Claude;
    record.organization_uuid = "org".into();
    roster.add_record(2, record);
    let credential = ClaudeCredential::from_value(
        json!({"claudeAiOauth": {"accessToken": "at-old", "refreshToken": "rt-old", "expiresAt": 4102444800000i64}, "mcpOAuth": {"private": true}}),
    );
    let account =
        OauthAccount(json!({"emailAddress": "session@example.com", "organizationUuid": "org"}));
    credentials::write(&store, 2, &SlotFile::new(&credential, account).to_value()).unwrap();
    roster::write(&store.paths, &roster).unwrap();
    (dir, store, roster)
}

#[test]
fn seeds_profile_credentials_and_identity_without_changing_the_default_login() {
    let (_dir, store, roster) = fixture();
    write_json_private(
        &store.paths.claude_credentials_file(),
        &json!({"sentinel": "default"}),
    )
    .unwrap();
    let prepared = prepare_profile(&store, &roster, 2, ShareOptions::default()).unwrap();
    let auth = read_json(&prepared.profile.join(".credentials.json"))
        .unwrap()
        .unwrap();
    assert_eq!(auth["claudeAiOauth"]["refreshToken"], "rt-old");
    assert!(auth.get("mcpOAuth").is_none());
    assert!(!prepared.profile.join("auth.json").exists());
    let config = read_json(&prepared.profile.join(".claude.json"))
        .unwrap()
        .unwrap();
    assert_eq!(
        config["oauthAccount"]["emailAddress"],
        "session@example.com"
    );
    assert_eq!(config["hasCompletedOnboarding"], true);
    assert!(config["theme"].is_string());
    assert_eq!(
        read_json(&store.paths.claude_credentials_file())
            .unwrap()
            .unwrap()["sentinel"],
        "default"
    );
}

#[test]
fn recovers_rotated_profile_tokens_and_preserves_local_config() {
    let (_dir, store, roster) = fixture();
    let profile = store.paths.session_dir(2, "session@example.com");
    write_json_private(&profile.join(".credentials.json"), &json!({"claudeAiOauth": {"accessToken": "at-new", "refreshToken": "rt-new", "expiresAt": 4102444900000i64}})).unwrap();
    write_json_private(&profile.join(".claude.json"), &json!({"oauthAccount": {"emailAddress": "session@example.com", "organizationUuid": "org"}, "projects": {"/work": {"trusted": true}}, "theme": "light"})).unwrap();
    prepare_profile(&store, &roster, 2, ShareOptions::default()).unwrap();
    let saved = credentials::read(&store, 2).unwrap().unwrap();
    assert_eq!(saved["claudeAiOauth"]["refreshToken"], "rt-new");
    let config = read_json(&profile.join(".claude.json")).unwrap().unwrap();
    assert_eq!(config["projects"]["/work"]["trusted"], true);
    assert_eq!(config["theme"], "light");
}

#[test]
fn does_not_overwrite_a_running_profile_or_capture_a_different_identity() {
    let (_dir, store, roster) = fixture();
    let profile = store.paths.session_dir(2, "session@example.com");
    write_json_private(
        &profile.join("sessions/owner.json"),
        &json!({"pid": std::process::id()}),
    )
    .unwrap();
    write_json_private(&profile.join(".credentials.json"), &json!({"claudeAiOauth": {"accessToken": "at-other", "refreshToken": "rt-other", "expiresAt": 4102444900000i64}})).unwrap();
    write_json_private(
        &profile.join(".claude.json"),
        &json!({"oauthAccount": {"emailAddress": "other@example.com", "organizationUuid": "org"}}),
    )
    .unwrap();
    let before = std::fs::read(profile.join(".credentials.json")).unwrap();
    assert!(prepare_profile(&store, &roster, 2, ShareOptions::default()).is_err());
    assert_eq!(
        std::fs::read(profile.join(".credentials.json")).unwrap(),
        before
    );
    assert_eq!(
        credentials::read(&store, 2).unwrap().unwrap()["claudeAiOauth"]["refreshToken"],
        "rt-old"
    );
}
