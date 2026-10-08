//! Session mode against a fake `codex` on PATH: profile bootstrap, sharing,
//! launch argv and environment, token fold-back, `env` lines, and mappings.
//!
//! POSIX only: the fake `codex` is a shell script and sharing uses symlinks.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use ccsw::codex::auth::AuthJson;
use ccsw::model::{AccountKind, AccountRecord, Roster};
use ccsw::paths::Paths;
use ccsw::session::{
    self, CODEX_MISSING, EnvPlan, EnvRequest, HostEnv, MANIFEST_NAME, MappedAccount, RunTarget,
    ShareOptions, Shell,
};
use ccsw::store::{MappingStore, Store, credentials, roster};

const FAR: i64 = 4_102_444_800;

fn jwt(claims: &Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    format!("header.{payload}.sig")
}

fn chatgpt(email: &str, account_id: &str, refresh: &str, last_refresh: &str) -> AuthJson {
    AuthJson::from_value(json!({
        "OPENAI_API_KEY": null,
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": jwt(&json!({
                "email": email,
                "exp": FAR,
                "https://api.openai.com/auth": {"chatgpt_account_id": account_id, "chatgpt_plan_type": "plus"}
            })),
            "access_token": jwt(&json!({"exp": FAR})),
            "refresh_token": refresh,
            "account_id": account_id
        },
        "last_refresh": last_refresh
    }))
}

const FAKE_CODEX: &str = r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  echo "Options:"
  echo "  --no-daemon  Run in process"
  exit 0
fi
: > "$CCSW_TEST_RECORD"
for arg in "$@"; do printf '%s\n' "$arg" >> "$CCSW_TEST_RECORD"; done
printf 'HOME=%s\n' "${CODEX_HOME-unset}" >> "$CCSW_TEST_RECORD"
exit 7
"#;

struct World {
    dir: tempfile::TempDir,
    store: Store,
    roster: Roster,
    codex: PathBuf,
}

impl World {
    fn new() -> Self {
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
        for (slot, email, account_id) in [(1, "a@x.com", "acct-1"), (2, "b@x.com", "acct-2")] {
            let mut record = AccountRecord::new(email);
            record.organization_uuid = account_id.into();
            roster.add_record(slot, record);
            credentials::write(
                &store,
                slot,
                &chatgpt(
                    email,
                    account_id,
                    &format!("rt-{slot}"),
                    "2026-09-29T10:00:00Z",
                )
                .0,
            )
            .unwrap();
        }
        roster::write(&store.paths, &roster).unwrap();

        let source = &store.paths.codex_home;
        fs::create_dir_all(source.join("prompts")).unwrap();
        fs::create_dir_all(source.join("skills")).unwrap();
        fs::write(source.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
        fs::write(source.join("AGENTS.md"), "# agents\n").unwrap();
        fs::write(source.join("prompts/hello.md"), "hi\n").unwrap();

        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex");
        fs::write(&codex, FAKE_CODEX).unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            dir,
            store,
            roster,
            codex,
        }
    }

    fn host(&self) -> HostEnv {
        HostEnv {
            codex_home_preset: None,
            set_vars: Vec::new(),
            codex: Some(self.codex.clone()),
        }
    }

    fn profile(&self, slot: u32) -> PathBuf {
        let email = &self.roster.record(slot).unwrap().email;
        self.store.paths.session_dir(slot, email)
    }

    fn write_live(&self, auth: &AuthJson) {
        auth.write(&self.store.paths.live_auth_file()).unwrap();
    }

