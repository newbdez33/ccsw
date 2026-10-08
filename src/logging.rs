//! The rotating log file in the backup root (`ccsw.log`, 1 MiB × 3) plus
//! the `--debug` mirror on stderr.
//!
//! The file is opened on the first record, so a run that logs nothing leaves
//! no trace of the backup root. [`init`] is idempotent: the process-wide
//! subscriber can only be installed once, and later calls are no-ops.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::paths::Paths;

pub const MAX_BYTES: u64 = 1024 * 1024;
pub const BACKUPS: u32 = 3;

/// Install the file layer (INFO and up) and, with `debug`, a stderr layer at
/// DEBUG. Safe to call more than once per process.
pub fn init(paths: &Paths, debug: bool) {
    let file = RotatingFile::new(paths.log_file(), MAX_BYTES, BACKUPS);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(LogSink(Arc::new(Mutex::new(file))))
        .with_ansi(false)
        .with_target(false)
        .with_filter(LevelFilter::INFO);
    let stderr_layer = debug.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_filter(LevelFilter::DEBUG)
    });
    let _ = tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .try_init();
}

/// A size-rotated append-only file: `<path>` is renamed to `<path>.1` (and
/// `.1` → `.2`, …, the oldest dropped) once a record would push it past
/// `max_bytes`. Opened lazily; an open failure disables it for the process.
struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    backups: u32,
    file: Option<File>,
    len: u64,
    disabled: bool,
}

impl RotatingFile {
    fn new(path: PathBuf, max_bytes: u64, backups: u32) -> Self {
        Self {
            path,
            max_bytes,
            backups,
            file: None,
            len: 0,
            disabled: false,
        }
    }

    fn numbered(&self, n: u32) -> PathBuf {
        PathBuf::from(format!("{}.{n}", self.path.display()))
    }

    fn open(&mut self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }
        let mut options = OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&self.path)?;
        self.len = file.metadata()?.len();
        self.file = Some(file);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        for n in (1..=self.backups).rev() {
            let from = if n == 1 {
                self.path.clone()
            } else {
                self.numbered(n - 1)
            };
            let to = self.numbered(n);
            if n == self.backups {
                remove_if_exists(&to)?;
            }
            if from.exists() {
                fs::rename(&from, &to)?;
            }
        }
        self.open()
    }

    fn write_record(&mut self, record: &[u8]) -> io::Result<()> {
        if self.disabled {
            return Ok(());
        }
        if self.file.is_none()
            && let Err(err) = self.open()
        {
            self.disabled = true;
            return Err(err);
        }
        if self.len > 0 && self.len + record.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        let file = self.file.as_mut().expect("opened above");
        file.write_all(record)?;
        self.len += record.len() as u64;
        Ok(())
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[derive(Clone)]
struct LogSink(Arc<Mutex<RotatingFile>>);

struct LogWriter(Arc<Mutex<RotatingFile>>);

impl<'a> MakeWriter<'a> for LogSink {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.0.clone())
    }
}

impl Write for LogWriter {
    /// The fmt layer hands over one whole record per write, so the rotation
    /// check in `write_record` sits on a record boundary. Failures are
    /// swallowed: logging must never break a command.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut file) = self.0.lock() {
            let _ = file.write_record(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn opens_lazily_and_rotates_keeping_three_backups() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let mut log = RotatingFile::new(root.join("ccsw.log"), 100, 3);
        assert!(!root.exists(), "nothing is created before the first record");

        log.write_record(b"first record, sixty bytes long ......................... 1\n")
            .unwrap();
        assert_eq!(names(&root), ["ccsw.log"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&root), 0o700);
            assert_eq!(mode(&root.join("ccsw.log")), 0o600);
        }

        // The second record would cross 100 bytes: the file rotates first.
        log.write_record(b"second record, sixty bytes long ........................ 2\n")
            .unwrap();
        assert_eq!(names(&root), ["ccsw.log", "ccsw.log.1"]);
        assert!(
            fs::read_to_string(root.join("ccsw.log.1"))
                .unwrap()
                .starts_with("first record")
        );
        assert!(
            fs::read_to_string(root.join("ccsw.log"))
                .unwrap()
                .starts_with("second record")
        );

        for i in 3..8 {
            log.write_record(
                format!("record number {i} ....................................... {i}\n")
                    .as_bytes(),
            )
            .unwrap();
        }
        assert_eq!(
            names(&root),
            ["ccsw.log", "ccsw.log.1", "ccsw.log.2", "ccsw.log.3"]
        );
        assert!(
            fs::read_to_string(root.join("ccsw.log"))
                .unwrap()
                .starts_with("record number 7")
        );
        assert!(
            fs::read_to_string(root.join("ccsw.log.3"))
                .unwrap()
                .starts_with("record number 4")
        );

        // Reopening appends and picks up the current size.
        let mut reopened = RotatingFile::new(root.join("ccsw.log"), 100, 3);
        reopened.write_record(b"short\n").unwrap();
        let text = fs::read_to_string(root.join("ccsw.log")).unwrap();
        assert!(text.starts_with("record number 7") && text.ends_with("short\n"));
    }

    #[test]
    fn an_unwritable_location_disables_the_file_quietly() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "x").unwrap();
        // The parent "directory" is a regular file, so the open fails.
        let mut log = RotatingFile::new(blocker.join("ccsw.log"), 100, 3);
        assert!(log.write_record(b"one\n").is_err());
        assert!(log.disabled);
        assert!(
            log.write_record(b"two\n").is_ok(),
            "later records are dropped"
        );
        let mut writer = LogWriter(Arc::new(Mutex::new(log)));
        assert_eq!(writer.write(b"three\n").unwrap(), 6);
    }

    #[test]
    fn init_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_values(
            Some(dir.path().join("store")),
            Some(dir.path().join("codex")),
            None,
            dir.path(),
        )
        .unwrap();
        init(&paths, false);
        init(&paths, true);
    }
}
