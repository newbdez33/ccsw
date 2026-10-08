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
pub const CLAUDE_MISSING: &str = "'claude' was not found on PATH. Install Claude Code first.";
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
    pub claude: Option<PathBuf>,
    pub claude_home_preset: Option<String>,
}

impl HostEnv {
    pub fn detect() -> Self {
        Self {
            codex_home_preset: std::env::var("CODEX_HOME").ok().filter(|v| !v.is_empty()),
            claude_home_preset: std::env::var("CLAUDE_CONFIG_DIR")
                .ok()
                .filter(|v| !v.is_empty()),
            claude: command_on_path("claude"),
            set_vars: SCRUBBED_ENV
                .iter()
                .chain(crate::claude::session::SCRUBBED_ENV)
                .filter(|name| std::env::var_os(name).is_some())
                .map(|name| name.to_string())
                .collect(),
            codex: command_on_path("codex"),
        }
    }

    fn executable(&self, provider: Provider) -> Result<&Path> {
        let (executable, message) = match provider {
            Provider::Codex => (&self.codex, CODEX_MISSING),
            Provider::Claude => (&self.claude, CLAUDE_MISSING),
        };
        executable
            .as_deref()
            .ok_or_else(|| CcswError::session(message))
    }

    fn preset(&self, provider: Provider) -> Option<&str> {
        match provider {
            Provider::Codex => self.codex_home_preset.as_deref(),
            Provider::Claude => self.claude_home_preset.as_deref(),
        }
    }

