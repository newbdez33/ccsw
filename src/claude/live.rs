//! Claude Code's live login: read and write across the macOS Keychain and the
//! plaintext file exactly as Claude Code does (spec §4, §7), the global
//! config splice, and the pre-switch backup.

use std::cell::Cell;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::errors::{CcswError, Result};
use crate::fsutil;
use crate::model::Identity;
use crate::paths::Paths;
use crate::store::ensure_private_dir;

use super::credentials::{
    ClaudeCredential, CredentialKind, MANAGED_KEY, OAUTH_ACCOUNT_KEY, OAUTH_KEY, OauthAccount,
};
use super::keychain::{Keychain, LIVE_SERVICE, MANAGED_SERVICE, SecurityCli, account_name};

/// The line printed after a switch, keyed to where the live write landed.
pub const KEYCHAIN_FOLLOWUP: &str = "Restart Claude Code to apply immediately — otherwise the session can take up to ~30 seconds to pick up the new account.";
pub const FILE_FOLLOWUP: &str = "New account is active on your next message — no restart needed.";
const MAX_BACKUPS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Keychain,
    File,
}

impl Backend {
    pub fn followup(self) -> &'static str {
        match self {
            Self::Keychain => KEYCHAIN_FOLLOWUP,
            Self::File => FILE_FOLLOWUP,
        }
    }
}

/// What Claude Code is logged in as right now.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveLogin {
    pub credential: Option<ClaudeCredential>,
    /// The credential object exactly as read from the backend consulted,
    /// whatever its kind (siblings such as `mcpOAuth` included); `None` only
    /// when nothing was readable.
    pub raw: Option<ClaudeCredential>,
    pub oauth_account: Option<OauthAccount>,
    /// The Keychain failed (or the process is pinned to the file after a
    /// failure) and nothing else covered the login.
    pub keychain_unavailable: bool,
}

impl LiveLogin {
    pub fn identity(&self) -> Option<Identity> {
        self.oauth_account.as_ref()?.identity()
    }
}

pub struct ClaudeLive<'a> {
    paths: &'a Paths,
    keychain: Keychain<'a>,
    /// Sticky for the process: one Keychain failure routes every later call
    /// to the file so a command never splits between backends.
    file_mode: Cell<bool>,
    account: String,
}

/// A stored credential covers the login only when it holds a login; a file
/// left with just sibling keys (such as `mcpOAuth`) does not.
fn covering(credential: ClaudeCredential) -> Option<ClaudeCredential> {
    (credential.kind() != CredentialKind::Unknown).then_some(credential)
}

fn read_text_if_present(path: &std::path::Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(fsutil::io_error(CcswError::CredentialRead, path, &err)),
    }
}

impl<'a> ClaudeLive<'a> {
    pub fn new(paths: &'a Paths, cli: &'a dyn SecurityCli) -> Self {
        Self {
            paths,
            keychain: Keychain::new(cli),
            file_mode: Cell::new(!paths.keychain_enabled),
            account: account_name(),
        }
    }

    fn use_keychain(&self) -> bool {
        !self.file_mode.get()
    }

    fn pin_file_mode(&self) {
        self.file_mode.set(true);
    }

    /// Keychain (when usable) → `.credentials.json` → managed key (Keychain
    /// `Claude Code`, then `primaryApiKey`), plus `oauthAccount`.
    pub fn read(&self) -> Result<LiveLogin> {
        let mut keychain_unavailable = false;
        let mut credential = None;
        let mut raw: Option<ClaudeCredential> = None;
        if self.use_keychain() {
            match self.keychain.get_password(LIVE_SERVICE, &self.account) {
                Ok(Some(text)) => {
                    let parsed = ClaudeCredential::parse(&text)?;
                    raw = Some(parsed.clone());
                    credential = covering(parsed);
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!("Claude Keychain read failed, using the file backend: {err}");
                    keychain_unavailable = true;
                    self.pin_file_mode();
                }
            }
        } else if self.paths.keychain_enabled {
            // Pinned after an earlier failure: an empty slot below is the
            // Keychain's fault, not a logged-out machine.
            keychain_unavailable = true;
        }
        if credential.is_none()
            && let Some(text) = read_text_if_present(&self.paths.claude_credentials_file())?
            && !text.trim().is_empty()
        {
            let parsed = ClaudeCredential::parse(&text)?;
            if raw.is_none() || parsed.kind() != CredentialKind::Unknown {
                raw = Some(parsed.clone());
            }
            credential = covering(parsed);
        }
        if credential.is_none() {
            let (key, failed) = self.read_managed_key()?;
            keychain_unavailable |= failed;
            credential = key.map(|key| ClaudeCredential::managed_key(&key));
        }
        let oauth_account = self
            .read_global_config()?
            .and_then(|config| {
                config
                    .get(OAUTH_ACCOUNT_KEY)
                    .filter(|v| v.is_object())
                    .cloned()
            })
            .map(OauthAccount);
        Ok(LiveLogin {
            keychain_unavailable: keychain_unavailable && credential.is_none(),
            credential,
            raw,
            oauth_account,
        })
    }

