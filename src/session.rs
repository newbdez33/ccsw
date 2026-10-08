//! Session mode: per-account `CODEX_HOME` profiles behind `run` / `env`, and the
//! directory mappings behind `map` / `unmap` (spec §10; contract §9).
//!
//! Everything here returns prerendered lines and a ready [`Command`] instead of
//! printing or launching, so the command layer decides where lines go (`env`
//! keeps stdout eval-able) and tests can drive a fake `codex`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use crate::codex::app_server::{codex_supports_no_daemon, command_on_path, embedded_codex_argv};
use crate::codex::auth::{AuthJson, AuthKind};
use crate::collect::{self, RefreshStatus};
use crate::errors::{CcswError, Result};
use crate::fsutil;
use crate::model::Roster;
use crate::printer;
use crate::provider::Provider;
use crate::store::usage_store::UsageStore;
use crate::store::{MappingStore, Store, credentials, ensure_private_dir, resolve_slot};

/// Items linked from the source Codex home into a profile unless `--no-share`.
pub const SHARED_ITEMS: &[&str] = &["AGENTS.md", "prompts", "skills"];
/// Items linked with `--share-history` (POSIX only).
pub const HISTORY_ITEMS: &[&str] = &["sessions", "history.jsonl"];
pub const MANIFEST_NAME: &str = ".ccsw-shared.json";
/// Environment variables that would override the selected account inside Codex.
pub const SCRUBBED_ENV: &[&str] = &["OPENAI_API_KEY", "CODEX_API_KEY"];

pub const CODEX_MISSING: &str = "'codex' was not found on PATH. Install the Codex CLI first.";
pub const CLAUDE_SESSION_LATER: &str = "Session mode for Claude Code accounts arrives in a later release; use `ccsw switch <slot>` for now.";
pub const SHARE_HISTORY_WINDOWS: &str = "--share-history is not supported on Windows yet: sharing uses re-synced copies there, which would fork the history instead of sharing it.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareOptions {
    pub share: bool,
    pub share_history: bool,
}

impl Default for ShareOptions {
    fn default() -> Self {
        Self {
            share: true,
            share_history: false,
        }
    }
}

/// What the launching process sees: the preset `CODEX_HOME`, which auth
/// override variables are set, and where `codex` is.
#[derive(Debug, Clone, Default)]
pub struct HostEnv {
    pub codex_home_preset: Option<String>,
    pub set_vars: Vec<String>,
    pub codex: Option<PathBuf>,
}

impl HostEnv {
    pub fn detect() -> Self {
        Self {
            codex_home_preset: std::env::var("CODEX_HOME").ok().filter(|v| !v.is_empty()),
            set_vars: SCRUBBED_ENV
                .iter()
                .filter(|name| std::env::var_os(name).is_some())
                .map(|name| name.to_string())
                .collect(),
            codex: command_on_path("codex"),
        }
    }

    fn codex(&self) -> Result<&Path> {
        self.codex
            .as_deref()
            .ok_or_else(|| CcswError::session(CODEX_MISSING))
    }
}

/// The Codex home whose items are shared into profiles: `$CODEX_HOME`, unless
/// that is itself a session profile (a shell pinned by `ccsw env`), in
/// which case the default home is the source.
pub fn source_home(store: &Store) -> PathBuf {
    let home = &store.paths.codex_home;
    if home.starts_with(store.paths.sessions_dir()) {
        dirs::home_dir()
            .map(|h| h.join(".codex"))
            .unwrap_or_else(|| home.clone())
    } else {
        home.clone()
    }
}

/// Session mode is Codex-only in this release: refuse a Claude Code slot.
fn codex_slot(roster: &Roster, slot: u32) -> Result<u32> {
    if roster
        .record(slot)
        .is_some_and(|r| r.provider == Provider::Claude)
    {
        return Err(CcswError::session(CLAUDE_SESSION_LATER));
    }
    Ok(slot)
}

