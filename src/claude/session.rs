//! Managed profile credentials. A running profile owns its refresh token.
//!
//! Ported from claude-swap's session.py and process_detection.py at 3a4e5c1.
//! Copyright (c) 2026 Onur Cetinkol. MIT license; see NOTICE.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use super::credentials::{ClaudeCredential, CredentialKind, OauthAccount, SlotFile};
use super::keychain::{Keychain, SecurityCli, account_name};
use crate::errors::{CcswError, Result};
use crate::fsutil::{read_json, write_json_private};
use crate::model::AccountRecord;
use crate::provider::Provider;
use crate::store::{Store, credentials, ensure_private_dir};

pub const SHARED_ITEMS: &[&str] = &[
    "settings.json",
    "keybindings.json",
    "CLAUDE.md",
    "skills",
    "commands",
    "agents",
];
pub const HISTORY_ITEMS: &[&str] = &["projects", "history.jsonl"];
pub const SCRUBBED_ENV: &[&str] = &[
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
];

/// Hash the exact exported string, without path normalization.
pub fn keychain_service(config_dir: &str) -> String {
    let normalized: String = config_dir.nfc().collect();
    format!(
        "Claude Code-credentials-{}",
        &hex::encode(Sha256::digest(normalized.as_bytes()))[..8]
    )
}

fn failure(profile: &Path, detail: impl std::fmt::Display) -> CcswError {
    CcswError::session(format!("{}: {detail}", profile.display()))
}

/// An unreadable record is not proof that its process has exited. Reused PIDs
/// can cause a conservative refusal; they must never permit credential writes.
pub fn is_quiescent(profile: &Path) -> bool {
    for directory in [profile.join("sessions"), profile.join(".ccsw-launches")] {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return false,
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            if entry.path().extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let pid = read_json(&entry.path())
                .ok()
                .flatten()
                .and_then(|value| value.get("pid").and_then(|v| v.as_u64()))
                .and_then(|pid| u32::try_from(pid).ok())
                .filter(|pid| *pid > 1);
            if pid.is_none_or(process_alive) {
                return false;
            }
        }
    }
    true
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    // SAFETY: signal 0 checks existence and does not send a signal.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: the handle is used only for an existence check and closed once.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        CloseHandle(handle);
        true
    }
}

pub fn require_quiescent(profile: &Path) -> Result<()> {
    if !is_quiescent(profile) {
        return Err(failure(
            profile,
            "a session is running or its PID records are unreadable; close it and retry",
        ));
    }
    Ok(())
}

/// Read Keychain first. Unlike a default-login best-effort read, failure must
/// not expose a stale plaintext seed as the current session credential.
pub(crate) fn read(
    store: &Store,
    profile: &Path,
    cli: &dyn SecurityCli,
) -> Result<Option<SlotFile>> {
    if !profile.exists() {
        return Ok(None);
    }
    let text = if store.paths.keychain_enabled {
        Keychain::new(cli)
            .get_password(
                &keychain_service(&profile.to_string_lossy()),
                &account_name(),
            )
            .map_err(|err| failure(profile, err))?
    } else {
        None
    };
    let credential = match text {
        Some(text) => Some(ClaudeCredential::parse(&text)?),
        None => read_json(&profile.join(".credentials.json"))
            .map_err(|err| failure(profile, err))?
            .map(ClaudeCredential::from_value),
    };
    let Some(credential) = credential else {
        return Ok(None);
    };
    let config_path = if profile.join(".config.json").exists() {
        profile.join(".config.json")
    } else {
        profile.join(".claude.json")
    };
    let config = read_json(&config_path)
        .map_err(|err| failure(profile, err))?
        .ok_or_else(|| failure(profile, "session identity is missing"))?;
    let account = config
        .get("oauthAccount")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| failure(profile, "session identity is missing"))?;
    Ok(Some(SlotFile::new(&credential, OauthAccount(account))))
}

