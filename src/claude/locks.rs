//! Claude Code's advisory locks (spec §4): `proper-lockfile` directories whose
//! `mkdir` is the mutex, stale by mtime, touched while held. Holding them
//! around a credential swap means a concurrent Claude Code refresh either
//! finishes first or re-reads and aborts; a crashed holder's lock is taken
//! over once it goes stale.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use std::{fs, io};

use crate::errors::{CswitchError, Result};
use crate::fsutil::io_error;
use crate::paths::Paths;

/// Credential locks (`stale: 60000` in Claude Code).
pub const CREDENTIALS_STALE: Duration = Duration::from_secs(60);
/// The config lock keeps proper-lockfile's default.
pub const CONFIG_STALE: Duration = Duration::from_secs(10);
/// Claude Code touches every 5 s; a little faster for margin.
pub const TOUCH_INTERVAL: Duration = Duration::from_secs(3);
/// Per lock. Claude Code holds a credential lock for one token round trip.
pub const WAIT_BUDGET: Duration = Duration::from_secs(9);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockSpec {
    pub path: PathBuf,
    pub stale: Duration,
}

/// The three locks Claude Code takes around a refresh and a config write, in its order.
pub fn specs(paths: &Paths) -> Vec<LockSpec> {
    vec![
        LockSpec {
            path: paths.claude_refresh_lock_dir(),
            stale: CREDENTIALS_STALE,
        },
        LockSpec {
            path: paths.claude_legacy_lock_dir(),
            stale: CREDENTIALS_STALE,
        },
        LockSpec {
            path: paths.claude_config_lock_dir(),
            stale: CONFIG_STALE,
        },
    ]
}

/// `CSWITCH_CLAUDE_LOCK_BUDGET_MS` shortens the wait (tests); anything else is the default.
pub fn wait_budget() -> Duration {
    budget_from(
        std::env::var("CSWITCH_CLAUDE_LOCK_BUDGET_MS")
            .ok()
            .as_deref(),
    )
}

fn budget_from(value: Option<&str>) -> Duration {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(WAIT_BUDGET, Duration::from_millis)
}

pub fn acquire(paths: &Paths) -> Result<ClaudeLocks> {
    acquire_with(specs(paths), wait_budget(), TOUCH_INTERVAL)
}

/// Holds the directories until dropped and keeps their mtime fresh meanwhile.
#[derive(Debug)]
pub struct ClaudeLocks {
    held: Vec<PathBuf>,
    stop: Arc<AtomicBool>,
    toucher: Option<JoinHandle<()>>,
}

impl ClaudeLocks {
    pub fn held(&self) -> &[PathBuf] {
        &self.held
    }
}

pub fn acquire_with(
    specs: Vec<LockSpec>,
    budget: Duration,
    touch_every: Duration,
) -> Result<ClaudeLocks> {
    let mut locks = ClaudeLocks {
        held: Vec::new(),
        stop: Arc::new(AtomicBool::new(false)),
        toucher: None,
    };
    for spec in &specs {
        // On an error the guard drops here and releases what was taken.
        acquire_one(spec, budget)?;
        locks.held.push(spec.path.clone());
    }
    let paths = locks.held.clone();
    let stop = locks.stop.clone();
    locks.toucher = Some(thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let mut slept = Duration::ZERO;
            while slept < touch_every && !stop.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(50));
                slept += Duration::from_millis(50);
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            for path in &paths {
                let _ = filetime::set_file_mtime(path, filetime::FileTime::now());
            }
        }
    }));
    Ok(locks)
}

fn is_stale(path: &Path, stale: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > stale)
}

fn acquire_one(spec: &LockSpec, budget: Duration) -> Result<()> {
    let started = Instant::now();
    if let Some(parent) = spec.path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    loop {
        match fs::create_dir(&spec.path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                // Only a stale lock directory is taken over. A stale plain file or a
                // directory that cannot be removed counts as held and runs into the budget.
                if is_stale(&spec.path, spec.stale)
                    && spec.path.is_dir()
                    && fs::remove_dir_all(&spec.path).is_ok()
                {
                    continue;
                }
                let elapsed = started.elapsed();
                if elapsed >= budget {
                    return Err(CswitchError::lock(format!(
                        "Claude Code is holding {}; retry in a moment",
                        spec.path.display()
                    )));
                }
                // 1–2 s jittered, as Claude Code's own retries; never past the budget.
                let jitter = Duration::from_millis(1000 + rand::random::<u64>() % 1000);
                thread::sleep(jitter.min(budget - elapsed));
            }
            Err(err) => return Err(io_error(CswitchError::Lock, &spec.path, &err)),
        }
    }
}

