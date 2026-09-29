//! `auth.json` model, backups, identity, freshness rule.
//!
//! The file is kept as a raw JSON object so keys cswitch does not know about
//! survive a round trip; Codex owns the format.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::errors::{CswitchError, Result};
use crate::fsutil;
use crate::model::{Identity, now_iso, parse_iso};

use super::jwt::AccountInfo;

/// Live backups retained next to `auth.json`.
const MAX_BACKUPS: usize = 3;

/// The contents of an `auth.json`: a ChatGPT OAuth login, an API key, or
/// something cswitch does not recognize.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthJson(pub Value);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    ChatGpt,
    ApiKey,
    Unknown,
}

impl AuthJson {
    pub fn from_value(value: Value) -> Self {
        Self(value)
    }

    /// `{"auth_mode":"apikey","OPENAI_API_KEY":"<key>"}`, the shape Codex
    /// writes for an API-key login.
    pub fn api_key_auth(key: &str) -> Self {
        Self(json!({ "auth_mode": "apikey", "OPENAI_API_KEY": key }))
    }

    /// Parse the text of an `auth.json`; it must be a JSON object.
    pub fn parse(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text)
            .map_err(|err| CswitchError::credential_read(format!("invalid JSON: {err}")))?;
        if !value.is_object() {
            return Err(CswitchError::credential_read("not a JSON object"));
        }
        Ok(Self(value))
    }

    /// `Ok(None)` when the file does not exist; unreadable or unparseable
    /// files are a `CredentialReadError` naming the path.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(fsutil::io_error(CswitchError::CredentialRead, path, &err));
            }
        };
        Self::parse(&text)
            .map(Some)
            .map_err(|err| CswitchError::credential_read(format!("{}: {err}", path.display())))
    }

    /// Pretty JSON, written atomically with mode 0600.
    pub fn write(&self, path: &Path) -> Result<()> {
        fsutil::write_json_private(path, &self.0)
            .map_err(|err| fsutil::io_error(CswitchError::CredentialWrite, path, &err))
    }

    pub fn kind(&self) -> AuthKind {
        if self.id_token().is_some() {
            AuthKind::ChatGpt
        } else if self.api_key().is_some() {
            AuthKind::ApiKey
        } else {
            AuthKind::Unknown
        }
    }

    /// `OPENAI_API_KEY` when it is a non-empty string.
    pub fn api_key(&self) -> Option<&str> {
        self.0
            .get("OPENAI_API_KEY")
            .and_then(Value::as_str)
            .filter(|key| !key.trim().is_empty())
    }

    pub fn id_token(&self) -> Option<&str> {
        self.token("id_token")
    }

    pub fn access_token(&self) -> Option<&str> {
        self.token("access_token")
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.token("refresh_token")
    }

    fn token(&self, name: &str) -> Option<&str> {
        self.0
            .pointer(&format!("/tokens/{name}"))
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
    }

    /// Root `last_refresh` as unix seconds; `None` when absent or malformed.
    pub fn last_refresh(&self) -> Option<i64> {
        self.0
            .get("last_refresh")
            .and_then(Value::as_str)
            .and_then(parse_iso)
    }

    pub fn account_info(&self) -> AccountInfo {
        AccountInfo::from_auth(self)
    }

    /// `(email, account_id)` of a ChatGPT login, email lowercased. API keys
    /// carry no identity of their own, and a login missing either claim is
    /// not identifiable.
    pub fn identity(&self) -> Option<Identity> {
        if self.kind() != AuthKind::ChatGpt {
            return None;
        }
        let info = self.account_info();
        Some(Identity::new(info.email?.to_lowercase(), info.account_id?))
    }

    /// Replace the three token fields in place and stamp `last_refresh`.
    /// Codex refreshes proactively when `last_refresh` is older than 8 days,
    /// so the stamp keeps cswitch's refreshes recognized.
    pub fn apply_tokens(&mut self, id_token: &str, access_token: &str, refresh_token: &str) {
        if !self.0.is_object() {
            self.0 = json!({});
        }
        let root = self.0.as_object_mut().expect("auth.json root is an object");
        let tokens = root.entry("tokens").or_insert_with(|| json!({}));
        if !tokens.is_object() {
            *tokens = json!({});
        }
        let tokens = tokens.as_object_mut().expect("tokens is an object");
        tokens.insert("id_token".into(), json!(id_token));
        tokens.insert("access_token".into(), json!(access_token));
        tokens.insert("refresh_token".into(), json!(refresh_token));
        root.insert("last_refresh".into(), json!(now_iso()));
    }

    /// The freshness rule for folding a live or session copy back into the
    /// store. The refresh token is single-use, so of two different tokens
    /// exactly one is alive; `last_refresh` is only weak evidence, so an equal
    /// or missing stamp is not proof of being newer. API keys are newer when
    /// the key differs.
    pub fn is_newer_than(&self, other: &AuthJson) -> bool {
        if self.kind() == AuthKind::ApiKey || other.kind() == AuthKind::ApiKey {
            return self.kind() == AuthKind::ApiKey && self.api_key() != other.api_key();
        }
        if self.refresh_token() == other.refresh_token() {
            return false;
        }
        match (self.last_refresh(), other.last_refresh()) {
            (Some(mine), Some(theirs)) => mine > theirs,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// SHA-256 of the bytes `write` produces, so it equals the hash of a file
    /// cswitch wrote from this value.
    pub fn sha256_hex(&self) -> String {
        let mut text = serde_json::to_string_pretty(&self.0).unwrap_or_default();
        text.push('\n');
        hex::encode(Sha256::digest(text.as_bytes()))
    }
}

/// SHA-256 (hex) of a file's bytes; `None` when it cannot be read.
pub fn sha256_file(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    Some(hex::encode(Sha256::digest(&data)))
}

/// Copy the live file to `auth.json.bak.<unix_nanos>` next to it (mode 0600)
/// and keep the newest three backups. A missing file needs no backup.
pub fn backup_live(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let write_err = |err: &io::Error| fsutil::io_error(CswitchError::CredentialWrite, path, err);
    let contents = std::fs::read(path).map_err(|err| write_err(&err))?;
    let backup = allocate_backup_path(path)?;
    fsutil::atomic_write_private(&backup, &contents).map_err(|err| write_err(&err))?;
    cleanup_old_backups(path);
    Ok(())
}

/// Nanoseconds rather than seconds so two switches inside one second keep
/// both recovery points; `-N` on the (theoretical) collision.
fn allocate_backup_path(path: &Path) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CswitchError::credential_write("system clock is before the Unix epoch"))?
        .as_nanos();
    backup_path_for(path, nanos).ok_or_else(|| {
        CswitchError::credential_write(format!(
            "could not allocate a unique backup path for {}",
            path.display()
        ))
    })
}

