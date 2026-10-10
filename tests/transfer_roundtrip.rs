//! Export / import through the library functions in `ccsw::transfer`, against
//! temp stores (no binary needed).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use ccsw::codex::auth::AuthJson;
use ccsw::errors::CcswError;
use ccsw::model::{AccountKind, AccountRecord, Identity, Roster, now_unix, parse_iso};
use ccsw::paths::Paths;
use ccsw::provider::Provider;
use ccsw::store::usage_store::{FetchRecord, UsageStore};
use ccsw::store::{Store, credentials, roster};
use ccsw::transfer::{
    ExportTarget, ImportOptions, ImportReport, ImportSource, export_accounts, export_cmd,
    import_accounts, import_cmd, platform_name,
};

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    paths: Paths,
    store: Store,
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_values(
        Some(dir.path().join("store")),
        Some(dir.path().join("codex")),
        Some(dir.path().join("claude")),
        dir.path(),
    )
    .unwrap();
    Fx {
        root: dir.path().to_path_buf(),
        store: Store::open(paths.clone()),
        paths,
        _dir: dir,
    }
}

fn jwt(claims: &Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    format!("header.{payload}.sig")
}

fn chatgpt_auth(email: &str, account_id: &str, refresh: &str) -> Value {
    let id_token = jwt(&json!({
        "email": email,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": format!("user-{account_id}"),
            "chatgpt_plan_type": "plus"
        }
    }));
    json!({
        "OPENAI_API_KEY": null,
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token,
            "access_token": "at",
            "refresh_token": refresh,
            "account_id": account_id
        },
        "last_refresh": "2026-09-29T10:00:00Z",
        "vendor_extra": {"keep": true}
    })
}

fn api_key_auth(key: &str) -> Value {
    json!({"auth_mode": "apikey", "OPENAI_API_KEY": key})
}

fn record(email: &str, account_id: &str) -> AccountRecord {
    let mut record = AccountRecord::new(email);
    if !account_id.is_empty() {
        record.uuid = format!("user-{account_id}");
    }
    record.organization_uuid = account_id.into();
    record.added = "2026-09-01T00:00:00Z".into();
    record
}

fn add(fx: &Fx, roster: &mut Roster, slot: u32, record: AccountRecord, creds: Option<&Value>) {
    roster.add_record(slot, record);
    if let Some(value) = creds {
        credentials::write(&fx.store, slot, value).unwrap();
    }
    roster::write(&fx.paths, roster).unwrap();
}

/// Slots 1 (ChatGPT, alias `dev`), 2 (API key), 3 (ChatGPT, Acme, plan `pro`, active).
fn seed_three(fx: &Fx) -> Roster {
    let mut ro = Roster::empty();
    let mut a = record("a@example.com", "acct-a");
    a.alias = Some("dev".into());
    add(
        fx,
        &mut ro,
        1,
        a,
        Some(&chatgpt_auth("a@example.com", "acct-a", "rt-a")),
    );
    let mut k = record("api-key-2@token.local", "");
    k.kind = Some(AccountKind::ApiKey);
    add(fx, &mut ro, 2, k, Some(&api_key_auth("sk-two")));
    let mut c = record("c@example.com", "acct-c");
    c.plan_type = Some("pro".into());
    c.organization_name = "Acme".into();
    add(
        fx,
        &mut ro,
        3,
        c,
        Some(&chatgpt_auth("c@example.com", "acct-c", "rt-c")),
    );
    ro.set_active(Some(3));
    roster::write(&fx.paths, &ro).unwrap();
    ro
}

