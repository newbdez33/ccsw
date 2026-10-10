//! Read a claude-swap store directly, as the shape of a cswap export, so
//! `import --from-cswap` can reuse the bundle importer (ccsw-engine spec,
//! "ccsw side"). Layout from claude-swap 0.25.0: `sequence.json` holds the
//! roster; per-account credentials are a base64 `.enc` file
//! (`credentials/.creds-{n}-{email}.enc`, the only backend off macOS) or,
//! on macOS, a Keychain item (service `claude-swap`, account
//! `account-{n}-{email}`) — reads are `.enc`-wins, as in claude-swap;
//! `configs/.claude-config-{n}-{email}.json` is the `.claude.json`
//! snapshot the export's `config` field carries.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::claude::keychain::{Keychain, SecurityCli};
use crate::errors::{CcswError, Result};
use crate::model::now_iso;
use crate::paths::user_home;
use crate::transfer::platform_name;

pub const KEYCHAIN_SERVICE: &str = "claude-swap";
const ROSTER: &str = "sequence.json";
const RETIRED_MARK: &str = ".migrated-";

/// Where claude-swap kept its store on this platform (`paths.py` in
/// claude-swap: macOS/Windows `~/.claude-swap-backup`, Linux/WSL
/// `$XDG_DATA_HOME/claude-swap`).
pub fn default_dir() -> Option<PathBuf> {
    let home = user_home()?;
    if cfg!(target_os = "linux") {
        let xdg = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        Some(xdg.join("claude-swap"))
    } else {
        Some(home.join(".claude-swap-backup"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// No directory (or no `sequence.json`) at the path.
    Missing,
    /// The directory is gone but a `<dir>.migrated-*` sibling exists.
    Retired,
    /// A roster with no accounts.
    Empty,
    Ready {
        accounts: usize,
    },
}

pub fn probe(dir: &Path) -> Result<Probe> {
    if !dir.join(ROSTER).is_file() {
        return Ok(if retired_sibling_exists(dir) {
            Probe::Retired
        } else {
            Probe::Missing
        });
    }
    let roster = read_roster(dir)?;
    let count = roster_accounts(&roster).len();
    Ok(if count == 0 {
        Probe::Empty
    } else {
        Probe::Ready { accounts: count }
    })
}

fn retired_sibling_exists(dir: &Path) -> bool {
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        return false;
    };
    let prefix = format!("{}{}", name.to_string_lossy(), RETIRED_MARK);
    fs::read_dir(parent)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        })
        .unwrap_or(false)
}

fn read_roster(dir: &Path) -> Result<Map<String, Value>> {
    let path = dir.join(ROSTER);
    let text = fs::read_to_string(&path)
        .map_err(|err| CcswError::transfer(format!("could not read {}: {err}", path.display())))?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(CcswError::transfer(format!(
            "{} is not a JSON object",
            path.display()
        ))),
        Err(err) => Err(CcswError::transfer(format!(
            "{} is not valid JSON: {err}",
            path.display()
        ))),
    }
}