fn matches_record(file: &SlotFile, record: &AccountRecord) -> bool {
    file.oauth_account.identity().as_ref() == Some(&record.identity())
}

/// Caller holds the store lock. Never attach an in-session /login to the old
/// slot, or replace a newer backup with the original profile seed.
pub(crate) fn reconcile_locked(
    store: &Store,
    slot: u32,
    record: &AccountRecord,
    cli: &dyn SecurityCli,
) -> Result<()> {
    if record.provider != Provider::Claude {
        return Ok(());
    }
    let profile = store.paths.session_dir(slot, &record.email);
    let Some(incoming) = read(store, &profile, cli)? else {
        return Ok(());
    };
    if !matches_record(&incoming, record) {
        return Err(failure(
            &profile,
            "session identity changed; close the session and save its login as a separate account",
        ));
    }
    let Some(saved) = credentials::read(store, slot)? else {
        return Ok(());
    };
    let stored = SlotFile::from_value(&saved)?;
    if incoming.credential.is_newer_than(&stored.credential) {
        credentials::write(store, slot, &incoming.to_value())?;
    }
    Ok(())
}

pub(crate) fn reconcile(
    store: &Store,
    slot: u32,
    record: &AccountRecord,
    cli: &dyn SecurityCli,
) -> Result<()> {
    let _lock = store.lock()?;
    reconcile_locked(store, slot, record, cli)
}

/// A read-only view for usage and refresh ownership checks.
pub(crate) fn current(
    store: &Store,
    slot: u32,
    record: &AccountRecord,
    cli: &dyn SecurityCli,
) -> Result<Option<SlotFile>> {
    let profile = store.paths.session_dir(slot, &record.email);
    let value = read(store, &profile, cli)?;
    if value
        .as_ref()
        .is_some_and(|file| !matches_record(file, record))
    {
        return Err(failure(&profile, "session identity changed"));
    }
    Ok(value)
}

pub(crate) fn prepare(
    store: &Store,
    slot: u32,
    record: &AccountRecord,
    cli: &dyn SecurityCli,
) -> Result<PathBuf> {
    let profile = store.paths.session_dir(slot, &record.email);
    let _lock = store.lock()?;
    reconcile_locked(store, slot, record, cli)?;
    let stored = credentials::read(store, slot)?
        .ok_or_else(|| failure(&profile, "stored credentials are missing"))?;
    let file = SlotFile::from_value(&stored)?;
    if !matches!(
        file.credential.kind(),
        CredentialKind::OAuth | CredentialKind::SetupToken
    ) {
        return Err(failure(
            &profile,
            "session mode requires an OAuth login or setup token",
        ));
    }
    if !is_quiescent(&profile) {
        let current = current(store, slot, record, cli)?
            .ok_or_else(|| failure(&profile, "a running session has no readable credentials"))?;
        if file.credential.is_newer_than(&current.credential) {
            return Err(failure(
                &profile,
                "stored credentials changed; close the running session before starting another",
            ));
        }
        return Ok(profile);
    }
    // cswap reuses the existing profile when it holds the same generation.
    if current(store, slot, record, cli)?
        .is_some_and(|current| current.credential.fingerprint() == file.credential.fingerprint())
    {
        return Ok(profile);
    }
    ensure_private_dir(&store.paths.sessions_dir())?;
    ensure_private_dir(&profile)?;
    // Keychain shadows the plaintext seed. Refuse on failure rather than
    // silently launching with a stale credential.
    if store.paths.keychain_enabled {
        Keychain::new(cli)
            .delete_password(
                &keychain_service(&profile.to_string_lossy()),
                &account_name(),
            )
            .map_err(|err| failure(&profile, err))?;
    }
    write_json_private(&profile.join(".credentials.json"), &file.credential.0)
        .map_err(|err| failure(&profile, err))?;
    let config_path = if profile.join(".config.json").exists() {
        profile.join(".config.json")
    } else {
        profile.join(".claude.json")
    };
    let mut config = read_json(&config_path)
        .map_err(|err| failure(&profile, err))?
        .unwrap_or_else(|| json!({}));
    let object = config
        .as_object_mut()
        .ok_or_else(|| failure(&profile, "session config is not an object"))?;
    object.insert("oauthAccount".into(), file.oauth_account.0);
    object.insert("hasCompletedOnboarding".into(), json!(true));
    object.entry("theme").or_insert_with(|| json!("dark"));
    write_json_private(&config_path, &config).map_err(|err| failure(&profile, err))?;
    Ok(profile)
}