fn write_live(fx: &Fx, value: &Value) {
    AuthJson::from_value(value.clone())
        .write(&fx.paths.live_auth_file())
        .unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn roster_of(fx: &Fx) -> Roster {
    roster::read(&fx.paths).unwrap().unwrap()
}

fn slot_creds(fx: &Fx, slot: u32) -> Value {
    credentials::read(&fx.store, slot).unwrap().unwrap()
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn entry(number: u32, email: &str, account_id: &str, creds: Value) -> Value {
    let uuid = if account_id.is_empty() {
        String::new()
    } else {
        format!("user-{account_id}")
    };
    json!({
        "number": number,
        "email": email,
        "uuid": uuid,
        "organizationUuid": account_id,
        "organizationName": "",
        "added": "2026-09-01T00:00:00Z",
        "credentials": creds
    })
}

fn envelope(accounts: Vec<Value>, active: Option<u32>) -> Value {
    // Keep the old metadata name to verify imports from before the rename.
    json!({
        "version": 1,
        "exportedAt": "2026-09-29T00:00:00Z",
        "exportedFrom": "linux",
        "cswitchVersion": "0.1.0",
        "swapVersion": "0.1.0",
        "encrypted": false,
        "activeAccountNumber": active,
        "accounts": accounts
    })
}

fn import_value(fx: &Fx, value: &Value, force: bool) -> Result<ImportReport, CcswError> {
    let path = fx.root.join("in.cswitch");
    fs::write(&path, value.to_string()).unwrap();
    import_accounts(&fx.paths, ImportSource::File(path), force)
}

fn strike(fx: &Fx, slot: u32, identity: &Identity, struck_fp: Option<String>) {
    UsageStore::new(&fx.paths)
        .record(
            slot,
            identity,
            FetchRecord::Failure {
                error: "invalid_grant".into(),
                retry_after: None,
                permanent_auth: true,
                struck_fp,
            },
            1000.0,
        )
        .unwrap();
}

fn strikes(fx: &Fx, slot: u32, identity: &Identity) -> u32 {
    let ids = BTreeMap::from([(slot, identity.clone())]);
    UsageStore::new(&fx.paths)
        .entries(&ids, 2000.0)
        .get(&slot)
        .map_or(0, |entry| entry.auth_dead_strikes)
}

#[test]
fn export_bulk_writes_the_envelope_file() {
    let fx = fixture();
    seed_three(&fx);
    // The active slot's live login carries a fresher token than the stored snapshot.
    write_live(&fx, &chatgpt_auth("c@example.com", "acct-c", "rt-live"));
    let out_dir = fx.root.join("out");
    fs::create_dir(&out_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&out_dir, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = out_dir.join("backup.ccsw");

    let report = export_accounts(&fx.paths, ExportTarget::File(path.clone()), None, false).unwrap();
    assert_eq!(report.written, 3);
    assert!(report.skipped.is_empty());
    assert_eq!(
        report.notices,
        vec![format!("Exported 3 account(s) to {}", path.display())]
    );

    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("{\n  \"version\": 2,\n  \"exportedAt\": \""),
        "{text}"
    );
    assert!(text.ends_with("}\n"));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value, report.envelope);
    assert_eq!(value["version"], 2);
    assert!(value["exportedAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(value["exportedFrom"], platform_name());
    assert!(["macos", "linux", "wsl", "windows", "unknown"].contains(&platform_name()));
    assert_eq!(value["ccswVersion"], ccsw::VERSION);
    assert_eq!(value["swapVersion"], ccsw::VERSION);
    assert_eq!(value["encrypted"], false);
    assert_eq!(value["activeAccountNumber"], 3);
    assert_eq!(value["activeByProvider"], json!({"codex": 3}));

    let accounts = value["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 3);
    let first = &accounts[0];
    assert_eq!(first["number"], 1);
    assert_eq!(first["provider"], "codex");
    assert_eq!(first["email"], "a@example.com");
    assert_eq!(first["uuid"], "user-acct-a");
    assert_eq!(first["organizationUuid"], "acct-a");
    assert_eq!(first["organizationName"], "");
    assert_eq!(first["added"], "2026-09-01T00:00:00Z");
    assert_eq!(first["alias"], "dev");
    assert!(first.get("kind").is_none());
    assert!(first.get("planType").is_none());
    assert_eq!(
        first["credentials"],
        chatgpt_auth("a@example.com", "acct-a", "rt-a")
    );
    let key = &accounts[1];
    assert_eq!(key["number"], 2);
    assert_eq!(key["kind"], "api_key");
    assert_eq!(key["uuid"], "");
    assert_eq!(key["credentials"], api_key_auth("sk-two"));
    assert!(key.get("alias").is_none());
    let active = &accounts[2];
    assert_eq!(active["planType"], "pro");
    assert_eq!(active["organizationName"], "Acme");
    assert_eq!(
        active["credentials"]["tokens"]["refresh_token"], "rt-live",
        "the live login is the freshest copy"
    );
    #[cfg(unix)]
    {
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(
            mode_of(&out_dir),
            0o755,
            "the parent directory's mode is untouched"
        );
    }
    assert_eq!(
        fs::read_dir(&out_dir).unwrap().count(),
        1,
        "no temp file left behind"
    );
    assert_eq!(
        slot_creds(&fx, 3)["tokens"]["refresh_token"],
        "rt-c",
        "an export never modifies the stored snapshot"
    );
}

#[test]
fn export_active_uses_the_snapshot_unless_the_live_login_matches() {
    let fx = fixture();
    seed_three(&fx);
    let export = |fx: &Fx| {
        export_accounts(
            &fx.paths,
            ExportTarget::File(fx.root.join("b.ccsw")),
            None,
            false,
        )
        .unwrap()
        .envelope
    };
    let refresh = |value: &Value, index: usize| {
        value["accounts"][index]["credentials"]["tokens"]["refresh_token"].clone()
    };
    // No live file at all.
    assert_eq!(refresh(&export(&fx), 2), "rt-c");
    // A live login of another identity.
    write_live(&fx, &chatgpt_auth("other@example.com", "acct-o", "rt-o"));
    assert_eq!(refresh(&export(&fx), 2), "rt-c");
    // Same email under another workspace is another identity.
    write_live(&fx, &chatgpt_auth("c@example.com", "acct-z", "rt-z"));
    assert_eq!(refresh(&export(&fx), 2), "rt-c");
    // A garbage live file is ignored.
    fs::write(fx.paths.live_auth_file(), "nope").unwrap();
    assert_eq!(refresh(&export(&fx), 2), "rt-c");
    // A live login matching an inactive slot is not consulted for that slot.
    write_live(&fx, &chatgpt_auth("a@example.com", "acct-a", "rt-live"));
    assert_eq!(refresh(&export(&fx), 0), "rt-a");
    assert_eq!(refresh(&export(&fx), 2), "rt-c");
}

#[test]
fn export_skips_slots_without_stored_credentials() {
    let fx = fixture();
    let mut ro = Roster::empty();
    add(
        &fx,
        &mut ro,
        1,
        record("a@example.com", "acct-a"),
        Some(&chatgpt_auth("a@example.com", "acct-a", "rt-a")),
    );
    add(&fx, &mut ro, 2, record("b@example.com", "acct-b"), None);
    ro.set_active(Some(2));
    roster::write(&fx.paths, &ro).unwrap();

    let path = fx.root.join("b.ccsw");
    let report = export_accounts(&fx.paths, ExportTarget::File(path.clone()), None, false).unwrap();
    assert_eq!(report.written, 1);
    assert_eq!(report.skipped, vec![2]);
    assert_eq!(
        report.notices,
        vec![
            "Skipping Account-2 (b@example.com): no stored credentials — re-add with: ccsw add --slot 2".to_string(),
            format!("Exported 1 account(s) to {}", path.display()),
        ]
    );
    assert_eq!(
        report.envelope["activeAccountNumber"],
        Value::Null,
        "the active slot was skipped"
    );
    assert_eq!(report.envelope["accounts"].as_array().unwrap().len(), 1);
}

#[test]
fn export_with_every_slot_skipped_is_a_transfer_error() {
    let fx = fixture();
    let mut ro = Roster::empty();
    add(&fx, &mut ro, 1, record("a@example.com", "acct-a"), None);
    let path = fx.root.join("b.ccsw");
    let err =
        export_accounts(&fx.paths, ExportTarget::File(path.clone()), None, false).unwrap_err();
    assert_eq!(err.type_name(), "TransferError");
    assert_eq!(
        err.to_string(),
        "no exportable accounts — all managed slots are missing stored credentials. Re-add with: ccsw add --slot <number>"
    );
    assert!(!path.exists());
}

#[test]
fn export_without_accounts_is_a_transfer_error() {
    let fx = fixture();
    let path = fx.root.join("b.ccsw");
    for prepared in [false, true] {
        if prepared {
            roster::write(&fx.paths, &Roster::empty()).unwrap();
        }
        let err =
            export_accounts(&fx.paths, ExportTarget::File(path.clone()), None, false).unwrap_err();
        assert_eq!(err.type_name(), "TransferError");
        assert_eq!(
            err.to_string(),
            "no accounts to export — run ccsw add first"
        );
    }
    assert!(!path.exists());
}

#[test]
fn export_one_account_by_number_alias_or_email() {
    let fx = fixture();
    seed_three(&fx);
    let path = fx.root.join("one.ccsw");
    for id in ["1", "dev", "DEV", "a@example.com"] {
        // `--full` is accepted and changes nothing.
        let report =
            export_accounts(&fx.paths, ExportTarget::File(path.clone()), Some(id), true).unwrap();
        assert_eq!(report.written, 1, "{id}");
        let accounts = report.envelope["accounts"].as_array().unwrap().clone();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["email"], "a@example.com");
        assert_eq!(
            report.envelope["activeAccountNumber"],
            Value::Null,
            "active slot 3 is not in the file"
        );
    }
    let report = export_accounts(
        &fx.paths,
        ExportTarget::File(path.clone()),
        Some("3"),
        false,
    )
    .unwrap();
    assert_eq!(report.envelope["activeAccountNumber"], 3);

    for id in ["nobody", "9", "x@example.com"] {
        let err = export_accounts(&fx.paths, ExportTarget::File(path.clone()), Some(id), false)
            .unwrap_err();
        assert_eq!(err.type_name(), "TransferError", "{id}");
        assert_eq!(err.to_string(), format!("account not found: {id}"));
    }
}

#[test]
fn export_one_account_without_credentials_is_a_hard_error() {
    let fx = fixture();
    let mut ro = Roster::empty();
    add(
        &fx,
        &mut ro,
        1,
        record("a@example.com", "acct-a"),
        Some(&chatgpt_auth("a@example.com", "acct-a", "rt-a")),
    );
    add(&fx, &mut ro, 2, record("b@example.com", "acct-b"), None);
    let err = export_accounts(
        &fx.paths,
        ExportTarget::File(fx.root.join("b.ccsw")),
        Some("2"),
        false,
    )
    .unwrap_err();
    assert_eq!(err.type_name(), "CredentialReadError");
    assert_eq!(
        err.to_string(),
        "no backup credentials found for account 2 (b@example.com)"
    );
}

#[test]
fn export_to_stdout_prints_no_summary() {
    let fx = fixture();
    seed_three(&fx);
    let report = export_accounts(&fx.paths, ExportTarget::Stdout, None, false).unwrap();
    assert_eq!(report.written, 3);
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    assert_eq!(report.envelope["accounts"].as_array().unwrap().len(), 3);
    assert_eq!(report.envelope["activeAccountNumber"], 3);
}

fn claude_slot_file(email: &str, org: &str, org_name: &str, refresh: &str) -> Value {
    json!({
        "claudeAiOauth": {
            "accessToken": format!("cat-{refresh}"),
            "refreshToken": refresh,
            "expiresAt": 4_102_444_800_000i64,
            "scopes": ["user:inference", "user:profile"]
        },
        "oauthAccount": {
            "accountUuid": format!("uuid-{email}"),
            "emailAddress": email,
            "organizationUuid": org,
            "organizationName": org_name,
            "billingType": "stripe"
        }
    })
}

fn claude_record(email: &str, org: &str, org_name: &str) -> AccountRecord {
    let mut record = AccountRecord::new(email);
    record.provider = Provider::Claude;
    record.uuid = format!("uuid-{email}");
    record.organization_uuid = org.into();
    record.organization_name = org_name.into();
    record.added = "2026-09-01T00:00:00Z".into();
    record
}

/// Slots 1 (Codex, active), 2 (Claude OAuth, Acme, alias `cc`, active), 4 (Claude managed key).
fn seed_mixed(fx: &Fx) -> Roster {
    let mut ro = Roster::empty();
    add(
        fx,
        &mut ro,
        1,
        record("a@example.com", "acct-a"),
        Some(&chatgpt_auth("a@example.com", "acct-a", "rt-a")),
    );
    let mut cc = claude_record("c@example.com", "org-c", "Acme");
    cc.alias = Some("cc".into());
    add(
        fx,
        &mut ro,
        2,
        cc,
        Some(&claude_slot_file("c@example.com", "org-c", "Acme", "crt-c")),
    );
    let mut key = claude_record("api-key-4@token.local", "", "");
    key.kind = Some(AccountKind::ApiKey);
    add(
        fx,
        &mut ro,
        4,
        key,
        Some(&json!({
            "primaryApiKey": "sk-ant-api03-four",
            "oauthAccount": {"emailAddress": "api-key-4@token.local", "accountUuid": "",
                             "organizationUuid": null, "organizationName": null}
        })),
    );
    ro.set_active_for(Provider::Codex, Some(1));
    ro.set_active_for(Provider::Claude, Some(2));
    roster::write(&fx.paths, &ro).unwrap();
    ro
}

fn write_claude_live(fx: &Fx, creds: &Value, config: &Value) {
    fs::create_dir_all(&fx.paths.claude_home).unwrap();
    fs::write(fx.paths.claude_credentials_file(), creds.to_string()).unwrap();
    fs::write(fx.paths.claude_global_config_file(), config.to_string()).unwrap();
}

#[test]
fn export_carries_both_providers() {
    let fx = fixture();
    seed_mixed(&fx);
    let path = fx.root.join("mixed.ccsw");
    let report = export_accounts(&fx.paths, ExportTarget::File(path.clone()), None, false).unwrap();
    assert_eq!(report.written, 3);
    assert_eq!(
        report.notices,
        vec![format!("Exported 3 account(s) to {}", path.display())]
    );
    let value = report.envelope;
    assert_eq!(value["version"], 2);
    assert_eq!(value["activeAccountNumber"], 1);
    assert_eq!(value["activeByProvider"], json!({"claude": 2, "codex": 1}));
    let accounts = value["accounts"].as_array().unwrap();
    assert_eq!(accounts[0]["provider"], "codex");
    assert_eq!(accounts[1]["provider"], "claude");
    assert_eq!(accounts[1]["number"], 2);
    assert_eq!(accounts[1]["alias"], "cc");
    assert_eq!(accounts[1]["organizationName"], "Acme");
    assert!(accounts[1].get("planType").is_none());
    assert_eq!(
        accounts[1]["credentials"],
        claude_slot_file("c@example.com", "org-c", "Acme", "crt-c"),
        "the slot file is exported as stored"
    );
    assert_eq!(accounts[2]["provider"], "claude");
    assert_eq!(accounts[2]["kind"], "api_key");
    assert_eq!(
        accounts[2]["credentials"]["primaryApiKey"],
        "sk-ant-api03-four"
    );

    // `--account` on a Claude slot by number or alias.
    for id in ["2", "cc"] {
        let report =
            export_accounts(&fx.paths, ExportTarget::File(path.clone()), Some(id), false).unwrap();
        assert_eq!(report.written, 1, "{id}");
        assert_eq!(report.envelope["accounts"][0]["email"], "c@example.com");
        assert_eq!(report.envelope["activeAccountNumber"], Value::Null);
        assert_eq!(report.envelope["activeByProvider"], json!({"claude": 2}));
    }
    // Neither active slot in the file: `activeByProvider` is left out entirely.
    let report = export_accounts(
        &fx.paths,
        ExportTarget::File(path.clone()),
        Some("4"),
        false,
    )
    .unwrap();
    assert!(report.envelope.get("activeByProvider").is_none());
    assert_eq!(report.envelope["activeAccountNumber"], Value::Null);
}

#[test]
fn export_claude_active_prefers_the_live_login_only_when_it_matches() {
    let fx = fixture();
    seed_mixed(&fx);
    let export = |fx: &Fx| {
        export_accounts(
            &fx.paths,
            ExportTarget::File(fx.root.join("b.ccsw")),
            None,
            false,
        )
        .unwrap()
        .envelope
    };
    let refresh = |value: &Value, index: usize| {
        value["accounts"][index]["credentials"]["claudeAiOauth"]["refreshToken"].clone()
    };
    let config = |email: &str, org: &str| {
        json!({"numStartups": 1, "oauthAccount": {"emailAddress": email, "accountUuid": "u",
               "organizationUuid": org, "organizationName": "Acme"}})
    };
    let live = |refresh: &str| {
        json!({"claudeAiOauth": {"accessToken": "live-at", "refreshToken": refresh,
               "expiresAt": 4_102_444_900_000i64, "scopes": ["user:inference"]},
               "mcpOAuth": {"srv": {"accessToken": "m"}}})
    };
    // No live login at all.
    assert_eq!(refresh(&export(&fx), 1), "crt-c");
    // The live login is the active slot's identity: its token wins, siblings are
    // dropped and the stored oauthAccount stays as stored.
    write_claude_live(&fx, &live("crt-live"), &config("C@example.com", "org-c"));
    let value = export(&fx);
    assert_eq!(refresh(&value, 1), "crt-live");
    assert!(
        value["accounts"][1]["credentials"]
            .get("mcpOAuth")
            .is_none()
    );
    assert_eq!(
        value["accounts"][1]["credentials"]["oauthAccount"]["billingType"],
        "stripe"
    );
    assert_eq!(
        slot_creds(&fx, 2)["claudeAiOauth"]["refreshToken"],
        "crt-c",
        "an export never modifies the stored snapshot"
    );
    // Another identity (same email, other org) is ignored.
    write_claude_live(&fx, &live("crt-z"), &config("c@example.com", "org-z"));
    assert_eq!(refresh(&export(&fx), 1), "crt-c");
    // The same identity but another credential kind (Claude Code moved to a
    // managed key while `oauthAccount` lingered) keeps the stored OAuth login.
    let mut key_config = config("c@example.com", "org-c");
    key_config["primaryApiKey"] = json!("sk-ant-api03-live");
    write_claude_live(
        &fx,
        &json!({"mcpOAuth": {"srv": {"accessToken": "m"}}}),
        &key_config,
    );
    let value = export(&fx);
    assert_eq!(refresh(&value, 1), "crt-c");
    assert!(
        value["accounts"][1]["credentials"]
            .get("primaryApiKey")
            .is_none()
    );
    // Garbage live files are ignored.
    fs::write(fx.paths.claude_credentials_file(), "nope").unwrap();
    assert_eq!(refresh(&export(&fx), 1), "crt-c");
    // A live login matching an inactive Claude slot is not consulted.
    let mut ro = roster_of(&fx);
    ro.set_active_for(Provider::Claude, Some(4));
    roster::write(&fx.paths, &ro).unwrap();
    write_claude_live(&fx, &live("crt-live"), &config("c@example.com", "org-c"));
    assert_eq!(refresh(&export(&fx), 1), "crt-c");
}

#[test]
fn export_folds_a_newer_session_profile_token_into_a_claude_slot() {
    let fx = fixture();
    seed_mixed(&fx);
    let profile = fx.paths.session_dir(2, "c@example.com");
    fs::create_dir_all(&profile).unwrap();
    let mut rotated = claude_slot_file("c@example.com", "org-c", "Acme", "crt-rotated");
    rotated["claudeAiOauth"]["expiresAt"] = json!(4_102_444_900_000i64);
    let account = rotated
        .as_object_mut()
        .unwrap()
        .remove("oauthAccount")
        .unwrap();
    fs::write(profile.join(".credentials.json"), rotated.to_string()).unwrap();
    fs::write(
        profile.join(".claude.json"),
        json!({"oauthAccount": account}).to_string(),
    )
    .unwrap();

    let report = export_accounts(
        &fx.paths,
        ExportTarget::File(fx.root.join("b.ccsw")),
        Some("2"),
        false,
    )
    .unwrap();
    assert_eq!(
        report.envelope["accounts"][0]["credentials"]["claudeAiOauth"]["refreshToken"],
        "crt-rotated"
    );
    assert_eq!(
        slot_creds(&fx, 2)["claudeAiOauth"]["refreshToken"],
        "crt-rotated",
        "the profile's rotation is folded into the slot first"
    );
}

#[test]
fn import_round_trips_an_export_into_an_empty_store() {
    let source = fixture();
    seed_three(&source);
    let path = source.root.join("backup.ccsw");
    export_accounts(&source.paths, ExportTarget::File(path.clone()), None, false).unwrap();

    let target = fixture();
    let report = import_accounts(&target.paths, ImportSource::File(path), false).unwrap();
    assert_eq!(
        (
            report.imported,
            report.overwritten,
            report.skipped,
            report.replaced
        ),
        (3, 0, 0, 0)
    );
    assert_eq!(report.written_slots, vec![1, 2, 3]);
    assert_eq!(
        report.notices,
        vec![
            "Imported a@example.com → slot 1",
            "Imported api-key-2@token.local → slot 2",
            "Imported c@example.com → slot 3",
            "Done: 3 imported, 0 overwritten, 0 skipped",
        ]
    );
    let imported = roster_of(&target);
    let original = roster_of(&source);
    assert_eq!(imported.sequence, vec![1, 2, 3]);
    assert_eq!(
        imported.active_account_number,
        Some(3),
        "seeded from the envelope"
    );
    for slot in [1, 2, 3] {
        assert_eq!(imported.record(slot), original.record(slot), "slot {slot}");
        assert_eq!(slot_creds(&target, slot), slot_creds(&source, slot));
        #[cfg(unix)]
        assert_eq!(mode_of(&target.paths.credential_file(slot)), 0o600);
    }
    assert_eq!(
        slot_creds(&target, 1)["vendor_extra"]["keep"],
        true,
        "unknown credential keys survive"
    );
    #[cfg(unix)]
    assert_eq!(mode_of(&target.paths.backup_root), 0o700);
}

#[test]
fn import_skips_existing_identities_and_allocates_free_slots() {
    let fx = fixture();
    let mut ro = Roster::empty();
    add(
        &fx,
        &mut ro,
        5,
        record("a@example.com", "acct-a"),
        Some(&chatgpt_auth("a@example.com", "acct-a", "rt-old")),
    );
    add(
        &fx,
        &mut ro,
        2,
        record("z@example.com", "acct-z"),
        Some(&chatgpt_auth("z@example.com", "acct-z", "rt-z")),
    );
    ro.set_active(Some(2));
    roster::write(&fx.paths, &ro).unwrap();

    let value = envelope(
        vec![
            entry(
                1,
                "a@example.com",
                "acct-a",
                chatgpt_auth("a@example.com", "acct-a", "rt-new"),
            ),
            entry(
                2,
                "b@example.com",
                "acct-b",
                chatgpt_auth("b@example.com", "acct-b", "rt-b"),
            ),
            entry(
                3,
                "c@example.com",
                "acct-c",
                chatgpt_auth("c@example.com", "acct-c", "rt-c"),
            ),
        ],
        Some(1),
    );
    let report = import_value(&fx, &value, false).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Skipped a@example.com (already exists, use --force)",
            "Imported b@example.com → slot 6",
            "Imported c@example.com → slot 3",
            "Done: 2 imported, 0 overwritten, 1 skipped",
        ]
    );
    assert_eq!(report.written_slots, vec![6, 3]);
    let after = roster_of(&fx);
    assert_eq!(after.sequence, vec![2, 3, 5, 6]);
    assert_eq!(
        after.active_account_number,
        Some(2),
        "an existing choice is never overridden"
    );
    assert_eq!(
        slot_creds(&fx, 5)["tokens"]["refresh_token"],
        "rt-old",
        "skipped slots are untouched"
    );
    assert_eq!(after.record(6).unwrap().email, "b@example.com");
    assert_eq!(after.record(3).unwrap().email, "c@example.com");
}

#[test]
fn import_force_overwrites_an_existing_slot_in_place() {
    let fx = fixture();
    let mut ro = Roster::empty();
    let mut old = record("a@example.com", "acct-a");
    old.alias = Some("old".into());
    old.disabled = true;
    add(
        &fx,
        &mut ro,
        5,
        old,
        Some(&chatgpt_auth("a@example.com", "acct-a", "rt-old")),
    );
    let mut e = entry(
        1,
        "a@example.com",
        "acct-a",
        chatgpt_auth("a@example.com", "acct-a", "rt-new"),
    );
    e["alias"] = json!("dev");
    e["organizationName"] = json!("Acme");
    e["planType"] = json!("team");

    let report = import_value(&fx, &envelope(vec![e], Some(1)), true).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Overwrote a@example.com (slot 5)",
            "Done: 0 imported, 1 overwritten, 0 skipped",
        ]
    );
    assert_eq!(report.written_slots, vec![5]);
    let after = roster_of(&fx);
    let rec = after.record(5).unwrap();
    assert_eq!(rec.alias.as_deref(), Some("dev"));
    assert_eq!(rec.organization_name, "Acme");
    assert_eq!(rec.plan_type.as_deref(), Some("team"));
    assert!(!rec.disabled, "the record is rebuilt from the envelope");
    assert_eq!(after.sequence, vec![5]);
    assert_eq!(
        after.active_account_number,
        Some(5),
        "seeded with the resolved local slot"
    );
    assert_eq!(slot_creds(&fx, 5)["tokens"]["refresh_token"], "rt-new");
    let prev = read_json(&fx.paths.credential_prev_file(5));
    assert_eq!(
        prev["tokens"]["refresh_token"], "rt-old",
        "the previous generation is retained"
    );
}