/// `(slot number, record)` pairs in ascending slot order.
fn roster_accounts(roster: &Map<String, Value>) -> Vec<(u32, Map<String, Value>)> {
    let mut out: Vec<(u32, Map<String, Value>)> = roster
        .get("accounts")
        .and_then(Value::as_object)
        .map(|accounts| {
            accounts
                .iter()
                .filter_map(|(slot, record)| {
                    Some((slot.parse::<u32>().ok()?, record.as_object()?.clone()))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|(slot, _)| *slot);
    out
}

pub struct StoreEnvelope {
    /// A cswap export (`version: 1`, `swapVersion`, `accounts[]`) ready for
    /// `transfer::import_accounts`.
    pub envelope: Value,
    /// Skipped-slot notices, in slot order.
    pub notices: Vec<String>,
    /// Accounts that made it into the envelope.
    pub readable: usize,
    /// Roster accounts left out: no email, or no readable credentials.
    pub skipped: usize,
}

/// Build the envelope. `use_keychain` is `paths.keychain_enabled` (macOS
/// with `CCSW_KEYCHAIN` not off): the `.enc` file wins, the Keychain item
/// is read only when the file is absent or corrupt; off macOS only the
/// file is read.
pub fn envelope(
    dir: &Path,
    security: &dyn SecurityCli,
    use_keychain: bool,
) -> Result<StoreEnvelope> {
    let roster = read_roster(dir)?;
    let keychain = Keychain::new(security);
    let mut accounts = Vec::new();
    let mut notices = Vec::new();
    let listed = roster_accounts(&roster);
    let total = listed.len();
    for (slot, record) in listed {
        let email = record
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if email.is_empty() {
            notices.push(format!("Skipping Account-{slot}: no email in {ROSTER}"));
            continue;
        }
        let credentials = match read_credentials(dir, &keychain, use_keychain, slot, &email) {
            Ok(Some(value)) => value,
            Ok(None) => {
                notices.push(format!(
                    "Skipping Account-{slot} ({email}): no stored credentials — add it again with: ccsw add claude"
                ));
                continue;
            }
            Err(reason) => {
                notices.push(format!("Skipping Account-{slot} ({email}): {reason}"));
                continue;
            }
        };
        let mut entry = Map::new();
        entry.insert("number".into(), json!(slot));
        entry.insert("email".into(), json!(email));
        for key in ["organizationUuid", "organizationName", "uuid", "added"] {
            if let Some(value) = record.get(key).filter(|v| v.is_string()) {
                entry.insert(key.into(), value.clone());
            }
        }
        entry.insert("credentials".into(), credentials);
        if let Some(config) = read_config(dir, slot, &email) {
            entry.insert("config".into(), config);
        }
        accounts.push(Value::Object(entry));
    }
    let readable = accounts.len();
    let skipped = total - readable;
    let envelope = json!({
        "version": 1,
        "swapVersion": "store",
        "exportedFrom": platform_name(),
        "exportedAt": now_iso(),
        "encrypted": false,
        "activeAccountNumber": roster.get("activeAccountNumber").cloned().unwrap_or(Value::Null),
        "accounts": accounts,
    });
    Ok(StoreEnvelope {
        envelope,
        notices,
        readable,
        skipped,
    })
}

/// The `.enc` file first, the Keychain only when the file is absent or
/// corrupt — claude-swap's own `.enc`-wins rule: a file beside a Keychain
/// item was written while the Keychain was unusable and holds the newer
/// refresh token. `Ok(None)` = no credential anywhere; `Err(reason)` = a
/// copy exists but is unreadable.
fn read_credentials(
    dir: &Path,
    keychain: &Keychain<'_>,
    use_keychain: bool,
    slot: u32,
    email: &str,
) -> std::result::Result<Option<Value>, String> {
    let file_problem = match read_enc(dir, slot, email) {
        Ok(Some(value)) => return Ok(Some(value)),
        Ok(None) => None,
        Err(reason) => Some(reason),
    };
    if use_keychain {
        let account = format!("account-{slot}-{email}");
        match keychain.get_password(KEYCHAIN_SERVICE, &account) {
            Ok(Some(text)) => {
                return serde_json::from_str::<Value>(&text)
                    .map(Some)
                    .map_err(|err| format!("Keychain item is not JSON: {err}"));
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(slot, error = %err, "claude-swap Keychain item unreadable");
            }
        }
    }
    match file_problem {
        Some(reason) => Err(reason),
        None => Ok(None),
    }
}

/// `credentials/.creds-{slot}-{email}.enc`: base64 of the credentials JSON.
fn read_enc(dir: &Path, slot: u32, email: &str) -> std::result::Result<Option<Value>, String> {
    let path = dir
        .join("credentials")
        .join(format!(".creds-{slot}-{email}.enc"));
    let encoded = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("could not read {}: {err}", path.display())),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|err| format!("{} is not base64: {err}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .map(Some)
        .map_err(|err| format!("{} does not hold JSON credentials: {err}", path.display()))
}

/// The `.claude.json` snapshot claude-swap kept per account, when present
/// and a JSON object.
fn read_config(dir: &Path, slot: u32, email: &str) -> Option<Value> {
    let path = dir
        .join("configs")
        .join(format!(".claude-config-{slot}-{email}.json"));
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
}

/// Rename the store to `<dir>.migrated-<YYYYMMDD-HHMMSS>` so a leftover
/// cswap finds no accounts. Reversible by renaming back.
pub fn retire(dir: &Path) -> Result<PathBuf> {
    let stamp = now_iso()
        .replace(['-', ':'], "")
        .replace('T', "-")
        .trim_end_matches('Z')
        .to_string();
    let mut name = dir
        .file_name()
        .ok_or_else(|| CcswError::transfer("store path has no directory name"))?
        .to_os_string();
    name.push(format!("{RETIRED_MARK}{stamp}"));
    let target = dir.with_file_name(name);
    fs::rename(dir, &target).map_err(|err| {
        CcswError::transfer(format!(
            "could not retire {} → {}: {err}",
            dir.display(),
            target.display()
        ))
    })?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::keychain::CliOutput;
    use std::cell::RefCell;

    /// Records `security` calls; `reply` scripts the next stdout, `fail`
    /// makes the next call exit 36 (Keychain locked). With no scripted
    /// reply the call answers 44 (item not found).
    struct Fake {
        replies: RefCell<Vec<CliOutput>>,
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                replies: RefCell::new(Vec::new()),
                calls: RefCell::new(Vec::new()),
            }
        }
        fn reply(&self, stdout: &str) {
            self.replies.borrow_mut().push(CliOutput {
                status: 0,
                stdout: stdout.to_string(),
            });
        }
        fn fail(&self) {
            self.replies.borrow_mut().push(CliOutput {
                status: 36,
                stdout: String::new(),
            });
        }
    }
    impl SecurityCli for Fake {
        fn run(&self, args: &[String], _stdin: Option<&str>) -> std::io::Result<CliOutput> {
            self.calls.borrow_mut().push(args.to_vec());
            let mut replies = self.replies.borrow_mut();
            Ok(if replies.is_empty() {
                CliOutput {
                    status: 44,
                    stdout: String::new(),
                }
            } else {
                replies.remove(0)
            })
        }
    }

    fn creds(refresh: &str) -> serde_json::Value {
        json!({"claudeAiOauth": {"accessToken": "at", "refreshToken": refresh,
               "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]}})
    }

    /// A store with two accounts: slot 1 (alice) and slot 2 (bob), both
    /// as base64 `.enc` files; alice also has a config backup.
    fn store(dir: &Path) {
        fs::create_dir_all(dir.join("credentials")).unwrap();
        fs::create_dir_all(dir.join("configs")).unwrap();
        fs::write(
            dir.join("sequence.json"),
            json!({
                "accounts": {
                    "1": {"email": "alice@example.com", "organizationUuid": "org-a",
                          "organizationName": "Acme", "uuid": "u-a", "added": "2026-01-01T00:00:00Z"},
                    "2": {"email": "Bob+x@Example.com", "organizationUuid": "",
                          "organizationName": "", "uuid": "u-b", "added": "2026-01-02T00:00:00Z"}
                },
                "activeAccountNumber": 1
            })
            .to_string(),
        )
        .unwrap();
        let b64 =
            |v: &serde_json::Value| base64::engine::general_purpose::STANDARD.encode(v.to_string());
        fs::write(
            dir.join("credentials/.creds-1-alice@example.com.enc"),
            b64(&creds("crt-a")),
        )
        .unwrap();
        fs::write(
            dir.join("credentials/.creds-2-Bob+x@Example.com.enc"),
            b64(&creds("crt-b")),
        )
        .unwrap();
        fs::write(
            dir.join("configs/.claude-config-1-alice@example.com.json"),
            json!({"oauthAccount": {"emailAddress": "alice@example.com", "organizationUuid": "org-a",
                   "organizationName": "Acme", "accountUuid": "u-a"}, "numStartups": 3})
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn probe_distinguishes_missing_retired_empty_and_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude-swap-backup");
        assert_eq!(probe(&dir).unwrap(), Probe::Missing);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("sequence.json"),
            r#"{"accounts": {}, "activeAccountNumber": null}"#,
        )
        .unwrap();
        assert_eq!(probe(&dir).unwrap(), Probe::Empty);
        store(&dir);
        assert_eq!(probe(&dir).unwrap(), Probe::Ready { accounts: 2 });
        let retired = retire(&dir).unwrap();
        assert!(
            retired
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("claude-swap-backup.migrated-")
        );
        assert_eq!(probe(&dir).unwrap(), Probe::Retired);
    }

    #[test]
    fn envelope_from_enc_files_matches_the_cswap_export_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        let fake = Fake::new();
        let out = envelope(&dir, &fake, false).unwrap();
        assert!(
            fake.calls.borrow().is_empty(),
            "file backend never calls security"
        );
        assert_eq!(out.readable, 2);
        assert_eq!(out.skipped, 0);
        assert!(out.notices.is_empty(), "{:?}", out.notices);
        let env = &out.envelope;
        assert_eq!(env["version"], 1);
        assert!(env["swapVersion"].is_string());
        assert_eq!(env["encrypted"], false);
        assert_eq!(env["activeAccountNumber"], 1);
        let accounts = env["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0]["number"], 1);
        assert_eq!(accounts[0]["email"], "alice@example.com");
        assert_eq!(accounts[0]["organizationUuid"], "org-a");
        assert_eq!(
            accounts[0]["credentials"]["claudeAiOauth"]["refreshToken"],
            "crt-a"
        );
        assert_eq!(
            accounts[0]["config"]["oauthAccount"]["organizationUuid"],
            "org-a"
        );
        assert!(
            accounts[1].get("config").is_none(),
            "no config backup → no config key"
        );
        assert_eq!(
            accounts[1]["credentials"]["claudeAiOauth"]["refreshToken"],
            "crt-b"
        );
    }

    #[test]
    fn file_and_keychain_names_use_the_raw_email() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        // No .enc files: both slots come from the Keychain.
        fs::remove_file(dir.join("credentials/.creds-1-alice@example.com.enc")).unwrap();
        fs::remove_file(dir.join("credentials/.creds-2-Bob+x@Example.com.enc")).unwrap();
        let fake = Fake::new();
        fake.reply(&creds("kc-a").to_string()); // slot 1 from the Keychain
        fake.reply(&creds("kc-b").to_string()); // slot 2 from the Keychain
        let out = envelope(&dir, &fake, true).unwrap();
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(
            calls[0].contains(&"account-1-alice@example.com".to_string()),
            "{:?}",
            calls[0]
        );
        assert!(calls[0].contains(&KEYCHAIN_SERVICE.to_string()));
        assert!(
            calls[1].contains(&"account-2-Bob+x@Example.com".to_string()),
            "raw email, not lowercased: {:?}",
            calls[1]
        );
        let accounts = out.envelope["accounts"].as_array().unwrap();
        assert_eq!(
            accounts[0]["credentials"]["claudeAiOauth"]["refreshToken"],
            "kc-a"
        );
        assert_eq!(
            accounts[1]["credentials"]["claudeAiOauth"]["refreshToken"],
            "kc-b"
        );
    }

    #[test]
    fn an_enc_file_wins_over_the_keychain_without_touching_it() {
        // claude-swap's reads are `.enc`-wins: a file beside a Keychain item
        // was written while the Keychain was unusable and holds the newer
        // refresh token, so the Keychain copy may be superseded.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        let fake = Fake::new();
        fake.reply(&creds("kc-a").to_string()); // must stay unread
        let out = envelope(&dir, &fake, true).unwrap();
        assert!(
            fake.calls.borrow().is_empty(),
            "the Keychain is not consulted while an .enc file exists"
        );
        let accounts = out.envelope["accounts"].as_array().unwrap();
        assert_eq!(
            accounts[0]["credentials"]["claudeAiOauth"]["refreshToken"],
            "crt-a"
        );
        assert_eq!(
            accounts[1]["credentials"]["claudeAiOauth"]["refreshToken"],
            "crt-b"
        );
        assert_eq!(out.readable, 2);
        assert_eq!(out.skipped, 0);
    }

    #[test]
    fn keychain_failure_without_an_enc_file_skips_the_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        fs::remove_file(dir.join("credentials/.creds-1-alice@example.com.enc")).unwrap();
        let fake = Fake::new();
        fake.fail(); // slot 1: Keychain locked, no file → skipped
        let out = envelope(&dir, &fake, true).unwrap();
        assert_eq!(out.readable, 1);
        assert_eq!(out.skipped, 1);
        assert_eq!(
            fake.calls.borrow().len(),
            1,
            "only slot 1 reached the Keychain"
        );
        assert_eq!(out.envelope["accounts"].as_array().unwrap()[0]["number"], 2);
        assert!(
            out.notices[0].contains("Skipping Account-1 (alice@example.com)"),
            "{}",
            out.notices[0]
        );
    }

    #[test]
    fn a_corrupt_enc_file_falls_back_to_the_keychain() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        fs::write(
            dir.join("credentials/.creds-2-Bob+x@Example.com.enc"),
            "%%not-base64%%",
        )
        .unwrap();
        let fake = Fake::new();
        fake.reply(&creds("kc-b").to_string()); // slot 2 from the Keychain
        let out = envelope(&dir, &fake, true).unwrap();
        assert_eq!(out.readable, 2);
        assert_eq!(out.skipped, 0);
        assert!(out.notices.is_empty(), "{:?}", out.notices);
        let accounts = out.envelope["accounts"].as_array().unwrap();
        assert_eq!(
            accounts[1]["credentials"]["claudeAiOauth"]["refreshToken"],
            "kc-b"
        );
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains(&"account-2-Bob+x@Example.com".to_string()));
    }

    #[test]
    fn a_corrupt_enc_file_skips_that_account_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        fs::write(
            dir.join("credentials/.creds-2-Bob+x@Example.com.enc"),
            "%%not-base64%%",
        )
        .unwrap();
        let out = envelope(&dir, &Fake::new(), false).unwrap();
        assert_eq!(out.readable, 1);
        assert_eq!(out.skipped, 1);
        let accounts = out.envelope["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["number"], 1);
        assert_eq!(out.notices.len(), 1);
        assert!(
            out.notices[0].contains("Skipping Account-2 (Bob+x@Example.com)"),
            "{}",
            out.notices[0]
        );
    }

    #[test]
    fn a_slot_with_no_credentials_anywhere_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        store(&dir);
        fs::remove_file(dir.join("credentials/.creds-1-alice@example.com.enc")).unwrap();
        let out = envelope(&dir, &Fake::new(), true).unwrap();
        assert_eq!(out.readable, 1);
        assert_eq!(out.skipped, 1);
        assert_eq!(out.envelope["accounts"].as_array().unwrap()[0]["number"], 2);
        assert!(
            out.notices[0].contains("no stored credentials"),
            "{}",
            out.notices[0]
        );
    }

    #[test]
    fn default_dir_follows_the_platform() {
        let dir = default_dir().unwrap();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        if cfg!(target_os = "linux") {
            assert_eq!(name, "claude-swap");
        } else {
            assert_eq!(name, ".claude-swap-backup");
        }
    }
}
