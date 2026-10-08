//! Export / import of managed accounts as a `.ccsw` envelope (spec §11;
//! research notes `cswap-cli-contract.md` §13 and `cswap-model-autoswitch.md` §8).
//!
//! Notices are printed to stderr as they happen, so a piped `export -` keeps
//! stdout pure JSON, and are also collected into the reports for callers and
//! tests.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::codex::auth::{AuthJson, AuthKind};
use crate::errors::{CcswError, Result};
use crate::model::{AccountKind, AccountRecord, Identity, Roster, now_iso, now_unix};
use crate::paths::Paths;
use crate::printer;
use crate::provider::Provider;
use crate::store::usage_store::{AUTH_DEAD_STRIKES, UsageStore};
use crate::store::{Store, alias_owner, credentials, normalize_alias, resolve_identifier, roster};

pub const FORMAT_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportTarget {
    File(PathBuf),
    Stdout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportSource {
    File(PathBuf),
    Stdin,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportReport {
    /// Accounts in the envelope.
    pub written: usize,
    /// Slots left out for lacking stored credentials (bulk export only).
    pub skipped: Vec<u32>,
    /// The envelope as written.
    pub envelope: Value,
    /// The stderr lines, in order.
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: usize,
    pub overwritten: usize,
    pub skipped: usize,
    pub replaced: usize,
    /// Slots whose credentials were written, in envelope order.
    pub written_slots: Vec<u32>,
    /// The stderr lines, in order.
    pub notices: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    version: u64,
    exported_at: String,
    exported_from: &'static str,
    ccsw_version: &'static str,
    /// The same value under cswap's name, for readers that look for it.
    swap_version: &'static str,
    encrypted: bool,
    active_account_number: Option<u32>,
    accounts: Vec<ExportedAccount>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportedAccount {
    number: u32,
    email: String,
    uuid: String,
    organization_uuid: String,
    organization_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_type: Option<String>,
    added: String,
    credentials: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<AccountKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    alias: Option<String>,
}

impl ExportedAccount {
    fn new(number: u32, record: &AccountRecord, credentials: Value) -> Self {
        Self {
            number,
            email: record.email.clone(),
            uuid: record.uuid.clone(),
            organization_uuid: record.organization_uuid.clone(),
            organization_name: record.organization_name.clone(),
            plan_type: record.plan_type.clone(),
            added: record.added.clone(),
            credentials,
            kind: record.kind,
            alias: record.alias.clone().filter(|alias| !alias.is_empty()),
        }
    }
}

/// `exportedFrom`: `macos`, `linux`, `wsl`, `windows` or `unknown`.
pub fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        if is_wsl() { "wsl" } else { "linux" }
    } else {
        "unknown"
    }
}

fn is_wsl() -> bool {
    fs::read_to_string("/proc/version")
        .is_ok_and(|version| version.to_lowercase().contains("microsoft"))
}

/// `export PATH` as the front controller calls it: `-` is stdout. Returns the exit status.
pub fn export_cmd(paths: &Paths, path: &str, account: Option<&str>, full: bool) -> i32 {
    let target = if path == "-" {
        ExportTarget::Stdout
    } else {
        ExportTarget::File(expand_tilde(path))
    };
    exit_status(export_accounts(paths, target, account, full).map(|_| ()))
}

/// `import PATH` as the front controller calls it: `-` is stdin. Returns the exit status.
pub fn import_cmd(paths: &Paths, path: &str, force: bool) -> i32 {
    let source = if path == "-" {
        ImportSource::Stdin
    } else {
        ImportSource::File(expand_tilde(path))
    };
    exit_status(import_accounts(paths, source, force).map(|_| ()))
}

fn exit_status(result: Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(err) => {
            printer::error(&format!("Error: {err}"));
            1
        }
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        if path == "~" {
            return home;
        }
        if let Some(rest) = path.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn notice(notices: &mut Vec<String>, line: String) {
    eprintln!("{line}");
    notices.push(line);
}

/// Export every managed account (or the one named by `account`: NUM | EMAIL | ALIAS).
/// `full` is accepted for cswap parity; Codex keeps no per-account config to add.
pub fn export_accounts(
    paths: &Paths,
    target: ExportTarget,
    account: Option<&str>,
    full: bool,
) -> Result<ExportReport> {
    let _ = full;
    let store = Store::open(paths.clone());
    let roster = roster::read(paths)?
        .filter(|roster| !roster.accounts.is_empty())
        .ok_or_else(|| CcswError::transfer("no accounts to export — run ccsw add first"))?;
    let slots = match account {
        Some(identifier) => vec![
            resolve_identifier(&roster, identifier)?
                .filter(|slot| roster.record(*slot).is_some())
                .ok_or_else(|| CcswError::transfer(format!("account not found: {identifier}")))?,
        ],
        None => roster.sorted_slots(),
    };
    // The archive format carries Codex accounts only in this release.
    let (slots, claude): (Vec<u32>, Vec<u32>) = slots.into_iter().partition(|slot| {
        roster
            .record(*slot)
            .is_none_or(|r| r.provider == Provider::Codex)
    });
    let live = AuthJson::read(&paths.live_auth_file()).ok().flatten();

    let mut notices = Vec::new();
    if !claude.is_empty() {
        notice(
            &mut notices,
            format!(
                "Skipped {} Claude Code account(s): export covers Codex accounts only in this release.",
                claude.len()
            ),
        );
    }
    let mut skipped = Vec::new();
    let mut accounts = Vec::new();
    for slot in slots {
        let Some(record) = roster.record(slot) else {
            continue;
        };
        match export_credentials(&store, &roster, slot, record, live.as_ref())? {
            Some(value) => accounts.push(ExportedAccount::new(slot, record, value)),
            None if account.is_some() => {
                return Err(CcswError::credential_read(format!(
                    "no backup credentials found for account {slot} ({})",
                    record.email
                )));
            }
            None => {
                notice(
                    &mut notices,
                    format!(
                        "Skipping Account-{slot} ({}): no stored credentials — re-add with: ccsw add --slot {slot}",
                        record.email
                    ),
                );
                skipped.push(slot);
            }
        }
    }
    if accounts.is_empty() && skipped.is_empty() {
        return Err(CcswError::transfer("no exportable accounts"));
    }
    if accounts.is_empty() {
        return Err(CcswError::transfer(
            "no exportable accounts — all managed slots are missing stored credentials. Re-add with: ccsw add --slot <number>",
        ));
    }

    let envelope = Envelope {
        version: FORMAT_VERSION,
        exported_at: now_iso(),
        exported_from: platform_name(),
        ccsw_version: crate::VERSION,
        swap_version: crate::VERSION,
        encrypted: false,
        active_account_number: roster
            .active_account_number
            .filter(|active| accounts.iter().any(|entry| entry.number == *active)),
        accounts,
    };
    let written = envelope.accounts.len();
    let serialize_err = |err: serde_json::Error| {
        CcswError::transfer(format!("could not serialize the export: {err}"))
    };
    let mut text = serde_json::to_string_pretty(&envelope).map_err(serialize_err)?;
    text.push('\n');
    let value = serde_json::to_value(&envelope).map_err(serialize_err)?;

    match &target {
        ExportTarget::Stdout => {
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(text.as_bytes())
                .and_then(|()| stdout.flush())
                .map_err(|err| CcswError::transfer(format!("could not write to stdout: {err}")))?;
        }
        ExportTarget::File(path) => {
            write_export_file(path, text.as_bytes())?;
            notice(
                &mut notices,
                format!("Exported {written} account(s) to {}", path.display()),
            );
        }
    }
    Ok(ExportReport {
        written,
        skipped,
        envelope: value,
        notices,
    })
}

/// The active slot exports the live login when it is the same identity (the
/// freshest tokens); everything else comes from the stored snapshot.
fn export_credentials(
    store: &Store,
    roster: &Roster,
    slot: u32,
    record: &AccountRecord,
    live: Option<&AuthJson>,
) -> Result<Option<Value>> {
    if roster.active_account_number == Some(slot)
        && let Some(live) = live
        && live
            .identity()
            .is_some_and(|identity| identity == record.identity())
    {
        return Ok(Some(live.0.clone()));
    }
    credentials::read(store, slot)
}

/// `<path>.<pid>.tmp` beside the target, 0600, fsync, rename. The parent is a
/// user directory, so its mode is left alone.
fn write_export_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut tmp_name: OsString = path.as_os_str().to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp_name);
    let written = write_private(&tmp, contents).and_then(|()| fs::rename(&tmp, path));
    if let Err(err) = written {
        let _ = fs::remove_file(&tmp);
        return Err(CcswError::transfer(format!(
            "could not write {}: {err}",
            path.display()
        )));
    }
    Ok(())
}

fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents)?;
    file.sync_all()
}

struct ParsedEnvelope {
    active: Option<u64>,
    accounts: Vec<Value>,
}

struct ImportEntry {
    number: u32,
    record: AccountRecord,
    credentials: Value,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Imported,
    Overwrote,
    Replaced,
}

/// Import an envelope: validate everything first (no writes), then write in
/// envelope order under the store lock, matching accounts on identity.
pub fn import_accounts(paths: &Paths, source: ImportSource, force: bool) -> Result<ImportReport> {
    let bytes = read_source(&source)?;
    let envelope = parse_envelope(&bytes)?;
    let mut report = ImportReport::default();
    let local = roster::read_or_empty(paths)?;
    let entries = validate_entries(&envelope.accounts, &local, &mut report.notices)?;

    let store = Store::open(paths.clone());
    let _lock = store.lock()?;
    store.ensure_dirs()?;
    let mut roster = roster::init_if_absent(paths)?;
    let usage = UsageStore::new(paths);
    let now = now_unix() as f64;
    let mut resolved_active: Option<u32> = None;

    for entry in entries {
        let identity = entry.record.identity();
        let email = identity.email.clone();
        let envelope_active = envelope.active == Some(u64::from(entry.number));
        let (slot, outcome) = match roster.find_slot(Provider::Codex, &identity) {
            Some(slot) if force => (slot, Outcome::Overwrote),
            Some(slot) if slot_token_dead(&usage, &store, slot, &identity, now) => {
                (slot, Outcome::Replaced)
            }
            Some(slot) => {
                notice(
                    &mut report.notices,
                    format!("Skipped {email} (already exists, use --force)"),
                );
                report.skipped += 1;
                if envelope_active {
                    resolved_active = Some(slot);
                }
                continue;
            }
            None if roster.record(entry.number).is_none() => (entry.number, Outcome::Imported),
            None => (roster.next_free_slot(), Outcome::Imported),
        };
        // What the overwrite notes report is read before the write clears it.
        let strike = (outcome == Outcome::Overwrote)
            .then(|| strike_state(&usage, slot, &identity, &entry.credentials, now));

        credentials::write(&store, slot, &entry.credentials)?;
        usage.clear_dead_token(&[slot])?;
        roster.add_record(slot, entry.record);
        roster::write(paths, &roster)?;

        match outcome {
            Outcome::Imported => {
                report.imported += 1;
                notice(
                    &mut report.notices,
                    format!("Imported {email} → slot {slot}"),
                );
            }
            Outcome::Overwrote => {
                report.overwritten += 1;
                notice(
                    &mut report.notices,
                    format!("Overwrote {email} (slot {slot})"),
                );
                if let Some((had_strike, same_generation)) = strike {
                    if had_strike {
                        notice(
                            &mut report.notices,
                            "  └ cleared this slot's stored dead-token strike".to_string(),
                        );
                    }
                    if same_generation {
                        notice(
                            &mut report.notices,
                            "  └ this import holds the same credential generation the strike condemned; another permanent auth failure will quarantine it again — recover with a newer export or a re-login".to_string(),
                        );
                    }
                }
            }
            Outcome::Replaced => {
                report.replaced += 1;
                notice(
                    &mut report.notices,
                    format!("Replaced {email} (slot {slot} was quarantined: refresh token dead)"),
                );
            }
        }
        report.written_slots.push(slot);
        if envelope_active {
            resolved_active = Some(slot);
        }
    }

    if roster
        .active_account_number
        .is_none_or(|active| active == 0)
        && let Some(slot) = resolved_active
    {
        roster.set_active(Some(slot));
        roster::write(paths, &roster)?;
    }

    let mut summary = format!(
        "Done: {} imported, {} overwritten, {} skipped",
        report.imported, report.overwritten, report.skipped
    );
    if report.replaced > 0 {
        summary.push_str(&format!(", {} replaced (dead token)", report.replaced));
    }
    notice(&mut report.notices, summary);

    if let Some(slot) = live_login_slot(paths, &store, &roster)
        && report.written_slots.contains(&slot)
        && let Some(record) = roster.record(slot)
    {
        notice(
            &mut report.notices,
            format!(
                "Note: {} is your current live login — activate the imported credentials with: ccsw switch {slot} --force",
                record.email
            ),
        );
    }
    Ok(report)
}