    /// Run the planned command and return the recorded argv and `CODEX_HOME`.
    fn execute(&self, launch: &mut session::Launch) -> (i32, Vec<String>, String) {
        let record = self.dir.path().join(format!(
            "record-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let status = launch
            .command
            .env("CCSW_TEST_RECORD", &record)
            .status()
            .unwrap();
        let text = fs::read_to_string(&record).unwrap();
        let mut argv = Vec::new();
        let mut home = String::new();
        for line in text.lines() {
            match line.strip_prefix("HOME=") {
                Some(value) => home = value.to_string(),
                None => argv.push(line.to_string()),
            }
        }
        (status.code().unwrap(), argv, home)
    }
}

fn manifest(profile: &Path) -> Option<Value> {
    fs::read(profile.join(MANIFEST_NAME))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn is_symlink_to(path: &Path, target: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink())
        && fs::read_link(path).is_ok_and(|link| link == target)
}

#[test]
fn bootstrap_writes_credentials_config_and_links() {
    let world = World::new();
    let prepared =
        session::prepare_profile(&world.store, &world.roster, 2, ShareOptions::default()).unwrap();
    let profile = world.profile(2);
    assert_eq!(prepared.profile, profile);
    assert_eq!(prepared.slot, 2);
    assert_eq!(prepared.email, "b@x.com");
    assert!(prepared.notices.is_empty(), "{:?}", prepared.notices);
    assert_eq!(
        fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
        0o700
    );

    let auth = AuthJson::read(&profile.join("auth.json")).unwrap().unwrap();
    assert_eq!(auth.refresh_token(), Some("rt-2"));
    assert_eq!(
        fs::metadata(profile.join("auth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::read_to_string(profile.join("config.toml")).unwrap(),
        "model = \"gpt-5\"\n"
    );
    let source = &world.store.paths.codex_home;
    for item in ["AGENTS.md", "prompts", "skills"] {
        assert!(
            is_symlink_to(&profile.join(item), &source.join(item)),
            "{item} is linked"
        );
    }
    assert!(
        !profile.join("sessions").exists(),
        "history is not shared by default"
    );
    assert_eq!(
        manifest(&profile).unwrap(),
        json!({"items": ["AGENTS.md", "prompts", "skills"], "mode": "symlink"})
    );

    // Every launch copies the config again and keeps the links.
    fs::write(source.join("config.toml"), "model = \"gpt-6\"\n").unwrap();
    session::prepare_profile(&world.store, &world.roster, 2, ShareOptions::default()).unwrap();
    assert_eq!(
        fs::read_to_string(profile.join("config.toml")).unwrap(),
        "model = \"gpt-6\"\n"
    );
    assert!(is_symlink_to(
        &profile.join("prompts"),
        &source.join("prompts")
    ));

    // A missing source config yields none; a missing source item is skipped.
    fs::remove_file(source.join("config.toml")).unwrap();
    fs::remove_dir_all(source.join("skills")).unwrap();
    fs::remove_file(profile.join("config.toml")).unwrap();
    session::prepare_profile(&world.store, &world.roster, 2, ShareOptions::default()).unwrap();
    assert!(!profile.join("config.toml").exists());
    assert_eq!(
        manifest(&profile).unwrap()["items"],
        json!(["AGENTS.md", "prompts"])
    );
}

#[test]
fn no_share_prunes_links_and_the_manifest() {
    let world = World::new();
    session::prepare_profile(&world.store, &world.roster, 1, ShareOptions::default()).unwrap();
    let profile = world.profile(1);
    assert!(profile.join("AGENTS.md").exists());
    // The profile's own file (not a link, not in the manifest) survives pruning.
    fs::write(profile.join("history.jsonl"), "{}\n").unwrap();

    let prepared = session::prepare_profile(
        &world.store,
        &world.roster,
        1,
        ShareOptions {
            share: false,
            share_history: false,
        },
    )
    .unwrap();
    assert!(prepared.notices.is_empty());
    for item in ["AGENTS.md", "prompts", "skills"] {
        assert!(
            fs::symlink_metadata(profile.join(item)).is_err(),
            "{item} pruned"
        );
    }
    assert!(manifest(&profile).is_none(), "no active items, no manifest");
    assert!(profile.join("history.jsonl").exists());
    assert!(
        world.store.paths.codex_home.join("AGENTS.md").exists(),
        "pruning a link never touches the source"
    );
}

#[test]
fn share_history_links_sessions_and_history() {
    let world = World::new();
    let source = &world.store.paths.codex_home;
    fs::create_dir_all(source.join("sessions/2026")).unwrap();
    fs::write(source.join("history.jsonl"), "{\"a\":1}\n").unwrap();
    let opts = ShareOptions {
        share: true,
        share_history: true,
    };
    session::prepare_profile(&world.store, &world.roster, 1, opts).unwrap();
    let profile = world.profile(1);
    assert!(is_symlink_to(
        &profile.join("sessions"),
        &source.join("sessions")
    ));
    assert!(is_symlink_to(
        &profile.join("history.jsonl"),
        &source.join("history.jsonl")
    ));
    assert_eq!(
        manifest(&profile).unwrap()["items"],
        json!([
            "AGENTS.md",
            "prompts",
            "skills",
            "sessions",
            "history.jsonl"
        ])
    );
    // Back to per-account history: only the history links go.
    session::prepare_profile(&world.store, &world.roster, 1, ShareOptions::default()).unwrap();
    assert!(fs::symlink_metadata(profile.join("sessions")).is_err());
    assert!(fs::symlink_metadata(profile.join("history.jsonl")).is_err());
    assert!(is_symlink_to(
        &profile.join("prompts"),
        &source.join("prompts")
    ));
}

#[test]
fn a_profiles_own_copy_is_kept_and_a_wrong_link_is_repointed() {
    let world = World::new();
    let profile = world.profile(2);
    fs::create_dir_all(profile.join("prompts")).unwrap();
    fs::write(profile.join("prompts/mine.md"), "mine\n").unwrap();
    let other = world.dir.path().join("elsewhere");
    fs::create_dir_all(&other).unwrap();
    std::os::unix::fs::symlink(&other, profile.join("skills")).unwrap();

    let prepared =
        session::prepare_profile(&world.store, &world.roster, 2, ShareOptions::default()).unwrap();
    assert_eq!(prepared.notices.len(), 1);
    assert!(
        prepared.notices[0]
            .contains("Not sharing prompts: the session profile already has its own copy."),
        "{}",
        prepared.notices[0]
    );
    assert!(profile.join("prompts/mine.md").exists());
    assert!(is_symlink_to(
        &profile.join("skills"),
        &world.store.paths.codex_home.join("skills")
    ));
    assert_eq!(
        manifest(&profile).unwrap()["items"],
        json!(["AGENTS.md", "skills"])
    );
}

#[test]
fn launch_runs_codex_in_the_profile_with_no_daemon_and_scrubs_keys() {
    let world = World::new();
    let mut host = world.host();
    host.set_vars = vec!["OPENAI_API_KEY".into()];
    let tail = vec!["exec".to_string(), "hello there".to_string()];
    let mut launch = session::plan_launch(
        &world.store,
        &world.roster,
        &host,
        RunTarget::Slot(2),
        tail,
        ShareOptions::default(),
    )
    .unwrap();
    let profile = world.profile(2);
    assert_eq!(launch.session, Some((2, profile.clone())));
    assert!(
        launch
            .notices
            .iter()
            .any(|n| n.contains("Ignoring OPENAI_API_KEY for this session — it would override the selected account inside Codex.")),
        "{:?}",
        launch.notices
    );
    assert!(
        launch
            .notices
            .last()
            .unwrap()
            .contains("Account-2 (b@x.com)"),
        "{:?}",
        launch.notices
    );
    assert!(launch.notices.last().unwrap().contains("[session mode]"));
    let mut removed: Vec<String> = launch
        .command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    removed.sort();
    assert_eq!(removed, ["CODEX_API_KEY", "OPENAI_API_KEY"]);

    let (code, argv, home) = world.execute(&mut launch);
    assert_eq!(code, 7, "the child's status is what the caller mirrors");
    assert_eq!(argv, ["--no-daemon", "exec", "hello there"]);
    assert_eq!(home, profile.to_string_lossy());
    assert!(profile.join("auth.json").exists());

    // An argv that already picks its server is forwarded untouched.
    let mut launch = session::plan_launch(
        &world.store,
        &world.roster,
        &world.host(),
        RunTarget::Slot(2),
        vec!["--remote".into()],
        ShareOptions::default(),
    )
    .unwrap();
    let (_, argv, _) = world.execute(&mut launch);
    assert_eq!(argv, ["--remote"]);
}

#[test]
fn same_account_fast_path_and_default_launch() {
    let world = World::new();
    world.write_live(&chatgpt(
        "b@x.com",
        "acct-2",
        "rt-2",
        "2026-09-29T10:00:00Z",
    ));
    let mut launch = session::plan_launch(
        &world.store,
        &world.roster,
        &world.host(),
        RunTarget::Slot(2),
        vec!["--resume".into()],
        ShareOptions::default(),
    )
    .unwrap();
    assert_eq!(launch.session, None);
    assert_eq!(
        launch.notices,
        ["Account-2 (b@x.com) is already the active default login — launching codex directly."]
    );
    let (code, argv, home) = world.execute(&mut launch);
    assert_eq!(code, 7);
    assert_eq!(argv, ["--resume"], "no --no-daemon on the fast path");
    assert_eq!(home, "unset");
    assert!(!world.profile(2).exists(), "no profile is prepared");

    // Another live login: the profile path applies.
    world.write_live(&chatgpt(
        "a@x.com",
        "acct-1",
        "rt-1",
        "2026-09-29T10:00:00Z",
    ));
    let launch = session::plan_launch(
        &world.store,
        &world.roster,
        &world.host(),
        RunTarget::Slot(2),
        vec![],
        ShareOptions::default(),
    )
    .unwrap();
    assert!(launch.session.is_some());

    // A preset CODEX_HOME is overridden, never a fast path.
    let mut host = world.host();
    host.codex_home_preset = Some("/elsewhere".into());
    world.write_live(&chatgpt(
        "b@x.com",
        "acct-2",
        "rt-2",
        "2026-09-29T10:00:00Z",
    ));
    let mut launch = session::plan_launch(
        &world.store,
        &world.roster,
        &host,
        RunTarget::Slot(2),
        vec![],
        ShareOptions::default(),
    )
    .unwrap();
    assert!(launch.session.is_some());
    assert!(
        launch.notices[0]
            .contains("CODEX_HOME is already set (/elsewhere); overriding it for this launch."),
        "{:?}",
        launch.notices
    );
    let (_, argv, home) = world.execute(&mut launch);
    assert_eq!(argv, ["--no-daemon"]);
    assert_eq!(home, world.profile(2).to_string_lossy());

    // The default launch: plain codex, env untouched, the notice explains.
    let mut launch = session::plan_launch(
        &world.store,
        &world.roster,
        &world.host(),
        RunTarget::Default {
            notice: Some("why".into()),
        },
        vec!["exec".into()],
        ShareOptions::default(),
    )
    .unwrap();
    assert_eq!(launch.notices, ["why"]);
    let (_, argv, home) = world.execute(&mut launch);
    assert_eq!(argv, ["exec"]);
    assert_eq!(home, "unset");
}

#[test]
fn missing_codex_and_missing_credentials_are_session_errors() {
    let world = World::new();
    let host = HostEnv {
        codex: None,
        ..world.host()
    };
    let err = session::plan_launch(
        &world.store,
        &world.roster,
        &host,
        RunTarget::Slot(2),
        vec![],
        ShareOptions::default(),
    )
    .unwrap_err();
    assert_eq!(err.type_name(), "SessionError");
    assert_eq!(err.to_string(), CODEX_MISSING);

    credentials::delete(&world.store, 2).unwrap();
    let err = session::plan_launch(
        &world.store,
        &world.roster,
        &world.host(),
        RunTarget::Slot(2),
        vec![],
        ShareOptions::default(),
    )
    .unwrap_err();
    assert_eq!(err.type_name(), "SessionError");
    assert_eq!(
        err.to_string(),
        "Account-2 has no stored credentials. Re-add with: ccsw add --slot 2"
    );
    assert!(
        session::plan_launch(
            &world.store,
            &world.roster,
            &world.host(),
            RunTarget::Slot(9),
            vec![],
            ShareOptions::default(),
        )
        .is_err()
    );
}

#[test]
fn api_key_accounts_run_in_session_mode() {
    let mut world = World::new();
    let mut record = AccountRecord::new("api-key-3@token.local");
    record.kind = Some(AccountKind::ApiKey);
    world.roster.add_record(3, record);
    credentials::write(&world.store, 3, &AuthJson::api_key_auth("sk-3").0).unwrap();
    let mut launch = session::plan_launch(
        &world.store,
        &world.roster,
        &world.host(),
        RunTarget::Slot(3),
        vec![],
        ShareOptions::default(),
    )
    .unwrap();
    let (code, _, home) = world.execute(&mut launch);
    assert_eq!(code, 7);
    let profile = PathBuf::from(home);
    assert_eq!(
        AuthJson::read(&profile.join("auth.json"))
            .unwrap()
            .unwrap()
            .api_key(),
        Some("sk-3")
    );
}

#[test]
fn fold_back_adopts_newer_profile_tokens_only() {
    let world = World::new();
    session::prepare_profile(&world.store, &world.roster, 1, ShareOptions::default()).unwrap();
    let profile = world.profile(1);

    // Codex rotated the token inside the profile: newer stamp, other token.
    chatgpt("a@x.com", "acct-1", "rt-1-rotated", "2026-09-29T11:00:00Z")
        .write(&profile.join("auth.json"))
        .unwrap();
    assert!(session::fold_back(&world.store, 1, &profile).unwrap());
    let stored = AuthJson::from_value(credentials::read(&world.store, 1).unwrap().unwrap());
    assert_eq!(stored.refresh_token(), Some("rt-1-rotated"));
    assert!(
        !session::fold_back(&world.store, 1, &profile).unwrap(),
        "same token: nothing"
    );

    // Older, foreign, or API-key profile files never win.
    chatgpt("a@x.com", "acct-1", "rt-old", "2026-09-29T09:00:00Z")
        .write(&profile.join("auth.json"))
        .unwrap();
    assert!(!session::fold_back(&world.store, 1, &profile).unwrap());
    chatgpt("z@x.com", "acct-z", "rt-z", "2026-09-30T09:00:00Z")
        .write(&profile.join("auth.json"))
        .unwrap();
    assert!(!session::fold_back(&world.store, 1, &profile).unwrap());
    AuthJson::api_key_auth("sk")
        .write(&profile.join("auth.json"))
        .unwrap();
    assert!(!session::fold_back(&world.store, 1, &profile).unwrap());
    assert_eq!(
        AuthJson::from_value(credentials::read(&world.store, 1).unwrap().unwrap()).refresh_token(),
        Some("rt-1-rotated")
    );

    // The next bootstrap folds first, then writes the slot's (newest) copy back.
    chatgpt("a@x.com", "acct-1", "rt-1-again", "2026-09-29T12:00:00Z")
        .write(&profile.join("auth.json"))
        .unwrap();
    session::prepare_profile(&world.store, &world.roster, 1, ShareOptions::default()).unwrap();
    assert_eq!(
        AuthJson::from_value(credentials::read(&world.store, 1).unwrap().unwrap()).refresh_token(),
        Some("rt-1-again")
    );
    assert_eq!(
        AuthJson::read(&profile.join("auth.json"))
            .unwrap()
            .unwrap()
            .refresh_token(),
        Some("rt-1-again")
    );
}

#[test]
fn env_prints_eval_lines_and_notes() {
    let world = World::new();
    let cwd = world.dir.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    let mut host = world.host();
    host.set_vars = vec!["CODEX_API_KEY".into()];
    let plan = session::plan_env(
        &world.store,
        &world.roster,
        &host,
        EnvRequest {
            account: Some("2"),
            cwd: &cwd,
            shell: Shell::Sh,
            unset: false,
            opts: ShareOptions::default(),
        },
    )
    .unwrap();
    let profile = world.profile(2);
    let EnvPlan::Lines { lines, notices } = plan else {
        panic!("lines expected");
    };
    assert_eq!(
        lines,
        [
            "unset CODEX_API_KEY".to_string(),
            format!("export CODEX_HOME='{}'", profile.display())
        ]
    );
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0].starts_with("Prepared Account-2 (b@x.com)"),
        "{}",
        notices[0]
    );
    assert!(
        profile.join("auth.json").exists(),
        "env prepares the profile"
    );

    let plan = session::plan_env(
        &world.store,
        &world.roster,
        &world.host(),
        EnvRequest {
            account: Some("b@x.com"),
            cwd: &cwd,
            shell: Shell::Fish,
            unset: false,
            opts: ShareOptions::default(),
        },
    )
    .unwrap();
    assert!(
        matches!(plan, EnvPlan::Lines { lines, .. } if lines == [format!("set -gx CODEX_HOME '{}'", profile.display())])
    );

    // --unset needs no account and no store contents.
    let plan = session::plan_env(
        &world.store,
        &Roster::empty(),
        &world.host(),
        EnvRequest {
            account: None,
            cwd: &cwd,
            shell: Shell::Pwsh,
            unset: true,
            opts: ShareOptions::default(),
        },
    )
    .unwrap();
    assert_eq!(
        plan,
        EnvPlan::Lines {
            lines: vec!["Remove-Item Env:CODEX_HOME -ErrorAction SilentlyContinue".into()],
            notices: vec![]
        }
    );

    // No account and no mapping: an error that names the ways out.
    let err = session::plan_env(
        &world.store,
        &world.roster,
        &world.host(),
        EnvRequest {
            account: None,
            cwd: &cwd,
            shell: Shell::Sh,
            unset: false,
            opts: ShareOptions::default(),
        },
    )
    .unwrap_err();
    assert_eq!(err.type_name(), "SessionError");
    assert!(
        err.to_string()
            .starts_with("Nothing to prepare an environment for (")
    );
    assert!(
        err.to_string()
            .ends_with("or clear a pinned profile with ccsw env --unset.")
    );

    // The active login with no preset: nothing exported.
    world.write_live(&chatgpt(
        "a@x.com",
        "acct-1",
        "rt-1",
        "2026-09-29T10:00:00Z",
    ));
    let plan = session::plan_env(
        &world.store,
        &world.roster,
        &world.host(),
        EnvRequest {
            account: Some("1"),
            cwd: &cwd,
            shell: Shell::Sh,
            unset: false,
            opts: ShareOptions::default(),
        },
    )
    .unwrap();
    assert_eq!(
        plan,
        EnvPlan::Note(
            "Account-1 (a@x.com) is the active default login — an unpinned shell already uses it; nothing exported."
                .into()
        )
    );

    // A mapped directory selects the account.
    session::map(&world.store, &world.roster, Some("2"), None, &cwd).unwrap();
    let plan = session::plan_env(
        &world.store,
        &world.roster,
        &world.host(),
        EnvRequest {
            account: None,
            cwd: &cwd.join("sub"),
            shell: Shell::Sh,
            unset: false,
            opts: ShareOptions::default(),
        },
    )
    .unwrap();
    assert!(matches!(plan, EnvPlan::Lines { .. }));
}

#[test]
fn map_unmap_and_run_target_resolution() {
    let mut world = World::new();
    let work = world.dir.path().join("work");
    fs::create_dir_all(work.join("deeper")).unwrap();
    let normalized = MappingStore::normalize_path(&work);
    let cwd = world.dir.path().to_path_buf();

    let lines = session::map(&world.store, &world.roster, None, None, &cwd).unwrap();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("No directory mappings yet."));
    assert!(lines[1].contains("Map one with: ccsw map <NUM|EMAIL> [PATH]"));

    let lines = session::map(&world.store, &world.roster, Some("2"), Some(&work), &cwd).unwrap();
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].contains(&format!(
            "Mapped {} → Account-2 (b@x.com)",
            normalized.display()
        )),
        "{}",
        lines[0]
    );
    assert!(!lines[0].contains("(was"));

    let lines = session::map(
        &world.store,
        &world.roster,
        Some("a@x.com"),
        Some(&work),
        &cwd,
    )
    .unwrap();
    assert!(lines[0].contains("Account-1 (a@x.com)"));
    assert!(lines[0].contains("(was b@x.com)"), "{}", lines[0]);

    let ghost = world.dir.path().join("ghost");
    let lines = session::map(&world.store, &world.roster, Some("2"), Some(&ghost), &cwd).unwrap();
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].contains(&format!(
            "Warning: {} is not an existing directory (mapping it anyway)",
            ghost.display()
        )),
        "{}",
        lines[0]
    );

    let lines = session::map(&world.store, &world.roster, None, None, &cwd).unwrap();
    assert!(lines[0].contains("Directory mappings:"));
    assert_eq!(lines.len(), 3);
    assert!(lines[1].contains(&format!(
        "{} ",
        MappingStore::normalize_path(&ghost).display()
    )));
    assert!(lines[1].contains("2: b@x.com") && lines[1].contains("[personal]"));
    assert!(lines[2].contains("1: a@x.com"));

    // Resolution walks up to the nearest mapped ancestor.
    assert_eq!(
        session::mapped_account(&world.store, &world.roster, &work.join("deeper")),
        MappedAccount::Slot(1)
    );
    assert_eq!(
        session::resolve_run_target(&world.store, &world.roster, None, &work.join("deeper"))
            .unwrap(),
        RunTarget::Slot(1)
    );
    assert_eq!(
        session::resolve_run_target(&world.store, &world.roster, Some("b@x.com"), &cwd).unwrap(),
        RunTarget::Slot(2)
    );
    assert_eq!(
        session::resolve_run_target(&world.store, &world.roster, Some("nobody"), &cwd)
            .unwrap_err()
            .to_string(),
        "No account found with identifier: nobody"
    );
    let RunTarget::Default { notice } =
        session::resolve_run_target(&world.store, &world.roster, None, &cwd).unwrap()
    else {
        panic!("unmapped cwd launches the default account");
    };
    assert!(notice.unwrap().contains(&format!(
        "No account mapped for {} — launching the default account.",
        cwd.display()
    )));

    // The mapped account leaves the roster.
    world.roster.remove_slot(1);
    assert_eq!(
        session::mapped_account(&world.store, &world.roster, &work),
        MappedAccount::Removed {
            email: "a@x.com".into()
        }
    );
    let RunTarget::Default { notice } =
        session::resolve_run_target(&world.store, &world.roster, None, &work).unwrap()
    else {
        panic!("removed account launches the default account");
    };
    assert!(
        notice
            .unwrap()
            .contains("Mapped account a@x.com no longer exists — launching the default account.")
    );
    let lines = session::map(&world.store, &world.roster, None, None, &cwd).unwrap();
    assert!(
        lines[2].contains("a@x.com (account removed)"),
        "{}",
        lines[2]
    );

    let line = session::unmap(&world.store, Some(&work.join(".")), &cwd).unwrap();
    assert!(
        line.contains(&format!("Unmapped {}", normalized.display())),
        "{line}"
    );
    let line = session::unmap(&world.store, Some(&work), &cwd).unwrap();
    assert!(
        line.contains(&format!("No mapping for {}", normalized.display())),
        "{line}"
    );
    let line = session::unmap(&world.store, None, &ghost).unwrap();
    assert!(
        line.starts_with(&format!(
            "Unmapped {}",
            MappingStore::normalize_path(&ghost).display()
        )) || line.contains("Unmapped")
    );
    assert!(
        MappingStore::load(&world.store.paths).is_empty(),
        "mappings persisted through save()"
    );
}