/// The account a directory maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappedAccount {
    None,
    /// A mapping exists but its account left the roster.
    Removed {
        email: String,
    },
    Slot(u32),
}

pub fn mapped_account(store: &Store, roster: &Roster, cwd: &Path) -> MappedAccount {
    match MappingStore::load(&store.paths).resolve(Provider::Codex, cwd) {
        None => MappedAccount::None,
        Some((_, identity)) => match roster.find_slot(Provider::Codex, &identity) {
            Some(slot) => MappedAccount::Slot(slot),
            None => MappedAccount::Removed {
                email: identity.email,
            },
        },
    }
}

/// Where `run` lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunTarget {
    /// Plain `codex`, env untouched; `notice` explains why.
    Default {
        notice: Option<String>,
    },
    Slot(u32),
}

pub fn resolve_run_target(
    store: &Store,
    roster: &Roster,
    account: Option<&str>,
    cwd: &Path,
) -> Result<RunTarget> {
    if let Some(identifier) = account {
        let slot = codex_slot(roster, resolve_slot(roster, identifier)?)?;
        return Ok(RunTarget::Slot(slot));
    }
    Ok(match mapped_account(store, roster, cwd) {
        MappedAccount::Slot(slot) => RunTarget::Slot(slot),
        MappedAccount::Removed { email } => RunTarget::Default {
            notice: Some(printer::yellowed(&format!(
                "Mapped account {email} no longer exists — launching the default account."
            ))),
        },
        MappedAccount::None => RunTarget::Default {
            notice: Some(printer::dimmed(&format!(
                "No account mapped for {} — launching the default account.",
                cwd.display()
            ))),
        },
    })
}

/// A profile ready for launch.
#[derive(Debug)]
pub struct Prepared {
    pub slot: u32,
    pub email: String,
    pub profile: PathBuf,
    /// Warnings gathered on the way (prerendered).
    pub notices: Vec<String>,
}

/// Bootstrap or reuse the slot's profile: fold back tokens Codex rotated inside
/// it, refresh an expiring token (best-effort), write `auth.json` from the
/// slot, copy `config.toml`, and sync the shared items.
pub fn prepare_profile(
    store: &Store,
    roster: &Roster,
    slot: u32,
    opts: ShareOptions,
) -> Result<Prepared> {
    if opts.share_history && cfg!(windows) {
        return Err(CcswError::session(SHARE_HISTORY_WINDOWS));
    }
    let record = roster
        .record(slot)
        .ok_or_else(|| CcswError::AccountNotFound(format!("Account-{slot} does not exist")))?;
    if record.provider == Provider::Claude {
        let profile = crate::claude::session::prepare(
            store,
            slot,
            record,
            &crate::claude::keychain::SystemSecurity,
        )?;
        return Ok(Prepared {
            slot,
            email: record.email.clone(),
            profile,
            notices: Vec::new(),
        });
    }
    let profile = store.paths.session_dir(slot, &record.email);
    let mut notices = Vec::new();

    fold_back(store, slot, &profile)?;
    match collect::refresh_slot(store, roster, slot, false) {
        RefreshStatus::Ok | RefreshStatus::NotNeeded => {}
        RefreshStatus::NoRefreshToken
        | RefreshStatus::Transient(_)
        | RefreshStatus::Terminal { .. } => notices.push(printer::yellowed(&format!(
            "Could not refresh the token for Account-{slot}; continuing with the stored credentials."
        ))),
    }
    let stored = credentials::read(store, slot)?.ok_or_else(|| {
        CcswError::session(format!(
            "Account-{slot} has no stored credentials. Re-add with: ccsw add --slot {slot}"
        ))
    })?;
    ensure_private_dir(&profile)?;
    AuthJson::from_value(stored).write(&profile.join("auth.json"))?;

    let source = source_home(store);
    let config = source.join("config.toml");
    match fs::read(&config) {
        Ok(bytes) => {
            let dest = profile.join("config.toml");
            fsutil::atomic_write_private(&dest, &bytes)
                .map_err(|err| fsutil::io_error(CcswError::Session, &dest, &err))?;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(fsutil::io_error(CcswError::Session, &config, &err));
        }
    }
    notices.extend(sync_sharing(&profile, &source, opts)?);
    Ok(Prepared {
        slot,
        email: record.email.clone(),
        profile,
        notices,
    })
}