#[test]
fn import_replaces_a_dead_token_slot_without_force() {
    let fx = fixture();
    let identity = Identity::new("a@example.com", "acct-a");
    let mut ro = Roster::empty();
    let dead = chatgpt_auth("a@example.com", "acct-a", "rt-dead");
    add(
        &fx,
        &mut ro,
        1,
        record("a@example.com", "acct-a"),
        Some(&dead),
    );
    strike(&fx, 1, &identity, credentials::fingerprint(&dead));

    let value = envelope(
        vec![entry(
            1,
            "a@example.com",
            "acct-a",
            chatgpt_auth("a@example.com", "acct-a", "rt-fresh"),
        )],
        None,
    );
    let report = import_value(&fx, &value, false).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Replaced a@example.com (slot 1 was quarantined: refresh token dead)",
            "Done: 0 imported, 0 overwritten, 0 skipped, 1 replaced (dead token)",
        ]
    );
    assert_eq!(report.replaced, 1);
    assert_eq!(report.written_slots, vec![1]);
    assert_eq!(slot_creds(&fx, 1)["tokens"]["refresh_token"], "rt-fresh");
    assert_eq!(strikes(&fx, 1, &identity), 0, "the strike is lifted");

    // A strike bound to another generation than the stored one no longer quarantines.
    strike(&fx, 1, &identity, Some("sha256:stale".into()));
    let report = import_value(&fx, &value, false).unwrap();
    assert_eq!(
        report.notices[0],
        "Skipped a@example.com (already exists, use --force)"
    );
    assert_eq!(report.skipped, 1);
    assert_eq!(strikes(&fx, 1, &identity), 1, "a skipped slot is untouched");
}