fn read_source(source: &ImportSource) -> Result<Vec<u8>> {
    match source {
        ImportSource::Stdin => {
            let mut bytes = Vec::new();
            io::stdin()
                .read_to_end(&mut bytes)
                .map_err(|err| CcswError::transfer(format!("could not read stdin: {err}")))?;
            Ok(bytes)
        }
        ImportSource::File(path) => match fs::read(path) {
            Ok(bytes) => Ok(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(CcswError::transfer(format!(
                "import file not found: {}",
                path.display()
            ))),
            Err(err) => Err(CcswError::transfer(format!(
                "could not read {}: {err}",
                path.display()
            ))),
        },
    }
}

fn parse_envelope(bytes: &[u8]) -> Result<ParsedEnvelope> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|err| CcswError::transfer(format!("export file is not valid JSON: {err}")))?;
    let Value::Object(root) = value else {
        return Err(CcswError::transfer("export file must be a JSON object"));
    };
    let version = root.get("version");
    if version.and_then(Value::as_f64) != Some(FORMAT_VERSION as f64) {
        return Err(CcswError::transfer(format!(
            "unsupported export version: {} (expected 1)",
            python_repr(version)
        )));
    }
    if root.get("encrypted") == Some(&Value::Bool(true)) {
        return Err(CcswError::transfer(
            "encrypted exports are not supported in this version — decrypt before piping (e.g. gpg -d backup.gpg | ccsw import -)",
        ));
    }
    let accounts = match root.get("accounts") {
        Some(Value::Array(list)) if !list.is_empty() => list.clone(),
        _ => {
            return Err(CcswError::transfer("export file has no accounts to import"));
        }
    };
    Ok(ParsedEnvelope {
        active: root.get("activeAccountNumber").and_then(Value::as_u64),
        accounts,
    })
}