fn backup_path_for(path: &Path, nanos: u128) -> Option<PathBuf> {
    (0..1000u16)
        .map(|collision| {
            if collision == 0 {
                path.with_extension(format!("json.bak.{nanos}"))
            } else {
                path.with_extension(format!("json.bak.{nanos}-{collision}"))
            }
        })
        .find(|candidate| !candidate.exists())
}

/// Delete all but the newest `MAX_BACKUPS` siblings named `<file>.bak.*`,
/// best-effort. Names sort by their stamp, so lexicographic order is age order.
fn cleanup_old_backups(path: &Path) {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|f| f.to_str()))
    else {
        return;
    };
    let prefix = format!("{name}.bak.");
    let mut backups: Vec<PathBuf> = std::fs::read_dir(parent)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .map(|entry| entry.path())
        .collect();
    if backups.len() <= MAX_BACKUPS {
        return;
    }
    backups.sort();
    for old in &backups[..backups.len() - MAX_BACKUPS] {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::jwt::make_jwt;
    use serde_json::json;

    fn chatgpt_auth(
        email: &str,
        account_id: &str,
        refresh: &str,
        last_refresh: Option<&str>,
    ) -> AuthJson {
        let id_token = make_jwt(&json!({
            "email": email,
            "https://api.openai.com/auth": {"chatgpt_account_id": account_id, "chatgpt_plan_type": "plus"}
        }));
        let mut value = json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "tokens": {"id_token": id_token, "access_token": "at", "refresh_token": refresh, "account_id": account_id},
            "vendor_extra": {"keep": true}
        });
        if let Some(stamp) = last_refresh {
            value["last_refresh"] = json!(stamp);
        }
        AuthJson(value)
    }

    #[test]
    fn kinds_and_accessors() {
        let auth = chatgpt_auth(
            "A@Example.com",
            "acct-1",
            "rt-1",
            Some("2026-09-29T10:00:00Z"),
        );
        assert_eq!(auth.kind(), AuthKind::ChatGpt);
        assert_eq!(auth.access_token(), Some("at"));
        assert_eq!(auth.refresh_token(), Some("rt-1"));
        assert!(auth.id_token().is_some());
        assert_eq!(auth.api_key(), None);
        assert_eq!(auth.last_refresh(), parse_iso("2026-09-29T10:00:00Z"));
        assert_eq!(
            auth.identity(),
            Some(Identity::new("a@example.com", "acct-1"))
        );

        let key = AuthJson::api_key_auth("sk-test");
        assert_eq!(key.kind(), AuthKind::ApiKey);
        assert_eq!(key.api_key(), Some("sk-test"));
        assert_eq!(key.identity(), None);
        assert_eq!(key.last_refresh(), None);
        assert_eq!(
            key.0,
            json!({"auth_mode": "apikey", "OPENAI_API_KEY": "sk-test"})
        );

        assert_eq!(AuthJson(json!({})).kind(), AuthKind::Unknown);
        assert_eq!(
            AuthJson(json!({"OPENAI_API_KEY": "  "})).kind(),
            AuthKind::Unknown
        );
        assert_eq!(
            AuthJson(json!({"tokens": {"id_token": ""}, "OPENAI_API_KEY": "sk"})).kind(),
            AuthKind::ApiKey
        );
    }

    #[test]
    fn identity_needs_both_claims() {
        let no_account =
            AuthJson(json!({"tokens": {"id_token": make_jwt(&json!({"email": "a@b.c"}))}}));
        assert_eq!(no_account.identity(), None);
        let no_email = AuthJson(json!({"tokens": {
            "id_token": make_jwt(&json!({"https://api.openai.com/auth": {"chatgpt_account_id": "x"}}))
        }}));
        assert_eq!(no_email.identity(), None);
    }

    #[test]
    fn parse_rejects_garbage_and_non_objects() {
        assert!(AuthJson::parse("{").is_err());
        let err = AuthJson::parse("[1]").unwrap_err();
        assert_eq!(err.type_name(), "CredentialReadError");
        assert_eq!(
            AuthJson::parse("{\"a\":1}").unwrap(),
            AuthJson(json!({"a": 1}))
        );
    }

    #[test]
    fn read_write_round_trip_preserves_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex").join("auth.json");
        assert_eq!(AuthJson::read(&path).unwrap(), None);

        let auth = chatgpt_auth("a@b.c", "acct", "rt", None);
        auth.write(&path).unwrap();
        let back = AuthJson::read(&path).unwrap().unwrap();
        assert_eq!(back, auth);
        assert_eq!(back.0["vendor_extra"]["keep"], true);
        assert_eq!(sha256_file(&path), Some(auth.sha256_hex()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        std::fs::write(&path, "not json").unwrap();
        let err = AuthJson::read(&path).unwrap_err();
        assert_eq!(err.type_name(), "CredentialReadError");
        assert!(err.to_string().contains("auth.json"), "{err}");
    }

    #[test]
    fn apply_tokens_replaces_in_place_and_stamps_last_refresh() {
        let mut auth = chatgpt_auth("a@b.c", "acct", "rt-old", Some("2020-01-01T00:00:00Z"));
        let before = crate::model::now_unix();
        auth.apply_tokens("id-new", "at-new", "rt-new");
        assert_eq!(auth.id_token(), Some("id-new"));
        assert_eq!(auth.access_token(), Some("at-new"));
        assert_eq!(auth.refresh_token(), Some("rt-new"));
        assert_eq!(auth.0["tokens"]["account_id"], "acct");
        assert_eq!(auth.0["auth_mode"], "chatgpt");
        let stamp = auth.0["last_refresh"].as_str().unwrap().to_string();
        assert!(stamp.ends_with('Z') && stamp.len() == 20, "{stamp}");
        assert!(auth.last_refresh().unwrap() >= before);

        let mut bare = AuthJson(json!({"tokens": null}));
        bare.apply_tokens("i", "a", "r");
        assert_eq!(bare.refresh_token(), Some("r"));
    }

    #[test]
    fn freshness_rule() {
        let stored = chatgpt_auth("a@b.c", "acct", "rt-1", Some("2026-09-29T10:00:00Z"));
        let same_token = chatgpt_auth("a@b.c", "acct", "rt-1", Some("2026-09-29T11:00:00Z"));
        assert!(!same_token.is_newer_than(&stored), "same refresh token");

        let newer = chatgpt_auth("a@b.c", "acct", "rt-2", Some("2026-09-29T10:00:01Z"));
        assert!(newer.is_newer_than(&stored));
        assert!(!stored.is_newer_than(&newer));

        let equal = chatgpt_auth("a@b.c", "acct", "rt-2", Some("2026-09-29T10:00:00Z"));
        assert!(!equal.is_newer_than(&stored), "equal stamps are a conflict");

        let unstamped = chatgpt_auth("a@b.c", "acct", "rt-2", None);
        assert!(
            !unstamped.is_newer_than(&stored),
            "missing stamp is not newer"
        );
        let unstamped_store = chatgpt_auth("a@b.c", "acct", "rt-1", None);
        assert!(
            newer.is_newer_than(&unstamped_store),
            "only the incoming side is stamped"
        );
        assert!(!unstamped.is_newer_than(&unstamped_store));

        let malformed = chatgpt_auth("a@b.c", "acct", "rt-3", Some("yesterday"));
        assert!(!malformed.is_newer_than(&stored));

        let key_a = AuthJson::api_key_auth("sk-a");
        let key_b = AuthJson::api_key_auth("sk-b");
        assert!(key_b.is_newer_than(&key_a));
        assert!(!key_a.is_newer_than(&key_a.clone()));
        assert!(!stored.is_newer_than(&key_a));
    }

    #[test]
    fn backups_rotate_and_keep_three() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("auth.json");
        backup_live(&live).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        for round in 0..5 {
            std::fs::write(&live, format!("{{\"round\":{round}}}")).unwrap();
            backup_live(&live).unwrap();
        }
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("auth.json.bak."))
            .collect();
        names.sort();
        assert_eq!(names.len(), MAX_BACKUPS, "{names:?}");
        let newest = dir.path().join(names.last().unwrap());
        assert_eq!(std::fs::read_to_string(&newest).unwrap(), "{\"round\":4}");
        let oldest = dir.path().join(names.first().unwrap());
        assert_eq!(std::fs::read_to_string(&oldest).unwrap(), "{\"round\":2}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&newest).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(live.exists(), "the live file is never moved");
    }

    #[test]
    fn backup_collision_gets_a_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("auth.json");
        let first = backup_path_for(&live, 123).unwrap();
        assert_eq!(first, dir.path().join("auth.json.bak.123"));
        std::fs::write(&first, "{}").unwrap();
        let second = backup_path_for(&live, 123).unwrap();
        assert_eq!(second, dir.path().join("auth.json.bak.123-1"));
    }
}