pub(crate) fn mark_launch(profile: &Path) -> Result<PathBuf> {
    let marker = profile
        .join(".ccsw-launches")
        .join(format!("{}.json", std::process::id()));
    write_json_private(&marker, &json!({"pid": std::process::id()}))
        .map_err(|err| failure(profile, err))?;
    Ok(marker)
}

/// Retain rotated Keychain credentials in the file before moving or removing a
/// profile. The caller has already checked ownership and holds the store lock.
pub(crate) fn materialize(store: &Store, profile: &Path, cli: &dyn SecurityCli) -> Result<()> {
    if let Some(current) = read(store, profile, cli)? {
        write_json_private(&profile.join(".credentials.json"), &current.credential.0)
            .map_err(|err| failure(profile, err))?;
    }
    if store.paths.keychain_enabled && profile.exists() {
        Keychain::new(cli)
            .delete_password(
                &keychain_service(&profile.to_string_lossy()),
                &account_name(),
            )
            .map_err(|err| failure(profile, err))?;
    }
    Ok(())
}

/// Port of cswap's _prepare_history_share / _merge_history_into_source.
/// Merge before linking so enabling shared history keeps existing transcripts.
pub(crate) fn prepare_history_share(source: &Path, destination: &Path) -> std::io::Result<()> {
    if destination.exists() && !destination.is_symlink() {
        merge_history(source, destination)?;
    }
    if !source.exists() {
        private_directory(source.parent().expect("history has a parent"))?;
        if source.extension().is_some_and(|ext| ext == "jsonl") {
            crate::fsutil::atomic_write_private(source, b"")?;
        } else {
            private_directory(source)?;
        }
    }
    Ok(())
}