    /// The managed key, and whether the Keychain failed while looking for it.
    fn read_managed_key(&self) -> Result<(Option<String>, bool)> {
        let mut failed = false;
        if self.use_keychain() {
            match self.keychain.get_password(MANAGED_SERVICE, &self.account) {
                Ok(Some(key)) if !key.trim().is_empty() => return Ok((Some(key), false)),
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!("Claude Keychain read failed, using the file backend: {err}");
                    failed = true;
                    self.pin_file_mode();
                }
            }
        }
        let key = self.read_global_config()?.and_then(|config| {
            config
                .get(MANAGED_KEY)
                .and_then(Value::as_str)
                .filter(|k| !k.trim().is_empty())
                .map(str::to_string)
        });
        Ok((key, failed))
    }

    /// `~/.claude.json` (or the legacy file) as an object; `None` when absent.
    pub fn read_global_config(&self) -> Result<Option<Value>> {
        let path = self.paths.claude_global_config_file();
        let value = fsutil::read_json(&path).map_err(|err| {
            CcswError::config(format!("{} could not be read ({err})", path.display()))
        })?;
        match value {
            Some(value) if !value.is_object() => Err(CcswError::config(format!(
                "{} does not hold a JSON object",
                path.display()
            ))),
            other => Ok(other),
        }
    }

    /// Read-modify-write of the global config preserving every other key;
    /// creates the file when absent. Atomic, 0600.
    pub fn update_global_config(&self, mutate: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
        let path = self.paths.claude_global_config_file();
        let mut value = self.read_global_config()?.unwrap_or_else(|| json!({}));
        mutate(value.as_object_mut().expect("object checked on read"));
        fsutil::write_json_private(&path, &value)
            .map_err(|err| fsutil::io_error(CcswError::CredentialWrite, &path, &err))
    }

    /// Write the OAuth login where Claude Code reads it (spec §7.2c): the
    /// Keychain when usable (an already-present file is rewritten, never
    /// created), else the file (and a stale Keychain item is dropped).
    pub fn write_oauth(&self, live: &ClaudeCredential) -> Result<Backend> {
        let text = serde_json::to_string(&live.0)
            .map_err(|err| CcswError::credential_write(format!("invalid credential: {err}")))?;
        let file = self.paths.claude_credentials_file();
        if self.use_keychain() {
            match self
                .keychain
                .set_password(LIVE_SERVICE, &self.account, &text)
            {
                Ok(()) => {
                    if file.exists()
                        && let Err(err) = fsutil::atomic_write_private(&file, text.as_bytes())
                    {
                        tracing::warn!(
                            "could not refresh {} after the Keychain write: {err}",
                            file.display()
                        );
                    }
                    self.clear_managed_key()?;
                    return Ok(Backend::Keychain);
                }
                Err(err) => {
                    tracing::warn!("Claude Keychain write failed, falling back to the file: {err}");
                    self.pin_file_mode();
                }
            }
        }
        fsutil::atomic_write_private(&file, text.as_bytes())
            .map_err(|err| fsutil::io_error(CcswError::CredentialWrite, &file, &err))?;
        if self.paths.keychain_enabled {
            let _ = self.keychain.delete_password(LIVE_SERVICE, &self.account);
        }
        self.clear_managed_key()?;
        Ok(Backend::File)
    }

    /// Write a managed API key (Keychain `Claude Code`, else `primaryApiKey`)
    /// and clear any OAuth login.
    pub fn write_managed_key(&self, key: &str) -> Result<Backend> {
        let backend = if self.use_keychain()
            && self
                .keychain
                .set_password(MANAGED_SERVICE, &self.account, key)
                .is_ok()
        {
            Backend::Keychain
        } else {
            self.pin_file_mode();
            self.update_global_config(|config| {
                config.insert(MANAGED_KEY.to_string(), json!(key));
            })?;
            Backend::File
        };
        self.clear_oauth()?;
        Ok(backend)
    }

    fn clear_managed_key(&self) -> Result<()> {
        if self.paths.keychain_enabled {
            let _ = self
                .keychain
                .delete_password(MANAGED_SERVICE, &self.account);
        }
        if self
            .read_global_config()?
            .is_some_and(|config| config.get(MANAGED_KEY).is_some())
        {
            self.update_global_config(|config| {
                config.remove(MANAGED_KEY);
            })?;
        }
        Ok(())
    }

    /// Drop `claudeAiOauth` from the Keychain item and the file, keeping
    /// every sibling (such as `mcpOAuth`, spec §4); an item or file left
    /// with nothing else is removed.
    fn clear_oauth(&self) -> Result<()> {
        if self.paths.keychain_enabled {
            self.clear_keychain_oauth();
        }
        let file = self.paths.claude_credentials_file();
        if let Some(text) = read_text_if_present(&file)?
            && let Ok(mut credential) = ClaudeCredential::parse(&text)
            && credential.0.get(OAUTH_KEY).is_some()
        {
            let object = credential.0.as_object_mut().expect("object");
            object.remove(OAUTH_KEY);
            if object.is_empty() {
                std::fs::remove_file(&file)
                    .map_err(|err| fsutil::io_error(CcswError::CredentialWrite, &file, &err))?;
            } else {
                fsutil::write_json_private(&file, &credential.0)
                    .map_err(|err| fsutil::io_error(CcswError::CredentialWrite, &file, &err))?;
            }
        }
        Ok(())
    }

    /// Best effort: a Keychain failure here leaves a login the managed key
    /// already outranks, so it is logged, not raised.
    fn clear_keychain_oauth(&self) {
        let text = match self.keychain.get_password(LIVE_SERVICE, &self.account) {
            Ok(Some(text)) => text,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!("could not read the Claude Keychain item to clear its login: {err}");
                return;
            }
        };
        let remaining = match ClaudeCredential::parse(&text) {
            Ok(mut credential) => {
                let object = credential.0.as_object_mut().expect("object");
                if object.remove(OAUTH_KEY).is_none() {
                    return;
                }
                (!object.is_empty()).then_some(credential)
            }
            // Nothing in an unreadable item can be kept.
            Err(_) => None,
        };
        match remaining {
            Some(credential) => {
                let written = serde_json::to_string(&credential.0)
                    .map_err(|err| err.to_string())
                    .and_then(|text| {
                        self.keychain
                            .set_password(LIVE_SERVICE, &self.account, &text)
                            .map_err(|err| err.to_string())
                    });
                if let Err(err) = written {
                    tracing::warn!(
                        "could not clear the login from the Claude Keychain item: {err}"
                    );
                }
            }
            None => match self.keychain.delete_password(LIVE_SERVICE, &self.account) {
                Ok(true) => {}
                Ok(false) => tracing::warn!("the Claude Keychain item was already gone"),
                Err(err) => tracing::warn!("could not delete the Claude Keychain item: {err}"),
            },
        }
    }
}