/// Store the profile's `auth.json` into the slot when Codex rotated it there
/// (freshness rule, same identity). Returns whether the slot changed.
pub fn fold_back(store: &Store, slot: u32, profile: &Path) -> Result<bool> {
    let Some(profile_auth) = AuthJson::read(&profile.join("auth.json")).ok().flatten() else {
        return Ok(false);
    };
    let Some(stored) = credentials::read(store, slot)? else {
        return Ok(false);
    };
    let stored = AuthJson::from_value(stored);
    if profile_auth.kind() != AuthKind::ChatGpt
        || profile_auth.identity() != stored.identity()
        || !profile_auth.is_newer_than(&stored)
    {
        return Ok(false);
    }
    let _lock = store.lock()?;
    credentials::write(store, slot, &profile_auth.0)?;
    UsageStore::new(&store.paths).clear_dead_token(&[slot])?;
    tracing::info!("folded the session profile's rotated tokens back into Account-{slot}");
    Ok(true)
}

fn manifest_items(profile: &Path) -> Vec<String> {
    fsutil::read_json(&profile.join(MANIFEST_NAME))
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_value(value["items"].clone()).ok())
        .unwrap_or_default()
}

/// Link (copy on Windows) the active items from `source` into the profile and
/// prune items the manifest lists that are no longer active. Returns notices.
pub fn sync_sharing(profile: &Path, source: &Path, opts: ShareOptions) -> Result<Vec<String>> {
    let mut active: Vec<&str> = Vec::new();
    if opts.share {
        active.extend(SHARED_ITEMS);
    }
    if opts.share_history && !cfg!(windows) {
        active.extend(HISTORY_ITEMS);
    }
    let previous = manifest_items(profile);
    let session_err =
        |path: &Path, err: &io::Error| fsutil::io_error(CcswError::Session, path, err);

    for item in previous
        .iter()
        .filter(|item| !active.contains(&item.as_str()))
    {
        remove_shared(&profile.join(item)).map_err(|err| session_err(&profile.join(item), &err))?;
    }

    let mut notices = Vec::new();
    let mut shared: Vec<String> = Vec::new();
    for item in active {
        let src = source.join(item);
        let dest = profile.join(item);
        if !src.exists() {
            continue;
        }
        match fs::symlink_metadata(&dest) {
            Ok(meta) if meta.is_symlink() => {
                if fs::read_link(&dest).ok().as_deref() != Some(src.as_path()) {
                    fs::remove_file(&dest).map_err(|err| session_err(&dest, &err))?;
                    link_item(&src, &dest).map_err(|err| session_err(&dest, &err))?;
                }
                shared.push(item.to_string());
            }
            Ok(_) if previous.iter().any(|p| p == item) && cfg!(windows) => {
                // A copy we made earlier: refresh it.
                remove_shared(&dest).map_err(|err| session_err(&dest, &err))?;
                link_item(&src, &dest).map_err(|err| session_err(&dest, &err))?;
                shared.push(item.to_string());
            }
            Ok(_) => notices.push(printer::yellowed(&format!(
                "Not sharing {item}: the session profile already has its own copy."
            ))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                link_item(&src, &dest).map_err(|err| session_err(&dest, &err))?;
                shared.push(item.to_string());
            }
            Err(err) => return Err(session_err(&dest, &err)),
        }
    }

    let manifest = profile.join(MANIFEST_NAME);
    if shared.is_empty() {
        match fs::remove_file(&manifest) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(session_err(&manifest, &err)),
        }
    } else {
        let mode = if cfg!(windows) { "copy" } else { "symlink" };
        fsutil::write_json_private(&manifest, &json!({"items": shared, "mode": mode}))
            .map_err(|err| session_err(&manifest, &err))?;
    }
    Ok(notices)
}

