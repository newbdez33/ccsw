//! The backup store: roster, credentials, settings, mappings, usage cache, auto-switch state.

pub mod credentials;
pub mod mappings;
pub mod poll_policy;
pub mod roster;
pub mod settings;
pub mod state;
pub mod usage_store;

use std::fs;
use std::path::Path;

use crate::errors::{CswitchError, Result};
use crate::fsutil::{FileLock, io_error};
use crate::paths::Paths;

pub use mappings::{MappingEntry, MappingStore};
pub use roster::{MoveOutcome, alias_owner, normalize_alias, resolve_identifier, resolve_slot};
pub use settings::{
    AutoSwitchOverrides, AutoSwitchSettings, SETTING_SPECS, SettingKind, SettingSpec, Settings,
    format_setting_value, parse_model_names,
};
pub use state::{AutoSwitchState, QuarantineEntry};

/// The backup store rooted at `paths.backup_root`. Directories are created lazily by
/// writers (`ensure_dirs`), never here, so a read-only command leaves no trace.
#[derive(Debug, Clone)]
pub struct Store {
    pub paths: Paths,
}

impl Store {
    pub fn open(paths: Paths) -> Self {
        Self { paths }
    }

    pub fn from_env() -> Result<Self> {
        Ok(Self::open(Paths::from_env()?))
    }

    /// The store lock (`.lock`, 10 s): roster and credential mutations, the switch body.
    pub fn lock(&self) -> Result<FileLock> {
        FileLock::acquire(&self.paths.lock_file(), FileLock::DEFAULT_TIMEOUT)
    }

    /// The auto-switch state lock (`.autoswitch_state.lock`, 10 s).
    pub fn lock_state(&self) -> Result<FileLock> {
        FileLock::acquire(&self.paths.state_lock_file(), FileLock::DEFAULT_TIMEOUT)
    }

    /// Create the backup root, `credentials/` and `cache/` as private (0700) directories.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            self.paths.backup_root.clone(),
            self.paths.credentials_dir(),
            self.paths.cache_dir(),
        ] {
            ensure_private_dir(&dir)?;
        }
        Ok(())
    }
}

/// `mkdir -p` plus `chmod 0700` on the leaf.
pub(crate) fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(|err| io_error(CswitchError::Config, dir, &err))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|err| io_error(CswitchError::Config, dir, &err))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn temp_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_values(
        Some(dir.path().join("store")),
        Some(dir.path().join("codex")),
        dir.path(),
    )
    .unwrap();
    (dir, Store::open(paths))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_nothing_until_ensure_dirs() {
        let (_dir, store) = temp_store();
        assert!(!store.paths.backup_root.exists());
        store.ensure_dirs().unwrap();
        assert!(store.paths.credentials_dir().is_dir());
        assert!(store.paths.cache_dir().is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [
                store.paths.backup_root.clone(),
                store.paths.credentials_dir(),
                store.paths.cache_dir(),
            ] {
                assert_eq!(
                    fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
    }

    #[test]
    fn locks_use_the_documented_files() {
        let (_dir, store) = temp_store();
        let lock = store.lock().unwrap();
        assert_eq!(lock.path(), store.paths.lock_file());
        let state = store.lock_state().unwrap();
        assert_eq!(state.path(), store.paths.state_lock_file());
    }
}