fn private_directory(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            private_directory(parent)?;
        }
        fs::create_dir(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn merge_history(source: &Path, destination: &Path) -> std::io::Result<()> {
    if destination.is_dir() {
        private_directory(source)?;
        let mut entries = fs::read_dir(destination)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let target = source.join(entry.file_name());
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                merge_history(&target, &path)?;
            } else if target.exists() {
                // Transcript names are UUIDs: upstream keeps the first copy.
                fs::remove_file(path)?;
            } else if fs::rename(&path, &target).is_err() {
                fs::copy(&path, &target)?;
                fs::remove_file(path)?;
            }
        }
        fs::remove_dir(destination)
    } else {
        use std::collections::HashSet;
        use std::io::Write;
        let existing = match fs::read_to_string(source) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err),
        };
        let mut known: HashSet<_> = existing.lines().map(str::to_string).collect();
        let incoming = fs::read_to_string(destination)?;
        let added: Vec<_> = incoming
            .lines()
            .filter(|line| !line.is_empty() && known.insert((*line).to_string()))
            .collect();
        if !added.is_empty() {
            private_directory(source.parent().expect("history has a parent"))?;
            let mut options = fs::OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(source)?;
            if !existing.is_empty() && !existing.ends_with('\n') {
                writeln!(file)?;
            }
            for line in added {
                writeln!(file, "{line}")?;
            }
            file.sync_all()?;
        }
        fs::remove_file(destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::keychain::{CliOutput, test_support::FakeSecurity};
    use crate::store::temp_store;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct MemoryKeychain(RefCell<BTreeMap<String, String>>);

    impl SecurityCli for MemoryKeychain {
        fn run(&self, args: &[String], _: Option<&str>) -> std::io::Result<CliOutput> {
            let service = args.windows(2).find(|a| a[0] == "-s").unwrap()[1].clone();
            let mut items = self.0.borrow_mut();
            let value = if args[0] == "delete-generic-password" {
                items.remove(&service)
            } else {
                items.get(&service).cloned()
            };
            Ok(CliOutput {
                status: if value.is_some() { 0 } else { 44 },
                stdout: value.unwrap_or_default(),
            })
        }
    }

    fn seed(store: &Store) -> (AccountRecord, PathBuf) {
        let mut record = AccountRecord::new("session@example.com");
        record.provider = Provider::Claude;
        record.organization_uuid = "org".into();
        let credential = ClaudeCredential::from_value(
            json!({"claudeAiOauth": {"accessToken": "old", "refreshToken": "old-rt", "expiresAt": 1000}}),
        );
        let account =
            OauthAccount(json!({"emailAddress": "session@example.com", "organizationUuid": "org"}));
        credentials::write(
            store,
            2,
            &SlotFile::new(&credential, account.clone()).to_value(),
        )
        .unwrap();
        let profile = store.paths.session_dir(2, &record.email);
        write_json_private(&profile.join(".credentials.json"), &credential.0).unwrap();
        write_json_private(
            &profile.join(".claude.json"),
            &json!({"oauthAccount": account.0}),
        )
        .unwrap();
        (record, profile)
    }

    #[test]
    fn reuses_keychain_rotation_instead_of_reseeding_a_valid_profile() {
        let (_dir, mut store) = temp_store();
        store.paths.keychain_enabled = true;
        let (record, profile) = seed(&store);
        let service = keychain_service(&profile.to_string_lossy());
        let fresh = json!({"claudeAiOauth": {"accessToken": "new", "refreshToken": "new-rt", "expiresAt": 2000}}).to_string();
        let cli = MemoryKeychain::default();
        cli.0.borrow_mut().insert(service.clone(), fresh.clone());
        prepare(&store, 2, &record, &cli).unwrap();
        assert_eq!(
            credentials::read(&store, 2).unwrap().unwrap()["claudeAiOauth"]["refreshToken"],
            "new-rt"
        );
        assert_eq!(cli.0.borrow().get(&service), Some(&fresh));
    }

    #[test]
    fn locked_profile_keychain_does_not_fall_back_to_the_seed() {
        let (_dir, mut store) = temp_store();
        store.paths.keychain_enabled = true;
        let (record, profile) = seed(&store);
        let before = fs::read(profile.join(".credentials.json")).unwrap();
        assert!(prepare(&store, 2, &record, &FakeSecurity { failing: true }).is_err());
        assert_eq!(fs::read(profile.join(".credentials.json")).unwrap(), before);
    }

    #[test]
    fn unreadable_session_records_are_not_treated_as_exited() {
        let (_dir, store) = temp_store();
        let (_, profile) = seed(&store);
        ensure_private_dir(&profile.join("sessions")).unwrap();
        fs::write(profile.join("sessions/broken.json"), "not json").unwrap();
        assert!(!is_quiescent(&profile));
        write_json_private(&profile.join("sessions/broken.json"), &json!({"pid": "42"})).unwrap();
        assert!(!is_quiescent(&profile));
    }

    #[test]
    fn service_hash_uses_nfc_but_keeps_path_spelling() {
        assert_eq!(
            keychain_service("/tmp/cafe\u{301}/"),
            keychain_service("/tmp/café/")
        );
        assert_ne!(
            keychain_service("/tmp/café/"),
            keychain_service("/tmp/café")
        );
        assert_ne!(
            keychain_service("/tmp/./profile"),
            keychain_service("/tmp/profile")
        );
    }
}