#[test]
fn import_force_reports_the_lifted_strike() {
    let fx = fixture();
    let identity = Identity::new("a@example.com", "acct-a");
    let mut ro = Roster::empty();
    let dead = chatgpt_auth("a@example.com", "acct-a", "rt-dead");
    add(
        &fx,
        &mut ro,
        1,
        record("a@example.com", "acct-a"),
        Some(&dead),
    );
    strike(&fx, 1, &identity, credentials::fingerprint(&dead));

    // The same generation the strike condemned.
    let same = envelope(
        vec![entry(1, "a@example.com", "acct-a", dead.clone())],
        None,
    );
    let report = import_value(&fx, &same, true).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Overwrote a@example.com (slot 1)",
            "  └ cleared this slot's stored dead-token strike",
            "  └ this import holds the same credential generation the strike condemned; another permanent auth failure will quarantine it again — recover with a newer export or a re-login",
            "Done: 0 imported, 1 overwritten, 0 skipped",
        ]
    );
    assert_eq!(strikes(&fx, 1, &identity), 0);

    // A newer generation: only the first note.
    strike(&fx, 1, &identity, credentials::fingerprint(&dead));
    let newer = envelope(
        vec![entry(
            1,
            "a@example.com",
            "acct-a",
            chatgpt_auth("a@example.com", "acct-a", "rt-new"),
        )],
        None,
    );
    let report = import_value(&fx, &newer, true).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Overwrote a@example.com (slot 1)",
            "  └ cleared this slot's stored dead-token strike",
            "Done: 0 imported, 1 overwritten, 0 skipped",
        ]
    );

    // No strike at all: no notes.
    let report = import_value(&fx, &newer, true).unwrap();
    assert_eq!(report.notices.len(), 2);
}