/// Pass 1: every entry checked, nothing written. Alias conflicts with a
/// different local identity are warnings that drop the imported alias.
fn validate_entries(
    raw: &[Value],
    local: &Roster,
    notices: &mut Vec<String>,
) -> Result<Vec<ImportEntry>> {
    let mut entries = Vec::new();
    let mut identities = BTreeSet::new();
    let mut aliases = BTreeSet::new();
    for item in raw {
        let Value::Object(entry) = item else {
            return Err(CcswError::transfer("account entry must be a JSON object"));
        };
        let email = match entry.get("email") {
            Some(Value::String(email)) if is_valid_email(email) => email.clone(),
            other => {
                return Err(CcswError::transfer(format!(
                    "invalid or missing email in imported account: {}",
                    python_repr(other)
                )));
            }
        };
        let number = entry
            .get("number")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n >= 1)
            .ok_or_else(|| {
                CcswError::transfer(format!(
                    "invalid slot number in imported account ({email}): {}",
                    python_repr(entry.get("number"))
                ))
            })?;
        let text_field = |field: &str| -> Result<Option<String>> {
            match entry.get(field) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) => Ok(Some(text.clone())),
                Some(other) => Err(CcswError::transfer(format!(
                    "{field} for {email} must be a string, got {}",
                    python_type_name(other)
                ))),
            }
        };
        let organization_uuid = text_field("organizationUuid")?.unwrap_or_default();
        let organization_name = text_field("organizationName")?.unwrap_or_default();
        let uuid = text_field("uuid")?.unwrap_or_default();
        let added = text_field("added")?
            .filter(|text| !text.is_empty())
            .unwrap_or_else(now_iso);
        let mut alias =
            match text_field("alias")? {
                None => None,
                Some(raw_alias) => Some(normalize_alias(&raw_alias).map_err(|err| {
                    CcswError::transfer(format!("invalid alias for {email}: {err}"))
                })?),
            };
        let plan_type = text_field("planType")?.filter(|text| !text.is_empty());
        let auth = match entry.get("credentials") {
            Some(value @ Value::Object(_)) => AuthJson::from_value(value.clone()),
            _ => {
                return Err(CcswError::transfer(format!(
                    "credentials for {email} must be a JSON object"
                )));
            }
        };
        let flagged_api_key = entry.get("kind").and_then(Value::as_str) == Some("api_key");
        let kind =
            (flagged_api_key || auth.kind() == AuthKind::ApiKey).then_some(AccountKind::ApiKey);

        if !identities.insert((email.clone(), organization_uuid.clone())) {
            let org = if organization_uuid.is_empty() {
                "personal"
            } else {
                organization_uuid.as_str()
            };
            return Err(CcswError::transfer(format!(
                "duplicate account in export: {email} (org={org})"
            )));
        }
        let identity = Identity::new(email.clone(), organization_uuid.clone());
        if let Some(name) = alias.clone() {
            if !aliases.insert(name.clone()) {
                return Err(CcswError::transfer(format!(
                    "duplicate alias in export: {name}"
                )));
            }
            let foreign_owner = alias_owner(local, &name)
                .and_then(|owner| local.record(owner))
                .is_some_and(|owner| owner.identity() != identity);
            if foreign_owner {
                notice(
                    notices,
                    format!(
                        "Warning: alias '{name}' for {email} already used by an existing account, dropping the imported alias"
                    ),
                );
                alias = None;
            }
        }

        let mut record = AccountRecord::new(email);
        record.uuid = uuid;
        record.organization_uuid = organization_uuid;
        record.organization_name = organization_name;
        record.plan_type = plan_type;
        record.added = added;
        record.alias = alias;
        record.kind = kind;
        entries.push(ImportEntry {
            number,
            record,
            credentials: auth.0,
        });
    }
    Ok(entries)
}