impl Drop for ClaudeLocks {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(toucher) = self.toucher.take() {
            let _ = toucher.join();
        }
        for path in self.held.iter().rev() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn spec(dir: &std::path::Path, name: &str, stale: Duration) -> LockSpec {
        LockSpec {
            path: dir.join(name),
            stale,
        }
    }

    #[test]
    fn acquire_creates_dirs_in_order_and_drop_removes_them() {
        let dir = tempfile::tempdir().unwrap();
        let specs = vec![
            spec(dir.path(), "a.lock", CREDENTIALS_STALE),
            spec(dir.path(), "nested/b.lock", CONFIG_STALE),
        ];
        let locks = acquire_with(specs.clone(), Duration::from_millis(50), TOUCH_INTERVAL).unwrap();
        assert!(dir.path().join("a.lock").is_dir());
        assert!(dir.path().join("nested/b.lock").is_dir());
        assert_eq!(locks.held().len(), 2);
        drop(locks);
        assert!(!dir.path().join("a.lock").exists());
        assert!(!dir.path().join("nested/b.lock").exists());
    }

    #[test]
    fn stale_lock_is_stolen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.lock");
        std::fs::create_dir(&path).unwrap();
        let old =
            filetime::FileTime::from_unix_time(filetime::FileTime::now().unix_seconds() - 120, 0);
        filetime::set_file_mtime(&path, old).unwrap();
        let locks = acquire_with(
            vec![spec(dir.path(), "a.lock", Duration::from_secs(60))],
            Duration::from_millis(50),
            TOUCH_INTERVAL,
        )
        .unwrap();
        assert_eq!(locks.held().len(), 1);
        let age = std::fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .elapsed()
            .unwrap();
        assert!(age < Duration::from_secs(5), "recreated now");
    }

    #[test]
    fn fresh_lock_times_out_with_a_lock_error_and_releases_earlier_locks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("b.lock")).unwrap();
        let started = std::time::Instant::now();
        let err = acquire_with(
            vec![
                spec(dir.path(), "a.lock", CREDENTIALS_STALE),
                spec(dir.path(), "b.lock", CREDENTIALS_STALE),
            ],
            Duration::from_millis(200),
            TOUCH_INTERVAL,
        )
        .unwrap_err();
        assert_eq!(err.type_name(), "LockError");
        assert!(
            err.to_string().starts_with("Claude Code is holding "),
            "{err}"
        );
        assert!(err.to_string().ends_with("; retry in a moment"));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            !dir.path().join("a.lock").exists(),
            "the first lock is released on failure"
        );
        assert!(
            dir.path().join("b.lock").exists(),
            "the foreign lock is left alone"
        );
    }

    #[test]
    fn stale_plain_file_is_not_taken_over_and_times_out_without_spinning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.lock");
        std::fs::write(&path, b"x").unwrap();
        let old =
            filetime::FileTime::from_unix_time(filetime::FileTime::now().unix_seconds() - 120, 0);
        filetime::set_file_mtime(&path, old).unwrap();
        let started = std::time::Instant::now();
        let err = acquire_with(
            vec![spec(dir.path(), "a.lock", Duration::from_secs(60))],
            Duration::from_millis(200),
            TOUCH_INTERVAL,
        )
        .unwrap_err();
        assert!(
            err.to_string().starts_with("Claude Code is holding "),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(path.is_file(), "the foreign file is left alone");
    }

    #[test]
    fn held_locks_are_touched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.lock");
        let locks = acquire_with(
            vec![spec(dir.path(), "a.lock", CREDENTIALS_STALE)],
            Duration::from_millis(50),
            Duration::from_millis(100),
        )
        .unwrap();
        let old =
            filetime::FileTime::from_unix_time(filetime::FileTime::now().unix_seconds() - 120, 0);
        filetime::set_file_mtime(&path, old).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        let age = std::fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .elapsed()
            .unwrap();
        assert!(age < Duration::from_secs(5), "touched while held: {age:?}");
        drop(locks);
    }

    #[test]
    fn specs_follow_the_paths_and_the_budget_env() {
        let (_dir, store) = crate::store::temp_store();
        let specs = specs(&store.paths);
        assert_eq!(specs[0].path, store.paths.claude_refresh_lock_dir());
        assert_eq!(specs[0].stale, CREDENTIALS_STALE);
        assert_eq!(specs[1].path, store.paths.claude_legacy_lock_dir());
        assert_eq!(specs[2].path, store.paths.claude_config_lock_dir());
        assert_eq!(specs[2].stale, CONFIG_STALE);
        assert_eq!(budget_from(None), WAIT_BUDGET);
        assert_eq!(budget_from(Some("250")), Duration::from_millis(250));
        assert_eq!(budget_from(Some("x")), WAIT_BUDGET);
    }
}