#[test]
fn import_validates_the_envelope_before_writing_anything() {
    let fx = fixture();
    let good =
        |n: u32, email: &str, acct: &str| entry(n, email, acct, chatgpt_auth(email, acct, "rt"));
    let with = |patch: &dyn Fn(&mut Value)| {
        let mut e = good(1, "a@example.com", "acct-a");
        patch(&mut e);
        envelope(vec![e], None)
    };
    let remove = |field: &'static str| {
        with(&move |e: &mut Value| {
            e.as_object_mut().unwrap().remove(field);
        })
    };
    let mut aliased_pair = vec![
        good(1, "a@example.com", "acct-a"),
        good(2, "b@example.com", "acct-b"),
    ];
    aliased_pair[0]["alias"] = json!("dev");
    aliased_pair[1]["alias"] = json!("DEV");

    let cases: Vec<(Value, &str)> = vec![
        (json!([1]), "export file must be a JSON object"),
        (
            json!({"version": 3, "accounts": [good(1, "a@example.com", "acct-a")]}),
            "unsupported export version: 3 (expected 1 or 2)",
        ),
        (
            json!({"accounts": []}),
            "unsupported export version: None (expected 1 or 2)",
        ),
        (
            json!({"version": "1"}),
            "unsupported export version: '1' (expected 1 or 2)",
        ),
        (
            json!({"version": 1, "encrypted": true, "accounts": [1]}),
            "encrypted exports are not supported in this version — decrypt before piping (e.g. gpg -d backup.gpg | ccsw import -)",
        ),
        (
            json!({"version": 1, "accounts": []}),
            "export file has no accounts to import",
        ),
        (
            json!({"version": 1}),
            "export file has no accounts to import",
        ),
        (
            json!({"version": 1, "accounts": {}}),
            "export file has no accounts to import",
        ),
        (
            envelope(vec![json!("x")], None),
            "account entry must be a JSON object",
        ),
        (
            with(&|e| e["email"] = json!("nope")),
            "invalid or missing email in imported account: 'nope'",
        ),
        (
            remove("email"),
            "invalid or missing email in imported account: None",
        ),
        (
            with(&|e| e["email"] = json!(7)),
            "invalid or missing email in imported account: 7",
        ),
        (
            with(&|e| e["number"] = json!(0)),
            "invalid slot number in imported account (a@example.com): 0",
        ),
        (
            with(&|e| e["number"] = json!(true)),
            "invalid slot number in imported account (a@example.com): True",
        ),
        (
            with(&|e| e["number"] = json!("1")),
            "invalid slot number in imported account (a@example.com): '1'",
        ),
        (
            with(&|e| e["number"] = json!(1.5)),
            "invalid slot number in imported account (a@example.com): 1.5",
        ),
        (
            remove("number"),
            "invalid slot number in imported account (a@example.com): None",
        ),
        (
            with(&|e| e["organizationUuid"] = json!(5)),
            "organizationUuid for a@example.com must be a string, got int",
        ),
        (
            with(&|e| e["organizationName"] = json!(5)),
            "organizationName for a@example.com must be a string, got int",
        ),
        (
            with(&|e| e["uuid"] = json!([1])),
            "uuid for a@example.com must be a string, got list",
        ),
        (
            with(&|e| e["added"] = json!({})),
            "added for a@example.com must be a string, got dict",
        ),
        (
            with(&|e| e["alias"] = json!(false)),
            "alias for a@example.com must be a string, got bool",
        ),
        (
            with(&|e| e["planType"] = json!(1.5)),
            "planType for a@example.com must be a string, got float",
        ),
        (
            with(&|e| e["alias"] = json!("-x")),
            "invalid alias for a@example.com: alias '-x' cannot start with '-' (would be read as a command flag)",
        ),
        (
            with(&|e| e["credentials"] = json!("sk-raw")),
            "credentials for a@example.com must be a JSON object",
        ),
        (
            remove("credentials"),
            "credentials for a@example.com must be a JSON object",
        ),
        (
            envelope(
                vec![
                    good(1, "a@example.com", "acct-a"),
                    good(2, "a@example.com", "acct-a"),
                ],
                None,
            ),
            "duplicate account in export: a@example.com (org=acct-a)",
        ),
        (
            envelope(
                vec![good(1, "a@example.com", ""), good(2, "a@example.com", "")],
                None,
            ),
            "duplicate account in export: a@example.com (org=personal)",
        ),
        (
            envelope(aliased_pair, None),
            "duplicate alias in export: dev",
        ),
    ];
    for (value, message) in cases {
        let err = import_value(&fx, &value, true).unwrap_err();
        assert_eq!(err.type_name(), "TransferError", "{value}");
        assert_eq!(err.to_string(), message, "{value}");
    }
    assert!(
        !fx.paths.backup_root.exists(),
        "a rejected import writes nothing"
    );

    let missing = fx.root.join("missing.ccsw");
    let err = import_accounts(&fx.paths, ImportSource::File(missing.clone()), false).unwrap_err();
    assert_eq!(err.type_name(), "TransferError");
    assert_eq!(
        err.to_string(),
        format!("import file not found: {}", missing.display())
    );
    let bad = fx.root.join("bad.ccsw");
    fs::write(&bad, "{nope").unwrap();
    let err = import_accounts(&fx.paths, ImportSource::File(bad), false).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("export file is not valid JSON: "),
        "{err}"
    );
}

