//! App-server daemon probe/restart and the `--no-daemon` launch rule.
//!
//! Codex CLI 0.157 and newer attaches interactive sessions to a managed local
//! app-server daemon. That daemon loads `auth.json` once and re-reads it only
//! for the account it already holds, so after cswitch replaces the file every
//! new Codex session keeps the previous account until the daemon restarts.
//! `codex exec` and `codex --no-daemon` run in process and read the file at
//! startup, so they are not affected.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::auth::sha256_file;

/// What happened to the managed app-server daemon after the live `auth.json`
/// changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonRestart {
    /// No managed daemon is running, or this Codex has no daemon; nothing to do.
    NotRunning,
    /// The live `auth.json` is byte-identical to the snapshot taken before the
    /// change, so the daemon already holds it.
    Unchanged,
    Restarted,
    /// The daemon is running but did not restart. The live `auth.json` is
    /// already switched, so this is a warning rather than a failed switch.
    Failed(String),
}

impl DaemonRestart {
    /// The follow-up line once `account` (e.g. `Account-2`) is live (spec §4);
    /// `None` when the live file did not change.
    pub fn message(&self, account: &str) -> Option<String> {
        match self {
            Self::Unchanged => None,
            Self::NotRunning => Some(
                "New account is active for the next Codex session — restart any running codex exec / --no-daemon session."
                    .to_string(),
            ),
            Self::Restarted => Some(format!(
                "Restarted the Codex app-server daemon — new and reconnecting Codex sessions use {account}."
            )),
            Self::Failed(detail) => Some(format!(
                "Warning: the Codex app-server daemon still holds the previous account ({detail}). Run `codex app-server daemon restart` so Codex sessions use {account}."
            )),
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// Content hash of the live `auth.json`, taken before a credential change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAuthSnapshot(Option<String>);

pub fn snapshot_live_auth(path: &Path) -> LiveAuthSnapshot {
    LiveAuthSnapshot(sha256_file(path))
}

/// Restart the managed daemon when one is running and the live `auth.json`
/// no longer matches `before`. Re-selecting the account that is already live
/// leaves the file identical and needs no restart.
pub fn restart_daemon_if_live_auth_changed(
    before: &LiveAuthSnapshot,
    path: &Path,
) -> DaemonRestart {
    if live_auth_unchanged(before, &snapshot_live_auth(path)) {
        return DaemonRestart::Unchanged;
    }
    restart_daemon_if_running()
}

/// A missing or unreadable file before the change gives no evidence of what
/// the daemon holds, so that counts as changed.
fn live_auth_unchanged(before: &LiveAuthSnapshot, after: &LiveAuthSnapshot) -> bool {
    before.0.is_some() && before == after
}

/// A daemon that is not running is left alone: Codex starts one on demand and
/// it then loads the current file.
fn restart_daemon_if_running() -> DaemonRestart {
    let Some(codex) = command_on_path("codex") else {
        tracing::debug!("codex not found on PATH; skipping app-server daemon restart");
        return DaemonRestart::NotRunning;
    };
    restart_with(|args| {
        Command::new(&codex)
            .args(args)
            .stdin(Stdio::null())
            .output()
    })
}

/// The restart decision, with `run_codex` standing in for the `codex` binary.
pub fn restart_with<F>(mut run_codex: F) -> DaemonRestart
where
    F: FnMut(&[&str]) -> io::Result<Output>,
{
    match run_codex(&["app-server", "daemon", "version"]) {
        Ok(output) if daemon_is_running(&output) => {}
        Ok(output) => {
            tracing::debug!(
                "no running Codex app-server daemon to restart: {}",
                failure_detail(&output)
            );
            return DaemonRestart::NotRunning;
        }
        Err(err) => {
            tracing::debug!("could not query the Codex app-server daemon: {err}");
            return DaemonRestart::NotRunning;
        }
    }
    match run_codex(&["app-server", "daemon", "restart"]) {
        Ok(output) if output.status.success() => DaemonRestart::Restarted,
        Ok(output) => DaemonRestart::Failed(failure_detail(&output)),
        Err(err) => DaemonRestart::Failed(err.to_string()),
    }
}

/// `codex app-server daemon version` prints `{"status":"running",…}` for a
/// live daemon. A stopped daemon fails to connect and an older Codex rejects
/// the subcommand; neither may be restarted, because `restart` would start a
/// daemon nobody asked for.
fn daemon_is_running(output: &Output) -> bool {
    output.status.success()
        && serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .is_some_and(|version| version["status"] == "running")
}

fn failure_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    match stderr.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => line.to_string(),
        None => format!("codex exited with {}", output.status),
    }
}

/// Locate an executable on `PATH` without running it (`codex --version`
/// writes into `$CODEX_HOME/tmp`). Windows also tries `.exe`, `.cmd`, `.bat`.
pub fn command_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    let candidates = if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
            name.to_string(),
        ]
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(&paths)
        .flat_map(|dir| candidates.iter().map(move |file| dir.join(file)))
        .find(|candidate| candidate.is_file())
}

