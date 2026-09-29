//! Atomic private file writes, JSON helpers, and cross-process file locks.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs4::{FileExt, TryLockError};
use serde_json::Value;

use crate::errors::{CswitchError, Result};

/// Write `contents` to `path` atomically: temp file in the same directory, fsync,
/// rename. The file becomes 0600; with `private_parent` the directory becomes 0700.
pub fn atomic_write(path: &Path, contents: &[u8], private_parent: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    if private_parent {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o600))?;
    }
    tmp.write_all(contents)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|err| err.error)?;
    Ok(())
}

/// Atomic 0600 write inside a 0700 directory (everything under the backup store
/// and `$CODEX_HOME`).
pub fn atomic_write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    atomic_write(path, contents, true)
}

/// Read a JSON file; `Ok(None)` when it does not exist.
pub fn read_json(path: &Path) -> io::Result<Option<Value>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Pretty JSON (2-space indent) + trailing newline, re-parsed before the rename
/// so a serialization bug never replaces a good file with garbage.
pub fn write_json_private(path: &Path, value: &Value) -> io::Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    serde_json::from_str::<Value>(&text)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    atomic_write_private(path, text.as_bytes())
}

/// Map an I/O failure on `path` to a domain error with the path in the message.
pub fn io_error(kind: fn(String) -> CswitchError, path: &Path, err: &io::Error) -> CswitchError {
    kind(format!("{}: {err}", path.display()))
}

/// Exclusive advisory lock on a file, released on drop. The lock file is created
/// on demand, never deleted, and records its holder for diagnostics.
pub struct FileLock {
    file: File,
    path: PathBuf,
}

impl FileLock {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
    const POLL: Duration = Duration::from_millis(100);

    pub fn acquire(path: &Path, timeout: Duration) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| io_error(CswitchError::Lock, parent, &err))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|err| io_error(CswitchError::Lock, path, &err))?;
        let started = Instant::now();
        loop {
            match <File as FileExt>::try_lock(&file) {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(err)) => {
                    return Err(io_error(CswitchError::Lock, path, &err));
                }
            }
            if started.elapsed() >= timeout {
                return Err(CswitchError::lock(
                    "Failed to acquire lock - another instance may be running",
                ));
            }
            std::thread::sleep(Self::POLL);
        }
        let mut lock = Self {
            file,
            path: path.to_path_buf(),
        };
        lock.record_holder();
        Ok(lock)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn record_holder(&mut self) {
        let _ = self.file.set_len(0);
        let _ = writeln!(
            self.file,
            "{} {}",
            std::process::id(),
            crate::model::now_unix()
        );
        let _ = self.file.flush();
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = <File as FileExt>::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store").join("file.json");
        atomic_write_private(&path, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        atomic_write_private(&path, b"{\"a\":2}").unwrap();
        assert_eq!(read_json(&path).unwrap().unwrap()["a"], 2);
        assert!(
            read_json(&dir.path().join("missing.json"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn write_json_is_pretty_with_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        write_json_private(&path, &serde_json::json!({"b": [1, 2]})).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\n  \"b\": [\n    1,\n    2\n  ]\n}\n"
        );
    }

    #[test]
    fn second_lock_times_out_while_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        let first = FileLock::acquire(&path, Duration::from_millis(200)).unwrap();
        let err = FileLock::acquire(&path, Duration::from_millis(250)).unwrap_err();
        assert_eq!(err.type_name(), "LockError");
        drop(first);
        FileLock::acquire(&path, Duration::from_millis(200)).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains(&std::process::id().to_string())
        );
    }
}
