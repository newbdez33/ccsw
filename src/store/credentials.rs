//! Per-slot credential snapshots under `credentials/`: the whole Codex `auth.json`
//! object, stored as `<slot>.json` with one retained previous generation.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::errors::{CswitchError, Result};
use crate::fsutil::{io_error, write_json_private};

use super::Store;

/// Read the stored snapshot; `Ok(None)` when the slot has none.
pub fn read(store: &Store, slot: u32) -> Result<Option<Value>> {
    let path = store.paths.credential_file(slot);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error(CswitchError::CredentialRead, &path, &err)),
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|err| {
        CswitchError::credential_read(format!(
            "{} exists but could not be parsed ({err})",
            path.display()
        ))
    })?;
    if !value.is_object() {
        return Err(CswitchError::credential_read(format!(
            "{} does not hold a JSON object",
            path.display()
        )));
    }
    Ok(Some(value))
}

/// Atomically store the snapshot (0600). When a different value is already stored,
/// the old file is kept as `<slot>.json.prev` first; a same-value rewrite leaves the
/// existing `.prev` alone.
pub fn write(store: &Store, slot: u32, value: &Value) -> Result<()> {
    store.ensure_dirs()?;
    let path = store.paths.credential_file(slot);
    let previous = read(store, slot).ok().flatten();
    if let Some(old) = previous
        && old != *value
    {
        let prev_path = store.paths.credential_prev_file(slot);
        copy_private(&path, &prev_path)
            .map_err(|err| io_error(CswitchError::CredentialWrite, &prev_path, &err))?;
    }
    write_json_private(&path, value)
        .map_err(|err| io_error(CswitchError::CredentialWrite, &path, &err))
}

fn copy_private(from: &Path, to: &Path) -> io::Result<()> {
    fs::copy(from, to)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(to, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Remove the snapshot and its `.prev`; missing files are not an error.
pub fn delete(store: &Store, slot: u32) -> Result<()> {
    for path in [
        store.paths.credential_file(slot),
        store.paths.credential_prev_file(slot),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_error(CswitchError::CredentialWrite, &path, &err)),
        }
    }
    Ok(())
}

pub fn exists(store: &Store, slot: u32) -> bool {
    store.paths.credential_file(slot).is_file()
}

/// Rotation-stable identity of a credential generation: the SHA-256 of the refresh
/// token when present, else of the canonical compact JSON. `None` for `null`.
pub fn fingerprint(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if let Some(token) = value
        .pointer("/tokens/refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
    {
        return Some(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(token.as_bytes()))
        ));
    }
    let canonical = serde_json::to_string(value).ok()?;
    Some(format!(
        "sha256-full:{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    ))
}

/// `fingerprint` of the stored snapshot; `None` when absent or unreadable.
pub fn slot_fingerprint(store: &Store, slot: u32) -> Option<String> {
    read(store, slot)
        .ok()
        .flatten()
        .as_ref()
        .and_then(fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::temp_store;
    use serde_json::json;

    fn chatgpt(refresh: &str) -> Value {
        json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "tokens": {"id_token": "id", "access_token": "at", "refresh_token": refresh, "account_id": "acct"},
            "last_refresh": "2026-09-29T00:00:00Z"
        })
    }

    #[test]
    fn write_read_delete_round_trip() {
        let (_dir, store) = temp_store();
        assert!(!exists(&store, 1));
        assert_eq!(read(&store, 1).unwrap(), None);
        let value = chatgpt("rt-1");
        write(&store, 1, &value).unwrap();
        assert!(exists(&store, 1));
        assert_eq!(read(&store, 1).unwrap(), Some(value));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.paths.credential_file(1))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let root = fs::metadata(&store.paths.backup_root)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(root & 0o777, 0o700);
        }
        delete(&store, 1).unwrap();
        assert!(!exists(&store, 1));
        delete(&store, 1).unwrap();
    }

    #[test]
    fn prev_generation_kept_only_when_the_value_changes() {
        let (_dir, store) = temp_store();
        let prev = store.paths.credential_prev_file(2);
        write(&store, 2, &chatgpt("rt-1")).unwrap();
        assert!(!prev.exists());
        write(&store, 2, &chatgpt("rt-1")).unwrap();
        assert!(!prev.exists(), "same value keeps no .prev");
        write(&store, 2, &chatgpt("rt-2")).unwrap();
        let kept: Value = serde_json::from_slice(&fs::read(&prev).unwrap()).unwrap();
        assert_eq!(kept["tokens"]["refresh_token"], "rt-1");
        write(&store, 2, &chatgpt("rt-2")).unwrap();
        let kept: Value = serde_json::from_slice(&fs::read(&prev).unwrap()).unwrap();
        assert_eq!(
            kept["tokens"]["refresh_token"], "rt-1",
            "same-value rewrite leaves .prev alone"
        );
        delete(&store, 2).unwrap();
        assert!(!prev.exists());
    }

    #[test]
    fn corrupt_snapshot_names_the_file() {
        let (_dir, store) = temp_store();
        let path = store.paths.credential_file(3);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "garbage").unwrap();
        let err = read(&store, 3).unwrap_err();
        assert_eq!(err.type_name(), "CredentialReadError");
        assert!(err.to_string().starts_with(&path.display().to_string()));
        fs::write(&path, "\"sk-just-a-string\"").unwrap();
        assert_eq!(
            read(&store, 3).unwrap_err().type_name(),
            "CredentialReadError"
        );
        assert!(exists(&store, 3));
        assert_eq!(slot_fingerprint(&store, 3), None);
    }

    #[test]
    fn fingerprint_prefers_the_refresh_token() {
        let expected = hex::encode(Sha256::digest(b"rt-1"));
        assert_eq!(
            fingerprint(&chatgpt("rt-1")),
            Some(format!("sha256:{expected}"))
        );
        let api_key = json!({"auth_mode": "apikey", "OPENAI_API_KEY": "sk-x"});
        let canonical = serde_json::to_string(&api_key).unwrap();
        let expected = hex::encode(Sha256::digest(canonical.as_bytes()));
        assert_eq!(
            fingerprint(&api_key),
            Some(format!("sha256-full:{expected}"))
        );
        assert!(
            fingerprint(&chatgpt(""))
                .unwrap()
                .starts_with("sha256-full:")
        );
        assert_eq!(fingerprint(&Value::Null), None);
        let (_dir, store) = temp_store();
        write(&store, 1, &chatgpt("rt-1")).unwrap();
        assert_eq!(slot_fingerprint(&store, 1), fingerprint(&chatgpt("rt-1")));
        assert_eq!(slot_fingerprint(&store, 2), None);
    }
}
