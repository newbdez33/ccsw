//! `autoswitch_state.json`: cooldown and quarantine state of the auto-switch engine.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{CswitchError, Result};
use crate::fsutil::{read_json, write_json_private};
use crate::paths::Paths;

use super::Store;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSwitchState {
    /// Wall-clock epoch seconds of the last real engine switch; drives the cooldown.
    #[serde(default)]
    pub last_switch_at: Option<f64>,
    /// Landing slot of the last engine switch, as a string.
    #[serde(default)]
    pub last_switch_to: Option<String>,
    /// Slot the engine left.
    #[serde(default)]
    pub last_switch_from: Option<u32>,
    #[serde(default)]
    pub quarantine: BTreeMap<String, QuarantineEntry>,
    /// Unknown keys (and `schemaVersion`) survive a read/write round trip.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineEntry {
    pub email: String,
    /// `invalid_grant` or `identity-conflict`.
    pub reason: String,
    /// ISO seconds, `Z` suffix.
    pub at: String,
    #[serde(default)]
    pub refresh_token_fingerprint: Option<String>,
}

/// Read the state; any error (missing, corrupt, wrong shape) yields the default.
pub fn read(paths: &Paths) -> AutoSwitchState {
    read_json(&paths.state_file())
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// Write the state atomically with `schemaVersion` 1.
pub fn write(paths: &Paths, state: &AutoSwitchState) -> Result<()> {
    let path = paths.state_file();
    let mut value = serde_json::to_value(state)
        .map_err(|err| CswitchError::config(format!("{}: {err}", path.display())))?;
    value["schemaVersion"] = Value::from(1);
    write_json_private(&path, &value)
        .map_err(|err| CswitchError::config(format!("{}: {err}", path.display())))
}

/// Read-modify-write under the state lock.
pub fn modify<T>(store: &Store, f: impl FnOnce(&mut AutoSwitchState) -> T) -> Result<T> {
    let _lock = store.lock_state()?;
    let mut state = read(&store.paths);
    let result = f(&mut state);
    write(&store.paths, &state)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::temp_store;
    use serde_json::json;

    #[test]
    fn missing_or_corrupt_state_reads_as_default() {
        let (_dir, store) = temp_store();
        assert_eq!(read(&store.paths), AutoSwitchState::default());
        std::fs::create_dir_all(&store.paths.backup_root).unwrap();
        std::fs::write(store.paths.state_file(), "{oops").unwrap();
        assert_eq!(read(&store.paths), AutoSwitchState::default());
        std::fs::write(store.paths.state_file(), "[1]").unwrap();
        assert_eq!(read(&store.paths), AutoSwitchState::default());
    }

    #[test]
    fn write_sets_schema_version_and_unknown_keys_round_trip() {
        let (_dir, store) = temp_store();
        std::fs::create_dir_all(&store.paths.backup_root).unwrap();
        std::fs::write(
            store.paths.state_file(),
            json!({
                "schemaVersion": 1,
                "lastSwitchAt": 1752000000.5,
                "lastSwitchTo": "3",
                "lastSwitchFrom": 1,
                "leftHeadroom": 4.0,
                "quarantine": {"2": {"email": "b@x.com", "reason": "invalid_grant",
                                      "at": "2026-07-17T12:00:00Z", "refreshTokenFingerprint": null}}
            })
            .to_string(),
        )
        .unwrap();
        let state = read(&store.paths);
        assert_eq!(state.last_switch_at, Some(1752000000.5));
        assert_eq!(state.last_switch_to.as_deref(), Some("3"));
        assert_eq!(state.last_switch_from, Some(1));
        assert_eq!(state.quarantine["2"].reason, "invalid_grant");
        assert_eq!(state.quarantine["2"].refresh_token_fingerprint, None);
        assert_eq!(state.extra["leftHeadroom"], json!(4.0));

        write(&store.paths, &state).unwrap();
        let raw = read_json(&store.paths.state_file()).unwrap().unwrap();
        assert_eq!(raw["schemaVersion"], 1);
        assert_eq!(raw["leftHeadroom"], 4.0);
        assert_eq!(raw["quarantine"]["2"]["email"], "b@x.com");
        assert_eq!(read(&store.paths), state);
    }

    #[test]
    fn modify_runs_under_the_state_lock_and_persists() {
        let (_dir, store) = temp_store();
        let count = modify(&store, |state| {
            state.quarantine.insert(
                "4".into(),
                QuarantineEntry {
                    email: "d@x.com".into(),
                    reason: "invalid_grant".into(),
                    at: "2026-09-29T00:00:00Z".into(),
                    refresh_token_fingerprint: Some("sha256:abc".into()),
                },
            );
            state.last_switch_at = Some(10.0);
            state.quarantine.len()
        })
        .unwrap();
        assert_eq!(count, 1);
        let state = read(&store.paths);
        assert_eq!(state.last_switch_at, Some(10.0));
        assert_eq!(
            state.quarantine["4"].refresh_token_fingerprint.as_deref(),
            Some("sha256:abc")
        );
        assert!(store.paths.state_lock_file().exists());
        let raw = read_json(&store.paths.state_file()).unwrap().unwrap();
        assert_eq!(raw["schemaVersion"], 1);

        // A held state lock makes a second acquisition time out instead of racing.
        let held = store.lock_state().unwrap();
        let err = crate::fsutil::FileLock::acquire(
            &store.paths.state_lock_file(),
            std::time::Duration::from_millis(150),
        )
        .unwrap_err();
        assert_eq!(err.type_name(), "LockError");
        drop(held);
    }
}
