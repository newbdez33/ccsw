//! macOS Keychain access through `/usr/bin/security` (spec §4): the same
//! commands Claude Code runs, so creator == reader and nothing prompts.
//! Compiled everywhere; only meaningful on macOS.

use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Claude Code's live OAuth credential.
pub const LIVE_SERVICE: &str = "Claude Code-credentials";
/// Claude Code's managed API key (`/login` with an `sk-ant-api…` key).
pub const MANAGED_SERVICE: &str = "Claude Code";
/// Pinned: a `security` earlier on PATH must never see a credential.
const SECURITY: &str = "/usr/bin/security";
/// `errSecItemNotFound` as surfaced by find/delete-generic-password.
const NOT_FOUND_RC: i32 = 44;
/// A wedged Keychain (locked, headless) must not hang the CLI.
const TIMEOUT: Duration = Duration::from_secs(5);
/// `security -i` reads stdin with a 4096-byte line buffer; keep headroom.
const STDIN_LINE_LIMIT: usize = 4096 - 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOutput {
    pub status: i32,
    pub stdout: String,
}

/// How `security` is run; tests substitute a recorder.
pub trait SecurityCli {
    fn run(&self, args: &[String], stdin_line: Option<&str>) -> io::Result<CliOutput>;
}

/// The real binary, with a bounded wait.
pub struct SystemSecurity;

impl SecurityCli for SystemSecurity {
    fn run(&self, args: &[String], stdin_line: Option<&str>) -> io::Result<CliOutput> {
        let mut command = Command::new(SECURITY);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(if stdin_line.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command.spawn()?;
        if let Some(line) = stdin_line {
            let mut stdin = child.stdin.take().expect("piped stdin");
            stdin.write_all(line.as_bytes())?;
            stdin.write_all(b"\n")?;
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                let mut stdout = String::new();
                if let Some(mut out) = child.stdout.take() {
                    out.read_to_string(&mut stdout)?;
                }
                return Ok(CliOutput {
                    status: status.code().unwrap_or(-1),
                    stdout,
                });
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "security did not answer within 5 s",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeychainError {
    /// Locked, denied, timed out, or any exit other than "not found".
    Unavailable(String),
}

impl fmt::Display for KeychainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(detail) => write!(f, "keychain unavailable: {detail}"),
        }
    }
}

impl std::error::Error for KeychainError {}

/// The account name of the live item, mirroring Claude Code's `getUsername()`:
/// `$USER`, then the OS user name, then a stable fallback.
pub fn account_name() -> String {
    if let Some(user) = std::env::var_os("USER").filter(|u| !u.is_empty()) {
        return user.to_string_lossy().into_owned();
    }
    #[cfg(unix)]
    if let Some(name) = unix_user_name() {
        return name;
    }
    "claude-code-user".to_string()
}

#[cfg(unix)]
fn unix_user_name() -> Option<String> {
    // SAFETY: getpwuid returns a pointer to static storage or null; the
    // C string is read immediately and never retained.
    unsafe {
        let entry = libc::getpwuid(libc::geteuid());
        if entry.is_null() {
            return None;
        }
        let name = std::ffi::CStr::from_ptr((*entry).pw_name);
        Some(name.to_string_lossy().into_owned())
    }
}

pub struct Keychain<'a> {
    cli: &'a dyn SecurityCli,
}