#[test]
fn import_drops_an_alias_owned_locally_by_another_identity() {
    let fx = fixture();
    let mut ro = Roster::empty();
    let mut x = record("x@example.com", "acct-x");
    x.alias = Some("dev".into());
    add(
        &fx,
        &mut ro,
        1,
        x,
        Some(&chatgpt_auth("x@example.com", "acct-x", "rt-x")),
    );

    let mut y = entry(
        2,
        "y@example.com",
        "acct-y",
        chatgpt_auth("y@example.com", "acct-y", "rt-y"),
    );
    y["alias"] = json!("Dev");
    let report = import_value(&fx, &envelope(vec![y], None), false).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Warning: alias 'dev' for y@example.com already used by an existing account, dropping the imported alias",
            "Imported y@example.com → slot 2",
            "Done: 1 imported, 0 overwritten, 0 skipped",
        ]
    );
    let after = roster_of(&fx);
    assert_eq!(after.record(2).unwrap().alias, None);
    assert_eq!(after.record(1).unwrap().alias.as_deref(), Some("dev"));

    // The owning identity keeps its own alias through an overwrite.
    let mut same = entry(
        1,
        "x@example.com",
        "acct-x",
        chatgpt_auth("x@example.com", "acct-x", "rt-x2"),
    );
    same["alias"] = json!("dev");
    let report = import_value(&fx, &envelope(vec![same], None), true).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Overwrote x@example.com (slot 1)",
            "Done: 0 imported, 1 overwritten, 0 skipped",
        ]
    );
    assert_eq!(
        roster_of(&fx).record(1).unwrap().alias.as_deref(),
        Some("dev")
    );
}

#[test]
fn import_notes_when_the_live_login_slot_was_written() {
    let fx = fixture();
    write_live(&fx, &chatgpt_auth("a@example.com", "acct-a", "rt-live"));
    let value = envelope(
        vec![entry(
            4,
            "a@example.com",
            "acct-a",
            chatgpt_auth("a@example.com", "acct-a", "rt-a"),
        )],
        None,
    );
    let report = import_value(&fx, &value, false).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Imported a@example.com → slot 4",
            "Done: 1 imported, 0 overwritten, 0 skipped",
            "Note: a@example.com is your current live login — activate the imported credentials with: ccsw switch 4 --force",
        ]
    );
    // A second import skips the slot: nothing written, no note.
    let report = import_value(&fx, &value, false).unwrap();
    assert_eq!(
        report.notices.last().unwrap(),
        "Done: 0 imported, 0 overwritten, 1 skipped"
    );

    // An API-key live login is matched by its key.
    let fx = fixture();
    write_live(&fx, &api_key_auth("sk-live"));
    let mut e = entry(1, "api-key-1@token.local", "", api_key_auth("sk-live"));
    e["kind"] = json!("api_key");
    let report = import_value(&fx, &envelope(vec![e], None), false).unwrap();
    assert_eq!(
        report.notices.last().unwrap(),
        "Note: api-key-1@token.local is your current live login — activate the imported credentials with: ccsw switch 1 --force"
    );
    assert!(roster_of(&fx).record(1).unwrap().is_api_key());
}

#[test]
fn import_fills_missing_optional_fields() {
    let fx = fixture();
    let before = now_unix();
    let value = envelope(
        vec![json!({
            "number": 1,
            "email": "a@example.com",
            "organizationName": null,
            "alias": null,
            "credentials": chatgpt_auth("a@example.com", "acct-a", "rt")
        })],
        None,
    );
    import_value(&fx, &value, false).unwrap();
    let rec = roster_of(&fx).record(1).unwrap().clone();
    assert_eq!(rec.uuid, "");
    assert_eq!(rec.organization_uuid, "");
    assert_eq!(rec.organization_name, "");
    assert_eq!(rec.plan_type, None);
    assert_eq!(rec.alias, None);
    assert_eq!(rec.kind, None);
    assert!(!rec.disabled);
    assert!(
        parse_iso(&rec.added).unwrap() >= before,
        "added defaults to now"
    );

    // Without a `kind`, an API-key auth.json still registers as an API-key account.
    let value = envelope(
        vec![json!({"number": 2, "email": "k@example.com", "credentials": api_key_auth("sk-k")})],
        None,
    );
    import_value(&fx, &value, false).unwrap();
    assert!(roster_of(&fx).record(2).unwrap().is_api_key());
    assert_eq!(roster_of(&fx).sequence, vec![1, 2]);
}

#[test]
fn import_seeds_the_active_slot_only_when_unset() {
    let value = envelope(
        vec![entry(
            1,
            "a@example.com",
            "acct-a",
            chatgpt_auth("a@example.com", "acct-a", "rt-a"),
        )],
        Some(1),
    );

    // The envelope's active entry lands on another slot locally.
    let fx = fixture();
    let mut ro = Roster::empty();
    add(
        &fx,
        &mut ro,
        1,
        record("z@example.com", "acct-z"),
        Some(&chatgpt_auth("z@example.com", "acct-z", "rt-z")),
    );
    import_value(&fx, &value, false).unwrap();
    let after = roster_of(&fx);
    assert_eq!(after.record(2).unwrap().email, "a@example.com");
    assert_eq!(after.active_account_number, Some(2));

    // An existing (skipped) entry still resolves the envelope's active account.
    let fx = fixture();
    let mut ro = Roster::empty();
    add(
        &fx,
        &mut ro,
        3,
        record("a@example.com", "acct-a"),
        Some(&chatgpt_auth("a@example.com", "acct-a", "rt-a")),
    );
    import_value(&fx, &value, false).unwrap();
    assert_eq!(roster_of(&fx).active_account_number, Some(3));

    // An envelope without an active account leaves it unset.
    let fx = fixture();
    let unset = envelope(
        vec![entry(
            1,
            "a@example.com",
            "acct-a",
            chatgpt_auth("a@example.com", "acct-a", "rt-a"),
        )],
        None,
    );
    import_value(&fx, &unset, false).unwrap();
    assert_eq!(roster_of(&fx).active_account_number, None);
}

#[test]
fn cmd_wrappers_map_results_to_exit_codes() {
    let fx = fixture();
    let path = fx.root.join("x.ccsw");
    let path_str = path.to_str().unwrap();
    assert_eq!(
        export_cmd(&fx.paths, path_str, None, false),
        1,
        "nothing to export"
    );
    seed_three(&fx);
    assert_eq!(export_cmd(&fx.paths, path_str, None, false), 0);
    assert!(path.is_file());
    assert_eq!(export_cmd(&fx.paths, path_str, Some("nobody"), false), 1);
    assert_eq!(export_cmd(&fx.paths, "-", None, false), 0, "stdout export");

    let target = fixture();
    assert_eq!(
        import_cmd(
            &target.paths,
            path_str,
            ImportOptions {
                force: false,
                ..Default::default()
            }
        ),
        0
    );
    assert_eq!(roster_of(&target).sequence, vec![1, 2, 3]);
    let missing = target.root.join("missing.ccsw");
    assert_eq!(
        import_cmd(
            &target.paths,
            missing.to_str().unwrap(),
            ImportOptions {
                force: false,
                ..Default::default()
            }
        ),
        1
    );
}

fn cswap_envelope(accounts: Vec<Value>, active: Option<u32>) -> Value {
    json!({
        "version": 1,
        "exportedAt": "2026-01-01T00:00:00Z",
        "exportedFrom": "macos",
        "swapVersion": "0.25.0",
        "encrypted": false,
        "activeAccountNumber": active,
        "accounts": accounts
    })
}