/// `--no-daemon` exists since Codex 0.156; an older Codex rejects unknown
/// options, so its root help decides whether the flag can be passed.
pub fn codex_supports_no_daemon(command: &Path) -> bool {
    Command::new(command)
        .arg("--help")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("--no-daemon")
        })
}

/// Keep a session-mode launch on its own `CODEX_HOME` instead of the shared
/// daemon: prepend `--no-daemon` (a root option, so before any subcommand)
/// unless the argv already picks its server (`--no-daemon`, `--remote`, or
/// the daemon-only `agents` command).
pub fn embedded_codex_argv(supports_no_daemon: bool, mut argv: Vec<String>) -> Vec<String> {
    if supports_no_daemon && !argv.iter().any(|arg| picks_app_server(arg)) {
        argv.insert(0, "--no-daemon".to_string());
    }
    argv
}

fn picks_app_server(arg: &str) -> bool {
    arg == "--no-daemon" || arg == "--remote" || arg.starts_with("--remote=") || arg == "agents"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::process::ExitStatus;

    fn exit_status(code: i32) -> ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(code << 8)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(code as u32)
        }
    }

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: exit_status(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    const RUNNING: &str = r#"{"status":"running","backend":"pid","cliVersion":"0.158.0","appServerVersion":"0.159.0"}"#;
    const VERSION_ARGS: [&str; 3] = ["app-server", "daemon", "version"];
    const RESTART_ARGS: [&str; 3] = ["app-server", "daemon", "restart"];

    /// Records every codex invocation and answers each from a script.
    struct FakeCodex {
        calls: RefCell<Vec<Vec<String>>>,
        answers: RefCell<Vec<io::Result<Output>>>,
    }

    impl FakeCodex {
        fn new(answers: Vec<io::Result<Output>>) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                answers: RefCell::new(answers.into_iter().rev().collect()),
            }
        }

        fn run(&self, args: &[&str]) -> io::Result<Output> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            self.answers
                .borrow_mut()
                .pop()
                .expect("more codex invocations than scripted answers")
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    #[test]
    fn running_daemon_is_restarted() {
        let codex = FakeCodex::new(vec![
            Ok(output(0, RUNNING, "")),
            Ok(output(0, r#"{"status":"restarted"}"#, "")),
        ]);
        assert_eq!(
            restart_with(|args| codex.run(args)),
            DaemonRestart::Restarted
        );
        assert_eq!(
            codex.calls(),
            vec![VERSION_ARGS.to_vec(), RESTART_ARGS.to_vec()]
        );
    }

    #[test]
    fn stopped_old_or_missing_codex_is_left_alone() {
        let stopped = FakeCodex::new(vec![Ok(output(
            1,
            "",
            "Error: failed to connect to /home/u/.codex/app-server-control/app-server-control.sock",
        ))]);
        assert_eq!(
            restart_with(|args| stopped.run(args)),
            DaemonRestart::NotRunning
        );
        assert_eq!(stopped.calls(), vec![VERSION_ARGS.to_vec()]);

        let old = FakeCodex::new(vec![Ok(output(
            2,
            "",
            "error: unrecognized subcommand 'daemon'",
        ))]);
        assert_eq!(
            restart_with(|args| old.run(args)),
            DaemonRestart::NotRunning
        );

        let not_running = FakeCodex::new(vec![Ok(output(0, r#"{"status":"notRunning"}"#, ""))]);
        assert_eq!(
            restart_with(|args| not_running.run(args)),
            DaemonRestart::NotRunning
        );
        let garbage = FakeCodex::new(vec![Ok(output(0, "not json", ""))]);
        assert_eq!(
            restart_with(|args| garbage.run(args)),
            DaemonRestart::NotRunning
        );

        let missing = FakeCodex::new(vec![Err(io::Error::from(io::ErrorKind::NotFound))]);
        assert_eq!(
            restart_with(|args| missing.run(args)),
            DaemonRestart::NotRunning
        );
        assert_eq!(missing.calls(), vec![VERSION_ARGS.to_vec()]);
    }

    #[test]
    fn failed_restart_reports_the_first_stderr_line_or_status() {
        let codex = FakeCodex::new(vec![
            Ok(output(0, RUNNING, "")),
            Ok(output(
                1,
                "",
                "\nError: app server is running but is not managed by codex app-server daemon\n\nCaused by:\n    something\n",
            )),
        ]);
        assert_eq!(
            restart_with(|args| codex.run(args)),
            DaemonRestart::Failed(
                "Error: app server is running but is not managed by codex app-server daemon".into()
            )
        );

        let silent = FakeCodex::new(vec![Ok(output(0, RUNNING, "")), Ok(output(3, "", "  \n"))]);
        let DaemonRestart::Failed(detail) = restart_with(|args| silent.run(args)) else {
            panic!("a failed restart must be reported");
        };
        assert!(detail.contains('3'), "{detail}");

        let unspawnable = FakeCodex::new(vec![
            Ok(output(0, RUNNING, "")),
            Err(io::Error::from(io::ErrorKind::PermissionDenied)),
        ]);
        assert!(restart_with(|args| unspawnable.run(args)).is_failure());
    }

    #[test]
    fn identical_live_auth_needs_no_restart() {
        let a = LiveAuthSnapshot(Some("a".into()));
        let b = LiveAuthSnapshot(Some("b".into()));
        let missing = LiveAuthSnapshot(None);
        assert!(live_auth_unchanged(&a, &a));
        assert!(!live_auth_unchanged(&a, &b));
        assert!(!live_auth_unchanged(&a, &missing));
        assert!(!live_auth_unchanged(&missing, &a));
        assert!(
            !live_auth_unchanged(&missing, &missing),
            "no file before the change says nothing about what the daemon holds"
        );
    }

    #[test]
    fn snapshot_reflects_the_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("auth.json");
        assert_eq!(snapshot_live_auth(&live), LiveAuthSnapshot(None));
        std::fs::write(&live, "{}").unwrap();
        let before = snapshot_live_auth(&live);
        assert!(before.0.is_some());
        assert_eq!(snapshot_live_auth(&live), before);
        // Unchanged file: no codex lookup happens, whatever PATH holds.
        assert_eq!(
            restart_daemon_if_live_auth_changed(&before, &live),
            DaemonRestart::Unchanged
        );
        std::fs::write(&live, "{\"changed\":true}").unwrap();
        assert_ne!(snapshot_live_auth(&live), before);
    }

    #[test]
    fn messages_follow_the_spec() {
        assert_eq!(DaemonRestart::Unchanged.message("Account-2"), None);
        assert_eq!(
            DaemonRestart::NotRunning.message("Account-2").unwrap(),
            "New account is active for the next Codex session — restart any running codex exec / --no-daemon session."
        );
        assert_eq!(
            DaemonRestart::Restarted.message("Account-2").unwrap(),
            "Restarted the Codex app-server daemon — new and reconnecting Codex sessions use Account-2."
        );
        assert_eq!(
            DaemonRestart::Failed("boom".into())
                .message("Account-2")
                .unwrap(),
            "Warning: the Codex app-server daemon still holds the previous account (boom). Run `codex app-server daemon restart` so Codex sessions use Account-2."
        );
    }

    #[test]
    fn no_daemon_flag_goes_first_unless_argv_picks_a_server() {
        let argv = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            embedded_codex_argv(true, argv(&["exec", "hi"])),
            argv(&["--no-daemon", "exec", "hi"])
        );
        assert_eq!(embedded_codex_argv(true, vec![]), argv(&["--no-daemon"]));
        assert_eq!(embedded_codex_argv(false, argv(&["exec"])), argv(&["exec"]));
        for picked in [
            argv(&["--no-daemon", "exec"]),
            argv(&["--remote"]),
            argv(&["--remote=ws://x"]),
            argv(&["agents", "list"]),
        ] {
            assert_eq!(embedded_codex_argv(true, picked.clone()), picked);
        }
    }

    #[cfg(unix)]
    #[test]
    fn command_lookup_and_help_probe_use_real_processes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let with_flag = dir.path().join("with-flag");
        std::fs::write(
            &with_flag,
            "#!/bin/sh\necho 'Options:\n  --no-daemon  Run in process'\n",
        )
        .unwrap();
        let without_flag = dir.path().join("without-flag");
        std::fs::write(&without_flag, "#!/bin/sh\necho 'Options:'\n").unwrap();
        let failing = dir.path().join("failing");
        std::fs::write(&failing, "#!/bin/sh\necho --no-daemon; exit 1\n").unwrap();
        for script in [&with_flag, &without_flag, &failing] {
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(codex_supports_no_daemon(&with_flag));
        assert!(!codex_supports_no_daemon(&without_flag));
        assert!(!codex_supports_no_daemon(&failing));
        assert!(!codex_supports_no_daemon(&dir.path().join("absent")));

        assert!(command_on_path("sh").is_some());
        assert!(command_on_path("cswitch-surely-not-installed-xyz").is_none());
    }
}