/// Remove a shared item: a symlink always, a real path only where sharing
/// works by copying (Windows). A real file on POSIX is the profile's own.
fn remove_shared(dest: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dest) {
        Ok(meta) if meta.is_symlink() => fs::remove_file(dest),
        Ok(meta) if cfg!(windows) => {
            if meta.is_dir() {
                fs::remove_dir_all(dest)
            } else {
                fs::remove_file(dest)
            }
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn link_item(src: &Path, dest: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dest)
}

#[cfg(not(unix))]
fn link_item(src: &Path, dest: &Path) -> io::Result<()> {
    if src.is_dir() {
        copy_recursive(src, dest)
    } else {
        fs::copy(src, dest).map(|_| ())
    }
}

#[cfg(not(unix))]
fn copy_recursive(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// A launch ready to exec: the child command plus the lines to print first.
pub struct Launch {
    pub command: Command,
    pub notices: Vec<String>,
    /// The profile slot when the child runs in session mode.
    pub session: Option<(u32, PathBuf)>,
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launch")
            .field("command", &self.command)
            .field("notices", &self.notices)
            .field("session", &self.session)
            .finish()
    }
}

fn launching_line(slot: u32, email: &str) -> String {
    format!(
        "{} Account-{slot} ({email}) {}",
        printer::accent("Launching"),
        printer::muted("[session mode]")
    )
}

/// Plan `codex <tail>` for `target` (contract §9.1, spec §10).
pub fn plan_launch(
    store: &Store,
    roster: &Roster,
    host: &HostEnv,
    target: RunTarget,
    tail: Vec<String>,
    opts: ShareOptions,
) -> Result<Launch> {
    let codex = host.codex()?;
    if opts.share_history && cfg!(windows) {
        return Err(CcswError::session(SHARE_HISTORY_WINDOWS));
    }
    let slot = match target {
        RunTarget::Default { notice } => {
            let mut command = Command::new(codex);
            command.args(&tail);
            return Ok(Launch {
                command,
                notices: notice.into_iter().collect(),
                session: None,
            });
        }
        RunTarget::Slot(slot) => slot,
    };
    let record = roster
        .record(slot)
        .ok_or_else(|| CcswError::AccountNotFound(format!("Account-{slot} does not exist")))?;
    let mut notices = Vec::new();
    match &host.codex_home_preset {
        None => {
            if collect::live_login(store, roster).slot() == Some(slot) {
                notices.push(format!(
                    "Account-{slot} ({}) is already the active default login — launching codex directly.",
                    record.email
                ));
                let mut command = Command::new(codex);
                command.args(&tail);
                return Ok(Launch {
                    command,
                    notices,
                    session: None,
                });
            }
        }
        Some(preset) => notices.push(printer::yellowed(&format!(
            "CODEX_HOME is already set ({preset}); overriding it for this launch."
        ))),
    }
    let prepared = prepare_profile(store, roster, slot, opts)?;
    notices.extend(prepared.notices);
    if !host.set_vars.is_empty() {
        notices.push(printer::yellowed(&format!(
            "Ignoring {} for this session — it would override the selected account inside Codex.",
            host.set_vars.join(", ")
        )));
    }
    notices.push(launching_line(slot, &prepared.email));
    let argv = embedded_codex_argv(codex_supports_no_daemon(codex), tail);
    let mut command = Command::new(codex);
    command.args(argv);
    for name in SCRUBBED_ENV {
        command.env_remove(name);
    }
    command.env("CODEX_HOME", &prepared.profile);
    Ok(Launch {
        command,
        notices,
        session: Some((slot, prepared.profile)),
    })
}

/// POSIX: replace this process with the child (only returns on failure).
/// Elsewhere: wait for the child, fold rotated tokens back, mirror its status.
pub fn exec_or_wait(store: &Store, mut launch: Launch) -> Result<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The fold-back happens at the next bootstrap: exec never returns.
        let _ = store;
        let err = launch.command.exec();
        Err(CcswError::session(format!("could not launch codex: {err}")))
    }
    #[cfg(not(unix))]
    {
        let status = launch
            .command
            .status()
            .map_err(|err| CcswError::session(format!("could not launch codex: {err}")))?;
        if let Some((slot, profile)) = &launch.session
            && let Err(err) = fold_back(store, *slot, profile)
        {
            tracing::warn!("could not fold the session's tokens back into Account-{slot}: {err}");
        }
        Ok(status.code().unwrap_or(1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Sh,
    Fish,
    Pwsh,
}

impl Shell {
    pub const CHOICES: &[&str] = &["sh", "fish", "pwsh"];

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "sh" => Some(Self::Sh),
            "fish" => Some(Self::Fish),
            "pwsh" => Some(Self::Pwsh),
            _ => None,
        }
    }

    fn quote(self, value: &str) -> String {
        match self {
            Self::Sh | Self::Fish => format!("'{}'", value.replace('\'', "'\\''")),
            Self::Pwsh => format!("'{}'", value.replace('\'', "''")),
        }
    }

    pub fn export_line(self, dir: &Path) -> String {
        let value = self.quote(&dir.to_string_lossy());
        match self {
            Self::Sh => format!("export CODEX_HOME={value}"),
            Self::Fish => format!("set -gx CODEX_HOME {value}"),
            Self::Pwsh => format!("$env:CODEX_HOME = {value}"),
        }
    }

    pub fn unset_line(self, name: &str) -> String {
        match self {
            Self::Sh => format!("unset {name}"),
            Self::Fish => format!("set -e {name}"),
            Self::Pwsh => format!("Remove-Item Env:{name} -ErrorAction SilentlyContinue"),
        }
    }
}