/// A dead-token strike on the slot's row that still binds to the stored generation.
fn slot_token_dead(
    usage: &UsageStore,
    store: &Store,
    slot: u32,
    identity: &Identity,
    now: f64,
) -> bool {
    let ids = BTreeMap::from([(slot, identity.clone())]);
    usage.entries(&ids, now).get(&slot).is_some_and(|entry| {
        entry.token_dead(
            AUTH_DEAD_STRIKES,
            credentials::slot_fingerprint(store, slot).as_deref(),
        )
    })
}

/// `(the slot had a strike, the imported credential is the generation it condemned)`.
fn strike_state(
    usage: &UsageStore,
    slot: u32,
    identity: &Identity,
    imported: &Value,
    now: f64,
) -> (bool, bool) {
    let ids = BTreeMap::from([(slot, identity.clone())]);
    let Some(entry) = usage.entries(&ids, now).remove(&slot) else {
        return (false, false);
    };
    let had_strike = entry.auth_dead_strikes > 0;
    let same_generation = had_strike
        && entry.struck_fingerprint.is_some()
        && entry.struck_fingerprint == credentials::fingerprint(imported);
    (had_strike, same_generation)
}

/// The managed slot holding the live login: by identity, or by key for an API key.
fn live_login_slot(paths: &Paths, store: &Store, roster: &Roster) -> Option<u32> {
    let live = AuthJson::read(&paths.live_auth_file()).ok().flatten()?;
    if let Some(identity) = live.identity() {
        return roster.find_slot(Provider::Codex, &identity);
    }
    let key = live.api_key()?;
    roster.sequence.iter().copied().find(|slot| {
        credentials::read(store, *slot)
            .ok()
            .flatten()
            .is_some_and(|value| AuthJson::from_value(value).api_key() == Some(key))
    })
}

/// `^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$`, which also keeps the
/// email out of path games.
fn is_valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-'));
    let Some((host, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    let host_ok = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
    let tld_ok = tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic());
    local_ok && host_ok && tld_ok
}