/// `backups/claude/<unix_nanos>.json` = `{"credentials": …, "oauthAccount": …}`,
/// three kept. Nothing to back up is not an error.
pub fn backup_live(paths: &Paths, login: &LiveLogin) -> Result<()> {
    // A managed key is the login itself; otherwise keep the whole object
    // read, siblings included.
    let saved = match login.credential.as_ref() {
        Some(c) if c.kind() == CredentialKind::ApiKey => Some(c),
        credential => login.raw.as_ref().or(credential),
    };
    if saved.is_none() && login.oauth_account.is_none() {
        return Ok(());
    }
    let dir = paths.claude_backups_dir();
    ensure_private_dir(&dir)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CcswError::credential_write("system clock is before the Unix epoch"))?
        .as_nanos();
    let path = (0..1000u16)
        .map(|n| {
            if n == 0 {
                dir.join(format!("{nanos}.json"))
            } else {
                dir.join(format!("{nanos}-{n}.json"))
            }
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| CcswError::credential_write("could not allocate a Claude backup path"))?;
    let value = json!({
        "credentials": saved.map(|c| c.0.clone()).unwrap_or(Value::Null),
        "oauthAccount": login.oauth_account.as_ref().map(|a| a.0.clone()).unwrap_or(Value::Null),
    });
    fsutil::write_json_private(&path, &value)
        .map_err(|err| fsutil::io_error(CcswError::CredentialWrite, &path, &err))?;
    let mut backups: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    if backups.len() > MAX_BACKUPS {
        backups.sort();
        for old in &backups[..backups.len() - MAX_BACKUPS] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::keychain::CliOutput;
    use crate::store::temp_store;
    use serde_json::json;
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::io;

    /// A Keychain that stores items in memory and can be told to fail.
    #[derive(Default)]
    struct FakeKeychain {
        items: RefCell<BTreeMap<(String, String), String>>,
        failures: RefCell<VecDeque<bool>>,
        calls: std::cell::Cell<usize>,
    }

    impl FakeKeychain {
        fn fail_next(&self) {
            self.failures.borrow_mut().push_back(true);
        }
        /// Let the next call through, so the one after it can be failed.
        fn pass_next(&self) {
            self.failures.borrow_mut().push_back(false);
        }
        fn item(&self, service: &str) -> Option<String> {
            self.items
                .borrow()
                .get(&(service.to_string(), crate::claude::keychain::account_name()))
                .cloned()
        }
    }

    impl SecurityCli for FakeKeychain {
        fn run(&self, args: &[String], stdin_line: Option<&str>) -> io::Result<CliOutput> {
            self.calls.set(self.calls.get() + 1);
            if self.failures.borrow_mut().pop_front() == Some(true) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "locked"));
            }
            let ok = |stdout: &str| {
                Ok(CliOutput {
                    status: 0,
                    stdout: stdout.to_string(),
                })
            };
            let missing = || {
                Ok(CliOutput {
                    status: 44,
                    stdout: String::new(),
                })
            };
            let mut items = self.items.borrow_mut();
            match args[0].as_str() {
                "find-generic-password" => {
                    let key = (args[args.len() - 1].clone(), args[2].clone());
                    match items.get(&key) {
                        Some(v) => ok(v),
                        None => missing(),
                    }
                }
                "delete-generic-password" => {
                    let key = (args[4].clone(), args[2].clone());
                    if items.remove(&key).is_some() {
                        ok("")
                    } else {
                        missing()
                    }
                }
                "-i" => {
                    let line = stdin_line.unwrap();
                    // The service name may contain a space, so cut on the flags.
                    let (head, hex_value) = line.rsplit_once(" -X ").unwrap();
                    let (head, service) = head.split_once(" -s ").unwrap();
                    let account = head.split_once(" -a ").unwrap().1;
                    let account = account.trim_matches('"').to_string();
                    let service = service.trim_matches('"').to_string();
                    let value = String::from_utf8(hex::decode(hex_value).unwrap()).unwrap();
                    items.insert((service, account), value);
                    ok("")
                }
                _ => ok(""),
            }
        }
    }

    fn creds(access: &str, refresh: &str) -> ClaudeCredential {
        ClaudeCredential::from_value(json!({
            OAUTH_KEY: {"accessToken": access, "refreshToken": refresh, "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]},
            "mcpOAuth": {"srv": {"accessToken": "m"}}
        }))
    }

    fn write_file(paths: &crate::paths::Paths, value: &serde_json::Value) {
        std::fs::create_dir_all(&paths.claude_home).unwrap();
        std::fs::write(paths.claude_credentials_file(), value.to_string()).unwrap();
    }

    fn write_config(paths: &crate::paths::Paths, value: &serde_json::Value) {
        std::fs::create_dir_all(paths.claude_global_config_file().parent().unwrap()).unwrap();
        std::fs::write(paths.claude_global_config_file(), value.to_string()).unwrap();
    }

    #[test]
    fn file_backend_reads_credentials_then_managed_key_and_the_account() {
        let (_dir, store) = temp_store();
        let fake = FakeKeychain::default();
        let live = ClaudeLive::new(&store.paths, &fake);
        let empty = live.read().unwrap();
        assert!(empty.credential.is_none() && empty.oauth_account.is_none());
        assert!(!empty.keychain_unavailable);

        write_config(
            &store.paths,
            &json!({"oauthAccount": {"emailAddress": "A@x.io", "organizationUuid": "org"}, "projects": {"/p": {}}}),
        );
        write_file(&store.paths, &creds("at", "rt").0);
        let login = live.read().unwrap();
        assert_eq!(
            login.credential.as_ref().unwrap().access_token(),
            Some("at")
        );
        assert_eq!(
            login.identity(),
            Some(crate::model::Identity::new("a@x.io", "org"))
        );

        std::fs::remove_file(store.paths.claude_credentials_file()).unwrap();
        write_config(
            &store.paths,
            &json!({"primaryApiKey": "sk-ant-api03-k", "oauthAccount": {"emailAddress": "A@x.io"}}),
        );
        let login = live.read().unwrap();
        assert_eq!(
            login.credential.as_ref().unwrap().api_key(),
            Some("sk-ant-api03-k")
        );
        assert!(
            fake.items.borrow().is_empty(),
            "the file backend never touches the Keychain"
        );
    }

    #[test]
    fn keychain_backend_wins_over_the_file_and_degrades_stickily() {
        let (_dir, store) = temp_store();
        let mut paths = store.paths.clone();
        paths.keychain_enabled = true;
        let fake = FakeKeychain::default();
        write_file(&paths, &creds("file", "rt-file").0);
        let live = ClaudeLive::new(&paths, &fake);
        assert_eq!(
            live.read().unwrap().credential.unwrap().access_token(),
            Some("file")
        );

        live.write_oauth(&creds("kc", "rt-kc")).unwrap();
        assert_eq!(
            live.read().unwrap().credential.unwrap().access_token(),
            Some("kc")
        );
        assert!(fake.item(LIVE_SERVICE).unwrap().contains("rt-kc"));
        let file: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(paths.claude_credentials_file()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            file[OAUTH_KEY]["refreshToken"], "rt-kc",
            "an existing file is rewritten"
        );

        fake.fail_next();
        let degraded = live.read().unwrap();
        assert!(!degraded.keychain_unavailable, "the file covered the login");
        assert_eq!(
            degraded.credential.unwrap().access_token(),
            Some("kc"),
            "the file still answers"
        );
        assert_eq!(
            live.read().unwrap().credential.unwrap().access_token(),
            Some("kc")
        );
        assert!(
            !live.read().unwrap().keychain_unavailable,
            "sticky file mode after a failure: no second probe"
        );
        let backend = live.write_oauth(&creds("after", "rt-after")).unwrap();
        assert_eq!(backend, Backend::File);
        assert_eq!(
            fake.item(LIVE_SERVICE),
            None,
            "a stale Keychain item is deleted"
        );
    }

    #[test]
    fn keychain_write_never_creates_the_file_and_failure_falls_back() {
        let (_dir, store) = temp_store();
        let mut paths = store.paths.clone();
        paths.keychain_enabled = true;
        let fake = FakeKeychain::default();
        let live = ClaudeLive::new(&paths, &fake);
        assert_eq!(
            live.write_oauth(&creds("a", "rt")).unwrap(),
            Backend::Keychain
        );
        assert!(!paths.claude_credentials_file().exists());
        let fake2 = FakeKeychain::default();
        fake2.fail_next();
        let live2 = ClaudeLive::new(&paths, &fake2);
        assert_eq!(
            live2.write_oauth(&creds("b", "rt-b")).unwrap(),
            Backend::File
        );
        assert!(paths.claude_credentials_file().exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.claude_credentials_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let covered = live2.read().unwrap();
        assert!(!covered.keychain_unavailable, "the file covered the login");
        std::fs::remove_file(paths.claude_credentials_file()).unwrap();
        let unavailable = live2.read().unwrap();
        assert!(unavailable.credential.is_none());
        assert!(
            unavailable.keychain_unavailable,
            "pinned by the write failure and nothing on disk"
        );
    }

    #[test]
    fn a_failing_keychain_with_nothing_on_disk_is_unavailable_and_sticky() {
        let (_dir, store) = temp_store();
        let mut paths = store.paths.clone();
        paths.keychain_enabled = true;
        let fake = FakeKeychain::default();
        fake.fail_next();
        let live = ClaudeLive::new(&paths, &fake);
        let first = live.read().unwrap();
        assert!(first.credential.is_none() && first.keychain_unavailable);
        let calls = fake.calls.get();
        let second = live.read().unwrap();
        assert!(second.credential.is_none() && second.keychain_unavailable);
        assert_eq!(
            fake.calls.get(),
            calls,
            "pinned: security is not invoked again"
        );
    }

    #[test]
    fn a_managed_service_failure_with_nothing_on_disk_is_unavailable() {
        let (_dir, store) = temp_store();
        let mut paths = store.paths.clone();
        paths.keychain_enabled = true;
        let fake = FakeKeychain::default();
        fake.pass_next(); // `Claude Code-credentials`: not found
        fake.fail_next(); // `Claude Code`: errors
        let login = ClaudeLive::new(&paths, &fake).read().unwrap();
        assert!(login.credential.is_none());
        assert!(login.keychain_unavailable);
    }

    #[test]
    fn managed_key_write_clears_oauth_and_vice_versa() {
        let (_dir, store) = temp_store();
        let fake = FakeKeychain::default();
        let live = ClaudeLive::new(&store.paths, &fake);
        write_config(
            &store.paths,
            &json!({"projects": {"/p": {"allowedTools": []}}}),
        );
        live.write_oauth(&creds("a", "rt")).unwrap();
        assert_eq!(
            live.write_managed_key("sk-ant-api03-k").unwrap(),
            Backend::File
        );
        let config = live.read_global_config().unwrap().unwrap();
        assert_eq!(config["primaryApiKey"], "sk-ant-api03-k");
        assert_eq!(
            config["projects"]["/p"]["allowedTools"],
            json!([]),
            "other keys survive"
        );
        let file: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(store.paths.claude_credentials_file()).unwrap(),
        )
        .unwrap();
        assert!(file.get(OAUTH_KEY).is_none(), "the OAuth login is cleared");
        assert_eq!(file["mcpOAuth"]["srv"]["accessToken"], "m", "siblings stay");
        let after_key = live.read().unwrap().credential.unwrap();
        assert_eq!(after_key.kind(), CredentialKind::ApiKey);
        assert_eq!(after_key.api_key(), Some("sk-ant-api03-k"));
        live.write_oauth(&creds("b", "rt-b")).unwrap();
        assert!(
            live.read_global_config()
                .unwrap()
                .unwrap()
                .get("primaryApiKey")
                .is_none()
        );
        assert_eq!(
            live.read().unwrap().credential.unwrap().access_token(),
            Some("b")
        );
    }

    #[test]
    fn keychain_round_trip_through_a_managed_key_keeps_siblings() {
        let (_dir, store) = temp_store();
        let mut paths = store.paths.clone();
        paths.keychain_enabled = true;
        let fake = FakeKeychain::default();
        let live = ClaudeLive::new(&paths, &fake);
        assert_eq!(
            live.write_oauth(&creds("a", "rt-a")).unwrap(),
            Backend::Keychain
        );
        let item = |fake: &FakeKeychain| -> serde_json::Value {
            serde_json::from_str(&fake.item(LIVE_SERVICE).expect("the item survives")).unwrap()
        };
        let mcp_before = serde_json::to_string(&item(&fake)["mcpOAuth"]).unwrap();

        assert_eq!(
            live.write_managed_key("sk-ant-api03-k").unwrap(),
            Backend::Keychain
        );
        let after_key = item(&fake);
        assert!(
            after_key.get(OAUTH_KEY).is_none(),
            "the OAuth login is cleared"
        );
        assert_eq!(
            serde_json::to_string(&after_key["mcpOAuth"]).unwrap(),
            mcp_before
        );
        assert!(!paths.claude_credentials_file().exists());

        // Back to OAuth the way a switch does it: the read item, OAuth replaced.
        let login = live.read().unwrap();
        assert_eq!(
            login.credential.as_ref().unwrap().kind(),
            CredentialKind::ApiKey
        );
        let mut object = login.raw.clone().expect("the sibling item is read");
        object.replace_oauth_from(&creds("b", "rt-b"));
        assert_eq!(live.write_oauth(&object).unwrap(), Backend::Keychain);
        let restored = item(&fake);
        assert_eq!(restored[OAUTH_KEY]["refreshToken"], "rt-b");
        assert_eq!(
            serde_json::to_string(&restored["mcpOAuth"]).unwrap(),
            mcp_before,
            "byte-identical siblings"
        );
        assert_eq!(
            fake.item(MANAGED_SERVICE),
            None,
            "the managed key is cleared"
        );
    }

    #[test]
    fn clearing_an_oauth_only_login_removes_the_item_and_the_file() {
        let (_dir, store) = temp_store();
        let mut paths = store.paths.clone();
        paths.keychain_enabled = true;
        let fake = FakeKeychain::default();
        let live = ClaudeLive::new(&paths, &fake);
        let oauth_only = ClaudeCredential::from_value(json!({
            OAUTH_KEY: {"accessToken": "a", "refreshToken": "rt", "expiresAt": 4_102_444_800_000i64}
        }));
        write_file(&paths, &oauth_only.0);
        live.write_oauth(&oauth_only).unwrap();
        live.write_managed_key("sk-ant-api03-k").unwrap();
        assert_eq!(fake.item(LIVE_SERVICE), None, "nothing else remained");
        assert!(
            !paths.claude_credentials_file().exists(),
            "nothing else remained"
        );
    }

    #[test]
    fn update_global_config_preserves_everything_else() {
        let (_dir, store) = temp_store();
        let fake = FakeKeychain::default();
        let live = ClaudeLive::new(&store.paths, &fake);
        live.update_global_config(|cfg| {
            cfg.insert("oauthAccount".into(), json!({"emailAddress": "a@x.io"}));
        })
        .unwrap();
        write_config(
            &store.paths,
            &json!({"oauthAccount": {"emailAddress": "old"}, "mcpServers": {"x": 1}, "numStartups": 7}),
        );
        live.update_global_config(|cfg| {
            cfg.insert("oauthAccount".into(), json!({"emailAddress": "new"}));
        })
        .unwrap();
        let config = live.read_global_config().unwrap().unwrap();
        assert_eq!(config["oauthAccount"]["emailAddress"], "new");
        assert_eq!(config["mcpServers"]["x"], 1);
        assert_eq!(config["numStartups"], 7);
        std::fs::write(store.paths.claude_global_config_file(), "[1]").unwrap();
        assert_eq!(
            live.read_global_config().unwrap_err().type_name(),
            "ConfigError"
        );
    }

    #[test]
    fn backups_keep_three() {
        let (_dir, store) = temp_store();
        let login = LiveLogin {
            credential: Some(creds("a", "rt")),
            raw: None,
            oauth_account: Some(OauthAccount::synthesized("a@x.io")),
            keychain_unavailable: false,
        };
        for _ in 0..5 {
            backup_live(&store.paths, &login).unwrap();
        }
        let mut names: Vec<String> = std::fs::read_dir(store.paths.claude_backups_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 3);
        let newest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(store.paths.claude_backups_dir().join(names.last().unwrap()))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(newest["credentials"][OAUTH_KEY]["refreshToken"], "rt");
        assert_eq!(newest["oauthAccount"]["emailAddress"], "a@x.io");
        let empty = LiveLogin {
            credential: None,
            raw: None,
            oauth_account: None,
            keychain_unavailable: false,
        };
        backup_live(&store.paths, &empty).unwrap();
        assert_eq!(
            std::fs::read_dir(store.paths.claude_backups_dir())
                .unwrap()
                .count(),
            3,
            "nothing to back up"
        );
    }
}