/// What `env` prints: `lines` are the eval-able stdout, `notices` go to stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvPlan {
    Lines {
        lines: Vec<String>,
        notices: Vec<String>,
    },
    /// Nothing exported; the note explains why (exit 0).
    Note(String),
}

/// The inputs of `env`.
#[derive(Debug, Clone)]
pub struct EnvRequest<'a> {
    pub account: Option<&'a str>,
    pub cwd: &'a Path,
    pub shell: Shell,
    /// Print only the unpin line; needs no account and touches nothing.
    pub unset: bool,
    pub opts: ShareOptions,
}

pub fn plan_env(
    store: &Store,
    roster: &Roster,
    host: &HostEnv,
    request: EnvRequest<'_>,
) -> Result<EnvPlan> {
    let EnvRequest {
        account,
        cwd,
        shell,
        unset,
        opts,
    } = request;
    if unset {
        return Ok(EnvPlan::Lines {
            lines: vec![shell.unset_line("CODEX_HOME")],
            notices: Vec::new(),
        });
    }
    let slot = match account {
        Some(identifier) => codex_slot(roster, resolve_slot(roster, identifier)?)?,
        None => match mapped_account(store, roster, cwd) {
            MappedAccount::Slot(slot) => slot,
            MappedAccount::Removed { email } => {
                return Err(CcswError::session(format!(
                    "Nothing to prepare an environment for (the mapped account {email} no longer exists). Pass an account (ccsw env <NUM|EMAIL|ALIAS>), map this directory (ccsw map <NUM|EMAIL|ALIAS>), or clear a pinned profile with ccsw env --unset."
                )));
            }
            MappedAccount::None => {
                return Err(CcswError::session(format!(
                    "Nothing to prepare an environment for (no account given and no mapping for {}). Pass an account (ccsw env <NUM|EMAIL|ALIAS>), map this directory (ccsw map <NUM|EMAIL|ALIAS>), or clear a pinned profile with ccsw env --unset.",
                    cwd.display()
                )));
            }
        },
    };
    let record = roster
        .record(slot)
        .ok_or_else(|| CcswError::AccountNotFound(format!("Account-{slot} does not exist")))?;
    if host.codex_home_preset.is_none() && collect::live_login(store, roster).slot() == Some(slot) {
        return Ok(EnvPlan::Note(format!(
            "Account-{slot} ({}) is the active default login — an unpinned shell already uses it; nothing exported.",
            record.email
        )));
    }
    let prepared = prepare_profile(store, roster, slot, opts)?;
    let mut lines: Vec<String> = host
        .set_vars
        .iter()
        .map(|name| shell.unset_line(name))
        .collect();
    lines.push(shell.export_line(&prepared.profile));
    let mut notices = prepared.notices;
    notices.push(format!(
        "Prepared Account-{slot} ({}) {}",
        prepared.email,
        printer::muted("[session mode]")
    ));
    Ok(EnvPlan::Lines { lines, notices })
}