// The messages mirror cswap, which shows Python's repr and type names.
fn python_repr(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(Value::String(text)) => format!("'{text}'"),
        Some(other) => other.to_string(),
    }
}

fn python_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn email_shape() {
        for ok in [
            "a@example.com",
            "first.last+tag@sub.example.org",
            "api-key-2@token.local",
            "x%y@a-b.co",
        ] {
            assert!(is_valid_email(ok), "{ok}");
        }
        for bad in [
            "",
            "a",
            "a@",
            "@b.com",
            "a@b",
            "a@b.c",
            "a@.com",
            "a b@x.com",
            "a@b@c.com",
            "a@b.c0m",
            "../x@example.com",
        ] {
            assert!(!is_valid_email(bad), "{bad}");
        }
    }

    #[test]
    fn python_flavoured_messages() {
        assert_eq!(python_repr(None), "None");
        assert_eq!(python_repr(Some(&Value::Null)), "None");
        assert_eq!(python_repr(Some(&json!(true))), "True");
        assert_eq!(python_repr(Some(&json!("1"))), "'1'");
        assert_eq!(python_repr(Some(&json!(2))), "2");
        assert_eq!(python_repr(Some(&json!(1.5))), "1.5");
        assert_eq!(python_type_name(&json!(1.5)), "float");
        assert_eq!(python_type_name(&json!(1)), "int");
        assert_eq!(python_type_name(&json!(null)), "NoneType");
        assert_eq!(python_type_name(&json!([])), "list");
        assert_eq!(python_type_name(&json!({})), "dict");
    }

    #[test]
    fn platform_and_tilde() {
        assert!(["macos", "linux", "wsl", "windows", "unknown"].contains(&platform_name()));
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/b.ccsw"), home.join("b.ccsw"));
        assert_eq!(expand_tilde("~x/b"), PathBuf::from("~x/b"));
        assert_eq!(expand_tilde("/tmp/b"), PathBuf::from("/tmp/b"));
    }

    /// Slot 1 is a Codex account, slot 2 a Claude Code one; both have credentials.
    fn mixed_store() -> (tempfile::TempDir, Store) {
        let (dir, store) = crate::store::temp_store();
        let mut roster = Roster::empty();
        roster.add_record(1, AccountRecord::new("codex@example.com"));
        let mut claude = AccountRecord::new("claude@example.com");
        claude.provider = Provider::Claude;
        roster.add_record(2, claude);
        roster::write(&store.paths, &roster).unwrap();
        credentials::write(
            &store,
            1,
            &json!({"auth_mode": "apikey", "OPENAI_API_KEY": "sk-x"}),
        )
        .unwrap();
        credentials::write(&store, 2, &json!({"claudeAiOauth": {"accessToken": "cat"}})).unwrap();
        (dir, store)
    }

    #[test]
    fn export_skips_claude_slots() {
        let (dir, store) = mixed_store();
        let path = dir.path().join("out.ccsw");
        let report =
            export_accounts(&store.paths, ExportTarget::File(path.clone()), None, false).unwrap();
        assert_eq!(report.written, 1);
        assert_eq!(
            report.notices[0],
            "Skipped 1 Claude Code account(s): export covers Codex accounts only in this release."
        );
        let numbers: Vec<u64> = report.envelope["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["number"].as_u64().unwrap())
            .collect();
        assert_eq!(numbers, [1]);
        let written = fs::read_to_string(&path).unwrap();
        assert!(!written.contains("claude@example.com"));
        assert!(!written.contains("claudeAiOauth"));

        let only_claude = dir.path().join("claude.ccsw");
        let err = export_accounts(
            &store.paths,
            ExportTarget::File(only_claude.clone()),
            Some("2"),
            false,
        )
        .unwrap_err();
        assert_eq!(err.type_name(), "TransferError");
        assert!(!only_claude.exists(), "nothing was written");
    }

    #[test]
    fn export_file_is_private_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.ccsw");
        write_export_file(&path, b"{}\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let err = write_export_file(&dir.path().join("missing/out.ccsw"), b"{}").unwrap_err();
        assert_eq!(err.type_name(), "TransferError");
        assert!(err.to_string().starts_with("could not write "));
    }
}