fn argv<const N: usize>(parts: [&str; N]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Double-quote a `security -i` argument.
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

impl<'a> Keychain<'a> {
    pub fn new(cli: &'a dyn SecurityCli) -> Self {
        Self { cli }
    }

    fn unavailable(what: &str, out: &CliOutput) -> KeychainError {
        KeychainError::Unavailable(format!("{what} exited {}", out.status))
    }

    /// `None` when the item does not exist.
    pub fn get_password(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<String>, KeychainError> {
        let args = argv(["find-generic-password", "-a", account, "-w", "-s", service]);
        match self.cli.run(&args, None) {
            Ok(out) if out.status == 0 => {
                Ok(Some(out.stdout.trim_end_matches(['\n', '\r']).to_string()))
            }
            Ok(out) if out.status == NOT_FOUND_RC => Ok(None),
            Ok(out) => Err(Self::unavailable("find-generic-password", &out)),
            Err(err) => Err(KeychainError::Unavailable(err.to_string())),
        }
    }

    pub fn item_exists(&self, service: &str, account: &str) -> Result<bool, KeychainError> {
        let args = argv(["find-generic-password", "-a", account, "-s", service]);
        match self.cli.run(&args, None) {
            Ok(out) if out.status == 0 => Ok(true),
            Ok(out) if out.status == NOT_FOUND_RC => Ok(false),
            Ok(out) => Err(Self::unavailable("find-generic-password", &out)),
            Err(err) => Err(KeychainError::Unavailable(err.to_string())),
        }
    }

    /// Create or update (`-U`) the item. The value is hex-encoded (`-X`) and the
    /// whole command goes through `security -i` on stdin so the secret never
    /// appears in argv; a command over the stdin line limit uses argv instead.
    pub fn set_password(
        &self,
        service: &str,
        account: &str,
        value: &str,
    ) -> Result<(), KeychainError> {
        let hex_value = hex::encode(value.as_bytes());
        let line = format!(
            "add-generic-password -U -a {} -s {} -X {hex_value}",
            quote(account),
            quote(service)
        );
        let result = if line.len() <= STDIN_LINE_LIMIT {
            self.cli.run(&argv(["-i"]), Some(&line))
        } else {
            self.cli.run(
                &argv([
                    "add-generic-password",
                    "-U",
                    "-a",
                    account,
                    "-s",
                    service,
                    "-X",
                    &hex_value,
                ]),
                None,
            )
        };
        match result {
            Ok(out) if out.status == 0 => Ok(()),
            Ok(out) => Err(Self::unavailable("add-generic-password", &out)),
            Err(err) => Err(KeychainError::Unavailable(err.to_string())),
        }
    }

    /// `true` when an item was removed, `false` when there was none.
    pub fn delete_password(&self, service: &str, account: &str) -> Result<bool, KeychainError> {
        let args = argv(["delete-generic-password", "-a", account, "-s", service]);
        match self.cli.run(&args, None) {
            Ok(out) if out.status == 0 => Ok(true),
            Ok(out) if out.status == NOT_FOUND_RC => Ok(false),
            Ok(out) => Err(Self::unavailable("delete-generic-password", &out)),
            Err(err) => Err(KeychainError::Unavailable(err.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Records every call and answers from a queue.
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<(Vec<String>, Option<String>)>>,
        replies: RefCell<VecDeque<io::Result<CliOutput>>>,
    }

    impl Fake {
        fn reply(&self, status: i32, stdout: &str) {
            self.replies.borrow_mut().push_back(Ok(CliOutput {
                status,
                stdout: stdout.to_string(),
            }));
        }
        fn fail(&self) {
            self.replies
                .borrow_mut()
                .push_back(Err(io::Error::new(io::ErrorKind::TimedOut, "stuck")));
        }
        fn calls(&self) -> Vec<(Vec<String>, Option<String>)> {
            self.calls.borrow().clone()
        }
    }

    impl SecurityCli for Fake {
        fn run(&self, args: &[String], stdin_line: Option<&str>) -> io::Result<CliOutput> {
            self.calls
                .borrow_mut()
                .push((args.to_vec(), stdin_line.map(str::to_string)));
            self.replies
                .borrow_mut()
                .pop_front()
                .expect("scripted reply")
        }
    }

    #[test]
    fn get_password_distinguishes_missing_from_broken() {
        let fake = Fake::default();
        let keychain = Keychain::new(&fake);
        fake.reply(0, "{\"claudeAiOauth\":{}}\n");
        assert_eq!(
            keychain.get_password(LIVE_SERVICE, "me").unwrap(),
            Some("{\"claudeAiOauth\":{}}".to_string())
        );
        fake.reply(44, "");
        assert_eq!(keychain.get_password(LIVE_SERVICE, "me").unwrap(), None);
        fake.reply(36, "");
        assert!(matches!(
            keychain.get_password(LIVE_SERVICE, "me"),
            Err(KeychainError::Unavailable(_))
        ));
        fake.fail();
        assert!(keychain.get_password(LIVE_SERVICE, "me").is_err());
        let calls = fake.calls();
        assert_eq!(
            calls[0].0,
            [
                "find-generic-password",
                "-a",
                "me",
                "-w",
                "-s",
                LIVE_SERVICE
            ]
        );
        assert_eq!(calls[0].1, None);
    }

    #[test]
    fn set_password_goes_through_stdin_as_hex_and_falls_back_to_argv() {
        let fake = Fake::default();
        let keychain = Keychain::new(&fake);
        fake.reply(0, "");
        keychain
            .set_password(LIVE_SERVICE, "me", "{\"a\":\"b\"}")
            .unwrap();
        let (args, line) = fake.calls()[0].clone();
        assert_eq!(args, ["-i"]);
        assert_eq!(
            line.unwrap(),
            format!(
                "add-generic-password -U -a \"me\" -s \"{LIVE_SERVICE}\" -X {}",
                hex::encode("{\"a\":\"b\"}")
            )
        );
        fake.reply(0, "");
        let long = "x".repeat(3000);
        keychain.set_password(MANAGED_SERVICE, "me", &long).unwrap();
        let (args, line) = fake.calls()[1].clone();
        assert_eq!(line, None, "a line over 4032 bytes uses argv");
        assert_eq!(args[0], "add-generic-password");
        assert_eq!(args[7], hex::encode(&long));
        fake.reply(1, "");
        assert!(keychain.set_password(LIVE_SERVICE, "me", "v").is_err());
    }

    #[test]
    fn quoting_escapes_backslashes_and_quotes() {
        assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn delete_and_exists() {
        let fake = Fake::default();
        let keychain = Keychain::new(&fake);
        fake.reply(0, "");
        assert!(keychain.delete_password(LIVE_SERVICE, "me").unwrap());
        fake.reply(44, "");
        assert!(!keychain.delete_password(LIVE_SERVICE, "me").unwrap());
        fake.reply(0, "keychain: ...");
        assert!(keychain.item_exists(MANAGED_SERVICE, "me").unwrap());
        fake.reply(44, "");
        assert!(!keychain.item_exists(MANAGED_SERVICE, "me").unwrap());
        let calls = fake.calls();
        assert_eq!(calls[0].0[0], "delete-generic-password");
        assert_eq!(
            calls[2].0,
            ["find-generic-password", "-a", "me", "-s", MANAGED_SERVICE]
        );
    }

    #[test]
    fn account_name_prefers_user() {
        // $USER is set in every CI shell we run on; the fallbacks are exercised by inspection.
        if let Ok(user) = std::env::var("USER") {
            assert_eq!(account_name(), user);
        }
        assert!(!account_name().is_empty());
    }
}

/// Fakes shared by the engine tests: no process is spawned.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io;

    use super::{CliOutput, SecurityCli};

    /// A `security` that is either empty (every item "not found", rc 44) or
    /// broken (every call fails, as a locked or missing Keychain does).
    pub(crate) struct FakeSecurity {
        pub failing: bool,
    }

    impl SecurityCli for FakeSecurity {
        fn run(&self, _args: &[String], _stdin_line: Option<&str>) -> io::Result<CliOutput> {
            if self.failing {
                Err(io::Error::other("keychain locked"))
            } else {
                Ok(CliOutput {
                    status: 44,
                    stdout: String::new(),
                })
            }
        }
    }
}