/// `map [<id> [PATH]]`: the lines to print, in order.
pub fn map(
    store: &Store,
    roster: &Roster,
    account: Option<&str>,
    path: Option<&Path>,
    cwd: &Path,
) -> Result<Vec<String>> {
    let mut mappings = MappingStore::load(&store.paths);
    let Some(identifier) = account else {
        return Ok(list_mappings(&mappings, roster));
    };
    let slot = codex_slot(roster, resolve_slot(roster, identifier)?)?;
    let record = roster
        .record(slot)
        .ok_or_else(|| CcswError::AccountNotFound(format!("Account-{slot} does not exist")))?;
    let target = path.map_or_else(|| cwd.to_path_buf(), Path::to_path_buf);
    let mut lines = Vec::new();
    if !target.is_dir() {
        lines.push(printer::yellowed(&format!(
            "Warning: {} is not an existing directory (mapping it anyway)",
            target.display()
        )));
    }
    let previous = mappings.set(record.provider, &target, &record.identity());
    mappings.save()?;
    let mut line = format!(
        "{} {} → Account-{slot} ({})",
        printer::accent("Mapped"),
        MappingStore::normalize_path(&target).display(),
        record.email
    );
    if let Some(previous) = previous
        && previous.email != record.email
    {
        line.push(' ');
        line.push_str(&printer::muted(&format!("(was {})", previous.email)));
    }
    lines.push(line);
    Ok(lines)
}

fn list_mappings(mappings: &MappingStore, roster: &Roster) -> Vec<String> {
    if mappings.is_empty() {
        return vec![
            printer::dimmed("No directory mappings yet."),
            printer::muted("Map one with: ccsw map <NUM|EMAIL> [PATH]"),
        ];
    }
    let mut lines = vec![printer::bolded("Directory mappings:")];
    for (path, provider, identity) in mappings.entries() {
        let arrow = printer::dimmed("→");
        let line = match roster.find_slot(provider, &identity) {
            Some(slot) => {
                let tag = roster
                    .record(slot)
                    .map(|r| r.display_tag())
                    .unwrap_or_default();
                format!(
                    "  {} {arrow} {slot}: {} {}",
                    path.display(),
                    identity.email,
                    printer::muted(&format!("[{tag}]"))
                )
            }
            None => format!(
                "  {} {arrow} {} (account removed)",
                path.display(),
                identity.email
            ),
        };
        lines.push(line);
    }
    lines
}