fn cswap_oauth_entry(number: u32, email: &str, org: &str, org_name: &str, refresh: &str) -> Value {
    json!({
        "number": number,
        "email": email,
        "uuid": format!("uuid-{email}"),
        "organizationUuid": org,
        "organizationName": org_name,
        "added": "2024-01-01T00:00:00Z",
        "credentials": {"claudeAiOauth": {"accessToken": format!("cat-{refresh}"), "refreshToken": refresh,
                        "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]}},
        "config": {"oauthAccount": {"emailAddress": email, "accountUuid": format!("uuid-{email}"),
                   "organizationUuid": org, "organizationName": org_name}}
    })
}

fn sessions_marker_count(fx: &Fx) -> usize {
    fs::read_dir(fx.paths.sessions_dir())
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .ends_with(".ccsw-stale-credentials")
                })
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn import_round_trips_a_mixed_export() {
    let source = fixture();
    seed_mixed(&source);
    let path = source.root.join("mixed.ccsw");
    export_accounts(&source.paths, ExportTarget::File(path.clone()), None, false).unwrap();

    let target = fixture();
    let report = import_accounts(&target.paths, ImportSource::File(path.clone()), false).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Imported a@example.com → slot 1",
            "Imported c@example.com → slot 2",
            "Imported api-key-4@token.local → slot 4",
            "Done: 3 imported, 0 overwritten, 0 skipped",
        ]
    );
    let imported = roster_of(&target);
    let original = roster_of(&source);
    assert_eq!(imported.sequence, vec![1, 2, 4]);
    for slot in [1, 2, 4] {
        assert_eq!(imported.record(slot), original.record(slot), "slot {slot}");
        assert_eq!(slot_creds(&target, slot), slot_creds(&source, slot));
    }
    assert_eq!(imported.active_for(Provider::Codex), Some(1));
    assert_eq!(imported.active_for(Provider::Claude), Some(2));
    assert_eq!(imported.active_account_number, Some(1));

    // The same email and organization under the other provider is another account.
    let mut twin = roster_of(&target);
    let mut codex_twin = record("c@example.com", "org-c");
    codex_twin.organization_name = "Acme".into();
    add(
        &target,
        &mut twin,
        5,
        codex_twin,
        Some(&chatgpt_auth("c@example.com", "org-c", "rt-twin")),
    );
    let report = import_accounts(&target.paths, ImportSource::File(path), false).unwrap();
    assert_eq!(report.skipped, 3);
    assert_eq!(roster_of(&target).sequence, vec![1, 2, 4, 5]);
    assert_eq!(slot_creds(&target, 5)["tokens"]["refresh_token"], "rt-twin");
}

#[test]
fn import_reads_a_cswap_export() {
    let fx = fixture();
    let before = now_unix();
    // Entry 1: a `--full` export — sibling keys and a full config.
    let mut full = cswap_oauth_entry(1, "Alice@Example.com", "org-a", "Acme", "crt-a");
    full["credentials"]["mcpOAuth"] = json!({"srv": {"accessToken": "mcp"}});
    full["config"]["numStartups"] = json!(9);
    full["config"]["projects"] = json!({"/tmp/p": {}});
    full["alias"] = json!("work");
    // Entry 2: a managed API key as cswap writes it — a bare string.
    let key = json!({
        "number": 2, "email": "api-key-2@token.local", "uuid": "", "organizationUuid": "",
        "organizationName": "", "credentials": "sk-ant-api03-two", "kind": "api_key",
        "config": {"oauthAccount": {"emailAddress": "api-key-2@token.local", "accountUuid": "",
                   "organizationUuid": null, "organizationName": null}}
    });
    // Entry 3: a setup-token with no config at all.
    let setup = json!({
        "number": 3, "email": "setup-token-3@token.local",
        "credentials": {"claudeAiOauth": {"accessToken": "sk-ant-oat01-x", "scopes": ["user:inference"]}}
    });
    let report =
        import_value(&fx, &cswap_envelope(vec![full, key, setup], Some(2)), false).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Imported alice@example.com → slot 1",
            "Imported api-key-2@token.local → slot 2",
            "Imported setup-token-3@token.local → slot 3",
            "Done: 3 imported, 0 overwritten, 0 skipped",
        ]
    );
    let after = roster_of(&fx);
    let alice = after.record(1).unwrap();
    assert_eq!(alice.provider, Provider::Claude);
    assert_eq!(
        alice.email, "alice@example.com",
        "the Claude identity is lowercase"
    );
    assert_eq!(alice.organization_uuid, "org-a");
    assert_eq!(alice.organization_name, "Acme");
    assert_eq!(alice.alias.as_deref(), Some("work"));
    assert_eq!(alice.kind, None);
    assert_eq!(alice.added, "2024-01-01T00:00:00Z");
    assert_eq!(
        slot_creds(&fx, 1),
        json!({
            "claudeAiOauth": {"accessToken": "cat-crt-a", "refreshToken": "crt-a",
                              "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]},
            "oauthAccount": {"emailAddress": "Alice@Example.com", "accountUuid": "uuid-Alice@Example.com",
                             "organizationUuid": "org-a", "organizationName": "Acme"}
        }),
        "login part plus the lifted oauthAccount; siblings and other config keys dropped"
    );
    assert_eq!(
        after.find_slot(
            Provider::Claude,
            &Identity::new("alice@example.com", "org-a")
        ),
        Some(1),
        "the slot is found the way `add claude` looks it up"
    );
    let key = after.record(2).unwrap();
    assert!(key.is_api_key());
    assert_eq!(key.provider, Provider::Claude);
    assert_eq!(slot_creds(&fx, 2)["primaryApiKey"], "sk-ant-api03-two");
    assert_eq!(
        slot_creds(&fx, 2)["oauthAccount"]["emailAddress"],
        "api-key-2@token.local"
    );
    let setup = after.record(3).unwrap();
    assert_eq!(setup.kind, None);
    assert!(parse_iso(&setup.added).unwrap() >= before);
    assert_eq!(
        slot_creds(&fx, 3)["oauthAccount"],
        json!({"emailAddress": "setup-token-3@token.local", "accountUuid": "",
               "organizationUuid": null, "organizationName": null}),
        "no oauthAccount anywhere: synthesized from the entry"
    );
    assert_eq!(after.active_for(Provider::Claude), Some(2));
    assert_eq!(after.active_account_number, None, "Codex is untouched");

    // cswap writes 0 for "no active account".
    let fx = fixture();
    let only = cswap_oauth_entry(1, "b@example.com", "org-b", "", "crt-b");
    import_value(&fx, &cswap_envelope(vec![only], Some(0)), false).unwrap();
    assert_eq!(roster_of(&fx).active_for(Provider::Claude), None);
}