    fn overrides(&self, provider: Provider) -> Vec<&str> {
        let names = auth_overrides(provider);
        self.set_vars
            .iter()
            .map(String::as_str)
            .filter(|name| names.contains(name))
            .collect()
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

fn home_variable(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "CODEX_HOME",
        Provider::Claude => "CLAUDE_CONFIG_DIR",
    }
}

fn auth_overrides(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Codex => SCRUBBED_ENV,
        Provider::Claude => crate::claude::session::SCRUBBED_ENV,
    }
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

pub fn mapped_account(
    store: &Store,
    roster: &Roster,
    cwd: &Path,
    provider: Provider,
) -> MappedAccount {
    match MappingStore::load(&store.paths).resolve(provider, cwd) {
        None => MappedAccount::None,
        Some((_, identity)) => match roster.find_slot(provider, &identity) {
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
        provider: Provider,
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
    let selected = account.and_then(Provider::parse_selector);
    if let Some(identifier) = account.filter(|_| selected.is_none()) {
        return Ok(RunTarget::Slot(resolve_slot(roster, identifier)?));
    }
    let provider = match selected {
        Some(provider) => provider,
        None => {
            let mappings = MappingStore::load(&store.paths);
            let mapped: Vec<_> = Provider::ALL
                .into_iter()
                .filter(|p| mappings.resolve(*p, cwd).is_some())
                .collect();
            let providers: Vec<_> = if mapped.is_empty() {
                Provider::ALL
                    .into_iter()
                    .filter(|p| !roster.slots_of(*p).is_empty())
                    .collect()
            } else {
                mapped
            };
            match providers.as_slice() {
                [] => Provider::Codex,
                [provider] => *provider,
                _ => {
                    return Err(CcswError::session(
                        "Choose a provider: ccsw run codex or ccsw run claude (or pass an account).",
                    ));
                }
            }
        }
    };
    Ok(match mapped_account(store, roster, cwd, provider) {
        MappedAccount::Slot(slot) => RunTarget::Slot(slot),
        MappedAccount::Removed { email } => RunTarget::Default {
            provider,
            notice: Some(printer::yellowed(&format!(
                "Mapped account {email} no longer exists — launching the default account."
            ))),
        },
        MappedAccount::None => RunTarget::Default {
            provider,
            notice: Some(printer::dimmed(&format!(
                "No account mapped for {} — launching the default account.",
                cwd.display()
            ))),
        },
    })
}

impl RunTarget {
    pub fn provider(&self, roster: &Roster) -> Result<Provider> {
        match self {
            Self::Default { provider, .. } => Ok(*provider),
            Self::Slot(slot) => roster.record(*slot).map(|r| r.provider).ok_or_else(|| {
                CcswError::AccountNotFound(format!("Account-{slot} does not exist"))
            }),
        }
    }
}

/// A profile ready for launch.
#[derive(Debug)]
pub struct Prepared {
    pub(crate) reservation: Option<crate::claude::session::LaunchReservation>,
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
    prepare_profile_inner(store, roster, slot, opts, false)
}

fn prepare_profile_inner(
    store: &Store,
    roster: &Roster,
    slot: u32,
    opts: ShareOptions,
    launch: bool,
) -> Result<Prepared> {
    if opts.share_history && cfg!(windows) {
        return Err(CcswError::session(SHARE_HISTORY_WINDOWS));
    }
    let record = roster
        .record(slot)
        .ok_or_else(|| CcswError::AccountNotFound(format!("Account-{slot} does not exist")))?;
    if record.provider == Provider::Claude {
        return crate::claude::session::prepare(
            store,
            slot,
            record,
            &crate::claude::keychain::SystemSecurity,
            opts,
            launch,
        );
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
        reservation: None,
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
    sync_sharing_items(profile, source, opts, SHARED_ITEMS, HISTORY_ITEMS, false)
}

pub(crate) fn sync_claude_sharing(
    store: &Store,
    profile: &Path,
    opts: ShareOptions,
) -> Result<Vec<String>> {
    sync_sharing_items(
        profile,
        &store.paths.claude_default_home,
        opts,
        crate::claude::session::SHARED_ITEMS,
        crate::claude::session::HISTORY_ITEMS,
        true,
    )
}

fn sync_sharing_items(
    profile: &Path,
    source: &Path,
    opts: ShareOptions,
    shared_items: &[&str],
    history_items: &[&str],
    merge_history: bool,
) -> Result<Vec<String>> {
    let mut active: Vec<&str> = Vec::new();
    if opts.share {
        active.extend(shared_items);
    }
    if opts.share_history && !cfg!(windows) {
        active.extend(history_items);
    }
    let previous: Vec<_> = manifest_items(profile)
        .into_iter()
        .filter(|item| {
            shared_items.contains(&item.as_str()) || history_items.contains(&item.as_str())
        })
        .collect();
    let session_err =
        |path: &Path, err: &io::Error| fsutil::io_error(CcswError::Session, path, err);

    for item in previous
        .iter()
        .filter(|item| !active.contains(&item.as_str()))
    {
        let dest = profile.join(item);
        if history_items.contains(&item.as_str()) && dest.exists() && !dest.is_symlink() {
            continue;
        }
        remove_shared(&dest).map_err(|err| session_err(&dest, &err))?;
    }

    let mut notices = Vec::new();
    let mut shared: Vec<String> = Vec::new();
    for item in active {
        let src = source.join(item);
        let dest = profile.join(item);
        if merge_history && history_items.contains(&item) && !cfg!(windows) {
            if dest.exists() && !dest.is_symlink() && !crate::claude::session::is_quiescent(profile)
            {
                notices.push(printer::dimmed(&format!(
                    "Not sharing {item} yet: another session is using this profile."
                )));
                continue;
            }
            if let Err(err) = crate::claude::session::prepare_history_share(&src, &dest) {
                notices.push(printer::yellowed(&format!(
                    "Not sharing {item}: could not merge existing history ({err})."
                )));
                continue;
            }
        }
        if !src.exists() {
            if previous.iter().any(|name| name == item) {
                remove_shared(&dest).map_err(|err| session_err(&dest, &err))?;
            }
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
    reservation: Option<crate::claude::session::LaunchReservation>,
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
    require_session: bool,
) -> Result<Launch> {
    let provider = target.provider(roster)?;
    let executable = host.executable(provider)?;
    if provider == Provider::Codex {
        store.paths.validate_credential_store()?;
    }
    if opts.share_history && cfg!(windows) {
        return Err(CcswError::session(SHARE_HISTORY_WINDOWS));
    }
    let slot = match target {
        RunTarget::Default { notice, .. } => {
            if require_session {
                return Err(CcswError::session(
                    "No mapped account for an isolated session; pass an account.",
                ));
            }
            let mut command = Command::new(executable);
            command.args(&tail);
            return Ok(Launch {
                command,
                notices: notice.into_iter().collect(),
                session: None,
                reservation: None,
            });
        }
        RunTarget::Slot(slot) => slot,
    };
    let record = roster.record(slot).expect("target was checked");
    if provider == Provider::Claude && record.is_api_key() {
        return Err(CcswError::session(
            "Session mode requires an OAuth login or setup token.",
        ));
    }
    let mut notices = Vec::new();
    let variable = home_variable(provider);
    match host.preset(provider) {
        None if collect::live_login_for(store, roster, provider).slot() == Some(slot) => {
            if require_session {
                return Err(CcswError::session(
                    "This account is the active default login; --require-session refuses the plain launch. Select a different account for an isolated session.",
                ));
            }
            notices.push(format!("Account-{slot} ({}) is already the active default login — launching {provider} directly.", record.email));
            let mut command = Command::new(executable);
            command.args(&tail);
            return Ok(Launch {
                command,
                notices,
                session: None,
                reservation: None,
            });
        }
        Some(preset) => notices.push(printer::yellowed(&format!(
            "{variable} is already set ({preset}); overriding it for this launch."
        ))),
        None => {}
    }
    let prepared = prepare_profile_inner(store, roster, slot, opts, true)?;
    notices.extend(prepared.notices);
    let overrides = host.overrides(provider);
    if !overrides.is_empty() {
        notices.push(printer::yellowed(&format!(
            "Ignoring {} for this session — it would override the selected account inside {}.",
            overrides.join(", "),
            provider.tool_name()
        )));
    }
    notices.push(launching_line(slot, &prepared.email));
    let argv = match provider {
        Provider::Codex => embedded_codex_argv(codex_supports_no_daemon(executable), tail),
        Provider::Claude => tail,
    };
    let mut command = Command::new(executable);
    command.args(argv);
    for name in auth_overrides(provider) {
        command.env_remove(name);
    }
    command.env(variable, &prepared.profile);
    Ok(Launch {
        command,
        notices,
        session: Some((slot, prepared.profile)),
        reservation: prepared.reservation,
    })
}

/// POSIX: replace this process with the child (only returns on failure).
/// Elsewhere: wait for the child, fold rotated tokens back, mirror its status.
pub fn exec_or_wait(store: &Store, mut launch: Launch) -> Result<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The reservation survives exec; this PID becomes the child.
        let _ = store;
        let err = launch.command.exec();
        drop(launch.reservation.take());
        Err(CcswError::session(format!(
            "could not launch the selected CLI: {err}"
        )))
    }
    #[cfg(not(unix))]
    {
        let status = launch.command.status().map_err(|err| {
            CcswError::session(format!("could not launch the selected CLI: {err}"))
        })?;
        drop(launch.reservation.take());
        if let Some((slot, profile)) = &launch.session
            && let Err(err) = fold_back_provider(store, *slot, profile)
        {
            tracing::warn!("could not fold the session's tokens back into Account-{slot}: {err}");
        }
        Ok(status.code().unwrap_or(1))
    }
}

#[cfg(not(unix))]
fn fold_back_provider(store: &Store, slot: u32, profile: &Path) -> Result<()> {
    let roster = crate::store::roster::read_or_empty(&store.paths)?;
    if let Some(record) = roster.record(slot)
        && record.provider == Provider::Claude
    {
        return crate::claude::session::reconcile(
            store,
            slot,
            record,
            &crate::claude::keychain::SystemSecurity,
        );
    }
    fold_back(store, slot, profile).map(|_| ())
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
        self.export_provider(Provider::Codex, dir)
    }

    pub fn export_provider(self, provider: Provider, dir: &Path) -> String {
        let value = self.quote(&dir.to_string_lossy());
        let name = home_variable(provider);
        match self {
            Self::Sh => format!("export {name}={value}"),
            Self::Fish => format!("set -gx {name} {value}"),
            Self::Pwsh => format!("$env:{name} = {value}"),
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
        let providers =
            match account {
                Some(text) => vec![Provider::parse_selector(text).ok_or_else(|| {
                    CcswError::session("--unset accepts only a provider selector.")
                })?],
                None => Provider::ALL.to_vec(),
            };
        return Ok(EnvPlan::Lines {
            lines: providers
                .into_iter()
                .map(|p| shell.unset_line(home_variable(p)))
                .collect(),
            notices: Vec::new(),
        });
    }
    let slot = match resolve_run_target(store, roster, account, cwd)? {
        RunTarget::Slot(slot) => slot,
        RunTarget::Default { .. } => {
            return Err(CcswError::session(format!(
                "Nothing to prepare an environment for (no account given and no mapping for {}, or the mapped account no longer exists). Pass an account (ccsw env <NUM|EMAIL|ALIAS>), map this directory (ccsw map <NUM|EMAIL|ALIAS>), or clear a pinned profile with ccsw env --unset.",
                cwd.display()
            )));
        }
    };
    let record = roster
        .record(slot)
        .ok_or_else(|| CcswError::AccountNotFound(format!("Account-{slot} does not exist")))?;
    let provider = record.provider;
    if provider == Provider::Codex {
        store.paths.validate_credential_store()?;
    }
    if host.preset(provider).is_none()
        && collect::live_login_for(store, roster, provider).slot() == Some(slot)
    {
        return Ok(EnvPlan::Note(format!(
            "Account-{slot} ({}) is the active default login — an unpinned shell already uses it; nothing exported.",
            record.email
        )));
    }
    let prepared = prepare_profile(store, roster, slot, opts)?;
    let mut lines: Vec<String> = host
        .overrides(provider)
        .iter()
        .map(|name| shell.unset_line(name))
        .collect();
    lines.push(shell.export_provider(provider, &prepared.profile));
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
    let slot = resolve_slot(roster, identifier)?;
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
pub fn unmap(
    store: &Store,
    path: Option<&Path>,
    cwd: &Path,
    provider: Option<Provider>,
) -> Result<String> {
    let mut mappings = MappingStore::load(&store.paths);
    let target = path.map_or_else(|| cwd.to_path_buf(), Path::to_path_buf);
    let normalized = MappingStore::normalize_path(&target);
    if mappings.remove(&target, provider) {
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

    #[test]
    fn mixed_targets_accept_accounts_and_require_a_provider_for_defaults() {
        let (dir, store, roster) = mixed_roster();
        assert_eq!(
            resolve_run_target(&store, &roster, Some("2"), dir.path()).unwrap(),
            RunTarget::Slot(2)
        );
        assert!(resolve_run_target(&store, &roster, None, dir.path()).is_err());
        assert!(matches!(
            resolve_run_target(&store, &roster, Some("claude"), dir.path()).unwrap(),
            RunTarget::Default {
                provider: Provider::Claude,
                ..
            }
        ));
        map(&store, &roster, Some("2"), None, dir.path()).unwrap();
        assert_eq!(
            resolve_run_target(&store, &roster, None, dir.path()).unwrap(),
            RunTarget::Slot(2)
        );
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