/// `unmap [PATH]`: the line to print.
pub fn unmap(store: &Store, path: Option<&Path>, cwd: &Path) -> Result<String> {
    let mut mappings = MappingStore::load(&store.paths);
    let target = path.map_or_else(|| cwd.to_path_buf(), Path::to_path_buf);
    let normalized = MappingStore::normalize_path(&target);
    if mappings.remove(&target, None) {
        mappings.save()?;
        Ok(format!(
            "{} {}",
            printer::accent("Unmapped"),
            normalized.display()
        ))
    } else {
        Ok(printer::dimmed(&format!(
            "No mapping for {}",
            normalized.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_lines_quote_the_directory() {
        let dir = Path::new("/home/u/.ccsw/sessions/2-a_b.c");
        assert_eq!(
            Shell::Sh.export_line(dir),
            "export CODEX_HOME='/home/u/.ccsw/sessions/2-a_b.c'"
        );
        assert_eq!(
            Shell::Fish.export_line(dir),
            "set -gx CODEX_HOME '/home/u/.ccsw/sessions/2-a_b.c'"
        );
        assert_eq!(
            Shell::Pwsh.export_line(dir),
            "$env:CODEX_HOME = '/home/u/.ccsw/sessions/2-a_b.c'"
        );
        assert_eq!(Shell::Sh.unset_line("CODEX_HOME"), "unset CODEX_HOME");
        assert_eq!(Shell::Fish.unset_line("CODEX_HOME"), "set -e CODEX_HOME");
        assert_eq!(
            Shell::Pwsh.unset_line("CODEX_HOME"),
            "Remove-Item Env:CODEX_HOME -ErrorAction SilentlyContinue"
        );
        assert_eq!(
            Shell::Sh.export_line(Path::new("/it's")),
            "export CODEX_HOME='/it'\\''s'"
        );
        assert_eq!(
            Shell::Pwsh.export_line(Path::new("/it's")),
            "$env:CODEX_HOME = '/it''s'"
        );
        assert_eq!(Shell::parse("fish"), Some(Shell::Fish));
        assert_eq!(Shell::parse("zsh"), None);
    }

    /// Slot 1 is a Codex account, slot 2 a Claude Code one.
    fn mixed_roster() -> (tempfile::TempDir, Store, Roster) {
        let (dir, store) = crate::store::temp_store();
        let mut roster = Roster::empty();
        roster.add_record(1, crate::model::AccountRecord::new("codex@example.com"));
        let mut claude = crate::model::AccountRecord::new("claude@example.com");
        claude.provider = Provider::Claude;
        roster.add_record(2, claude);
        credentials::write(
            &store,
            2,
            &serde_json::json!({"claudeAiOauth": {"accessToken": "a"}}),
        )
        .unwrap();
        (dir, store, roster)
    }

    fn assert_claude_refused(err: CcswError) {
        assert_eq!(err.type_name(), "SessionError");
        assert_eq!(
            err.to_string(),
            "Session mode for Claude Code accounts arrives in a later release; use `ccsw switch <slot>` for now."
        );
    }

    #[test]
    fn run_refuses_a_claude_slot() {
        let (dir, store, roster) = mixed_roster();
        let err = resolve_run_target(&store, &roster, Some("2"), dir.path()).unwrap_err();
        assert_claude_refused(err);
        assert!(!store.paths.sessions_dir().exists(), "nothing was written");
    }

    #[test]
    fn env_refuses_a_claude_slot() {
        let (dir, store, roster) = mixed_roster();
        let request = EnvRequest {
            account: Some("2"),
            cwd: dir.path(),
            shell: Shell::Sh,
            unset: false,
            opts: ShareOptions::default(),
        };
        let err = plan_env(&store, &roster, &HostEnv::default(), request).unwrap_err();
        assert_claude_refused(err);
        assert!(!store.paths.sessions_dir().exists(), "nothing was written");
    }

    #[test]
    fn map_refuses_a_claude_slot() {
        let (dir, store, roster) = mixed_roster();
        let err = map(&store, &roster, Some("2"), None, dir.path()).unwrap_err();
        assert_claude_refused(err);
        assert!(!store.paths.mappings_file().exists(), "nothing was written");
        assert!(MappingStore::load(&store.paths).is_empty());
    }

    #[test]
    fn source_home_skips_a_pinned_profile() {
        let (dir, store) = crate::store::temp_store();
        assert_eq!(source_home(&store), store.paths.codex_home);
        let pinned = crate::paths::Paths::from_values(
            Some(store.paths.backup_root.clone()),
            Some(store.paths.sessions_dir().join("2-x")),
            None,
            dir.path(),
        )
        .unwrap();
        let pinned_store = Store::open(pinned);
        assert_eq!(
            source_home(&pinned_store),
            dirs::home_dir().unwrap().join(".codex")
        );
    }
}