#[test]
fn import_keeps_v1_ccsw_envelopes_codex_and_defaults_v2_providers() {
    // A v1 ccsw file always carries `swapVersion` too; the ccsw marker wins.
    let fx = fixture();
    let v1 = envelope(
        vec![entry(
            1,
            "a@example.com",
            "acct-a",
            chatgpt_auth("a@example.com", "acct-a", "rt"),
        )],
        Some(1),
    );
    assert_eq!(v1["cswitchVersion"], "0.1.0");
    import_value(&fx, &v1, false).unwrap();
    assert_eq!(roster_of(&fx).record(1).unwrap().provider, Provider::Codex);
    assert_eq!(roster_of(&fx).active_account_number, Some(1));

    // An explicit `provider` in a v1 file is honoured (ccsw never wrote one).
    let fx = fixture();
    let v1 = envelope(
        vec![json!({
            "number": 1, "provider": "claude", "email": "c@example.com",
            "organizationUuid": "org-c",
            "credentials": {"claudeAiOauth": {"accessToken": "a", "refreshToken": "r"}}
        })],
        None,
    );
    import_value(&fx, &v1, false).unwrap();
    assert_eq!(roster_of(&fx).record(1).unwrap().provider, Provider::Claude);
    assert_eq!(
        slot_creds(&fx, 1)["oauthAccount"]["emailAddress"],
        "c@example.com"
    );
    let mut bad = v1.clone();
    bad["accounts"][0]["provider"] = json!("gemini");
    let err = import_value(&fx, &bad, false).unwrap_err();
    assert_eq!(
        err.to_string(),
        "provider for c@example.com must be \"codex\" or \"claude\", got 'gemini'"
    );

    // v2 without `provider` means Codex, as in the roster.
    let fx = fixture();
    let mut v2 = envelope(
        vec![entry(
            1,
            "a@example.com",
            "acct-a",
            chatgpt_auth("a@example.com", "acct-a", "rt"),
        )],
        None,
    );
    v2["version"] = json!(2);
    import_value(&fx, &v2, false).unwrap();
    assert_eq!(roster_of(&fx).record(1).unwrap().provider, Provider::Codex);

    // v2 Claude entry whose slot file lacks `oauthAccount`: synthesized from the entry.
    let fx = fixture();
    let mut v2 = envelope(
        vec![json!({
            "number": 1, "provider": "claude", "email": "c@example.com", "uuid": "u-c",
            "organizationUuid": "org-c", "organizationName": "Acme",
            "credentials": {"claudeAiOauth": {"accessToken": "a", "refreshToken": "r"}}
        })],
        None,
    );
    v2["version"] = json!(2);
    import_value(&fx, &v2, false).unwrap();
    assert_eq!(
        slot_creds(&fx, 1)["oauthAccount"],
        json!({"emailAddress": "c@example.com", "accountUuid": "u-c",
               "organizationUuid": "org-c", "organizationName": "Acme"})
    );
}

#[test]
fn import_rejects_malformed_provider_entries() {
    let fx = fixture();
    let v2 = |patch: &dyn Fn(&mut Value)| {
        let mut e = json!({
            "number": 1, "provider": "claude", "email": "c@example.com",
            "credentials": {"claudeAiOauth": {"accessToken": "a", "refreshToken": "r"}}
        });
        patch(&mut e);
        let mut env = envelope(vec![e], None);
        env["version"] = json!(2);
        env
    };
    let cases: Vec<(Value, &str)> = vec![
        (
            v2(&|e| e["provider"] = json!("gemini")),
            "provider for c@example.com must be \"codex\" or \"claude\", got 'gemini'",
        ),
        (
            v2(&|e| e["provider"] = json!(1)),
            "provider for c@example.com must be \"codex\" or \"claude\", got 1",
        ),
        (
            v2(&|e| e["credentials"] = json!("sk-ant-oat01-not-a-key")),
            "API-key credentials for c@example.com must be a raw sk-ant-api… string",
        ),
        (
            v2(&|e| e["kind"] = json!("api_key")),
            "API-key credentials for c@example.com must be a raw sk-ant-api… string",
        ),
        (
            v2(&|e| e["credentials"] = json!({"mcpOAuth": {}})),
            "credentials for c@example.com hold no Claude login (expected claudeAiOauth or primaryApiKey)",
        ),
        (
            v2(&|e| e["credentials"] = json!(7)),
            "credentials for c@example.com must be a JSON object",
        ),
        (
            v2(&|e| e["config"] = json!([1])),
            "config for c@example.com must be a JSON object",
        ),
        (
            cswap_envelope(
                vec![
                    cswap_oauth_entry(1, "a@example.com", "org", "", "r1"),
                    cswap_oauth_entry(2, "A@example.com", "org", "", "r2"),
                ],
                None,
            ),
            "duplicate account in export: a@example.com (org=org)",
        ),
    ];
    for (value, message) in cases {
        let err = import_value(&fx, &value, false).unwrap_err();
        assert_eq!(err.type_name(), "TransferError", "{value}");
        assert_eq!(err.to_string(), message, "{value}");
    }
    assert!(
        !fx.paths.backup_root.exists(),
        "a rejected import writes nothing"
    );
}

#[test]
fn import_force_on_a_claude_slot_marks_its_profile_stale() {
    let fx = fixture();
    seed_mixed(&fx);
    let profile = fx.paths.session_dir(2, "c@example.com");
    fs::create_dir_all(profile.join("sessions")).unwrap();
    let mut env = envelope(
        vec![cswap_oauth_entry(
            2,
            "c@example.com",
            "org-c",
            "Acme",
            "crt-new",
        )],
        None,
    );
    env["version"] = json!(2);
    env["accounts"][0]["provider"] = json!("claude");

    // A quiescent profile: overwritten, marked stale, no warning.
    let report = import_value(&fx, &env, true).unwrap();
    assert_eq!(
        report.notices,
        vec![
            "Overwrote c@example.com (slot 2)",
            "Done: 0 imported, 1 overwritten, 0 skipped",
        ]
    );
    assert_eq!(
        slot_creds(&fx, 2)["claudeAiOauth"]["refreshToken"],
        "crt-new"
    );
    assert_eq!(sessions_marker_count(&fx), 1, "the stale marker is written");

    // A live profile (this process's PID): still overwritten, with the warning.
    fs::write(
        profile.join("sessions").join("x.json"),
        json!({"pid": std::process::id()}).to_string(),
    )
    .unwrap();
    env["accounts"][0]["credentials"]["claudeAiOauth"]["refreshToken"] = json!("crt-newer");
    let report = import_value(&fx, &env, true).unwrap();
    assert_eq!(
        report.notices[0],
        "Warning: c@example.com (slot 2) has a live session-mode instance; its session profile keeps the pre-import credentials until it is restarted via 'ccsw run'."
    );
    assert_eq!(report.notices[1], "Overwrote c@example.com (slot 2)");
    assert_eq!(
        slot_creds(&fx, 2)["claudeAiOauth"]["refreshToken"],
        "crt-newer"
    );

    // An unparseable stored snapshot is uncertain ownership: refused, untouched.
    credentials::write(
        &fx.store,
        2,
        &json!({"claudeAiOauth": {"accessToken": "x"}}),
    )
    .unwrap();
    let err = import_value(&fx, &env, true).unwrap_err();
    assert_eq!(err.type_name(), "CredentialReadError");
    assert_eq!(
        slot_creds(&fx, 2),
        json!({"claudeAiOauth": {"accessToken": "x"}})
    );
}

#[test]
fn import_notes_a_written_claude_live_login() {
    let fx = fixture();
    let live = json!({"claudeAiOauth": {"accessToken": "la", "refreshToken": "crt-live",
                      "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]}});
    let config = json!({"oauthAccount": {"emailAddress": "C@example.com", "accountUuid": "u",
                        "organizationUuid": "org-c", "organizationName": "Acme"}});
    write_claude_live(&fx, &live, &config);
    let value = cswap_envelope(
        vec![cswap_oauth_entry(
            7,
            "c@example.com",
            "org-c",
            "Acme",
            "crt-c",
        )],
        None,
    );
    let report = import_value(&fx, &value, false).unwrap();
    assert_eq!(
        report.notices.last().unwrap(),
        "Note: c@example.com is your current live login — activate the imported credentials with: ccsw switch 7 --force"
    );
    // Both providers' live logins can be noted in one run.
    write_live(&fx, &chatgpt_auth("a@example.com", "acct-a", "rt-live"));
    let mut both = envelope(
        vec![
            entry(
                1,
                "a@example.com",
                "acct-a",
                chatgpt_auth("a@example.com", "acct-a", "rt-a"),
            ),
            cswap_oauth_entry(7, "c@example.com", "org-c", "Acme", "crt-c2"),
        ],
        None,
    );
    both["version"] = json!(2);
    both["accounts"][1]["provider"] = json!("claude");
    let report = import_value(&fx, &both, true).unwrap();
    let notes: Vec<&String> = report
        .notices
        .iter()
        .filter(|n| n.starts_with("Note: "))
        .collect();
    assert_eq!(notes.len(), 2, "{:?}", report.notices);
}
