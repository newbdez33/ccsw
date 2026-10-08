//! The account façade: lifecycle, switching, resolution, snapshots.
//!
//! Every surface (CLI, TUI, auto engine) drives accounts through [`Switcher`].
//! Human lines go through a [`Ui`] so the CLI can color them and the TUI can
//! show them in a modal; JSON-shaped results come back as values.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::claude::credentials::{
    ClaudeCredential, CredentialKind, OAUTH_ACCOUNT_KEY, OauthAccount, SlotFile,
    looks_like_api_key, looks_like_setup_token,
};
use crate::claude::keychain::SystemSecurity;
use crate::claude::live::{ClaudeLive, LiveLogin, backup_live as backup_claude_live};
use crate::codex::app_server::{restart_daemon_if_live_auth_changed, snapshot_live_auth};
use crate::codex::auth::{AuthJson, AuthKind, backup_live};
use crate::collect::{self, CollectMode, CollectOptions};
use crate::errors::{CcswError, Result};
use crate::model::{
    AccountKind, AccountRecord, AccountRef, ActiveSlots, CurrentAccount, Identity, Roster,
    SwitchOutcome, now_unix,
};
use crate::printer;
use crate::provider::Provider;
use crate::store::poll_policy::replan_new_active;
use crate::store::usage_store::{UsageEntry, UsageSentinel, UsageStore};
use crate::store::{
    MappingStore, MoveOutcome, Settings, Store, alias_owner, credentials, normalize_alias,
    resolve_slot, roster,
};
use crate::usage_math::{headroom, relevant_windows};

// ---------------------------------------------------------------------------
// Human output
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Accent,
    Muted,
    Dimmed,
    Bold,
    BoldAccent,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub style: Style,
    pub text: String,
}

/// One styled line of human output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self::new().push(Style::Plain, text)
    }

    pub fn dimmed(text: impl Into<String>) -> Self {
        Self::new().push(Style::Dimmed, text)
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self::new().push(Style::Warning, text)
    }

    pub fn push(mut self, style: Style, text: impl Into<String>) -> Self {
        self.spans.push(Span {
            style,
            text: text.into(),
        });
        self
    }

    /// The text without styling.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// The text with the printer's colors applied per span.
    pub fn render(&self) -> String {
        self.spans
            .iter()
            .map(|span| match span.style {
                Style::Plain => span.text.clone(),
                Style::Accent => printer::accent(&span.text),
                Style::Muted => printer::muted(&span.text),
                Style::Dimmed => printer::dimmed(&span.text),
                Style::Bold => printer::bolded(&span.text),
                Style::BoldAccent => printer::bold_accent(&span.text),
                Style::Warning => printer::yellowed(&span.text),
            })
            .collect()
    }
}

/// Where human lines and prompts go.
pub trait Ui {
    fn say(&mut self, line: Line);
    /// A `[y/N]` question; only `y` / `yes` is a yes.
    fn confirm(&mut self, prompt: &str) -> bool;
    /// A free-text question; `None` on EOF or when prompts are not possible.
    fn ask(&mut self, prompt: &str) -> Option<String>;
}

/// Colored lines on stdout, prompts on stdin.
pub struct ConsoleUi;

impl Ui for ConsoleUi {
    fn say(&mut self, line: Line) {
        println!("{}", line.render());
    }

    fn confirm(&mut self, prompt: &str) -> bool {
        printer::confirm(prompt)
    }

    fn ask(&mut self, prompt: &str) -> Option<String> {
        print!("{prompt}");
        let _ = io::stdout().flush();
        let mut line = String::new();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim().to_string()),
        }
    }
}

/// Plain lines on stderr (JSON mode keeps stdout for the document); prompts
/// are refused.
pub struct StderrUi;

impl Ui for StderrUi {
    fn say(&mut self, line: Line) {
        eprintln!("{}", line.text());
    }

    fn confirm(&mut self, _prompt: &str) -> bool {
        false
    }

    fn ask(&mut self, _prompt: &str) -> Option<String> {
        None
    }
}

/// Drops everything; prompts are refused.
pub struct SilentUi;

impl Ui for SilentUi {
    fn say(&mut self, _line: Line) {}

    fn confirm(&mut self, _prompt: &str) -> bool {
        false
    }

    fn ask(&mut self, _prompt: &str) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    Added { slot: u32 },
    Updated { slot: u32 },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Rotation,
    NextAvailable,
    Best,
}

impl Strategy {
    /// `None` is the bare rotation.
    pub fn from_flag(flag: Option<&str>) -> Result<Self> {
        match flag {
            None => Ok(Self::Rotation),
            Some("best") => Ok(Self::Best),
            Some("next-available") => Ok(Self::NextAvailable),
            Some(other) => Err(CcswError::validation(format!("unknown strategy '{other}'"))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rotation => "rotation",
            Self::NextAvailable => "next-available",
            Self::Best => "best",
        }
    }
}

/// What a switch produced: the JSON body plus what the CLI prints afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchReport {
    pub outcome: SwitchOutcome,
    /// The daemon follow-up line, printed after the account list.
    pub followup: Option<String>,
    /// Print the account list after the switch line.
    pub show_list: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountRow {
    pub slot: u32,
    pub record: AccountRecord,
    pub usage: UsageEntry,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListSnapshot {
    /// The slot of each provider's live login, when managed.
    pub actives: ActiveSlots,
    pub rows: Vec<AccountRow>,
    pub warnings: Vec<String>,
}

/// One provider's live login and (when managed) its row.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderStatus {
    pub provider: Provider,
    pub current: CurrentAccount,
    pub row: Option<AccountRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusSnapshot {
    /// One entry per provider, in `Provider::ALL` order.
    pub providers: Vec<ProviderStatus>,
    pub total: usize,
}

// ---------------------------------------------------------------------------
// The façade
// ---------------------------------------------------------------------------

pub struct Switcher {
    pub store: Store,
    pub settings: Settings,
    pub ui: Box<dyn Ui>,
}

fn no_accounts() -> CcswError {
    CcswError::config("No accounts are managed yet")
}

fn missing(slot: u32) -> CcswError {
    CcswError::AccountNotFound(format!("Account-{slot} does not exist"))
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

/// cswap's email check: `^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$`.
pub fn valid_email(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c));
    let Some((host, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    local_ok
        && !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
        && tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic())
}

/// What `add` found logged in for one provider.
enum Capture {
    Codex(AuthJson),
    Claude(LiveLogin),
}

fn no_login_error(provider: Option<Provider>) -> CcswError {
    match provider {
        Some(provider) => CcswError::config(format!(
            "No active {} account found. Please log in first.",
            provider.title()
        )),
        None => CcswError::config("No active Codex or Claude login found. Log in first."),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    OpenAiKey,
    ClaudeApiKey,
    SetupToken,
}

impl TokenKind {
    fn detect(token: &str) -> Self {
        if looks_like_api_key(token) {
            Self::ClaudeApiKey
        } else if looks_like_setup_token(token) {
            Self::SetupToken
        } else {
            Self::OpenAiKey
        }
    }

    fn provider(self) -> Provider {
        match self {
            Self::OpenAiKey => Provider::Codex,
            _ => Provider::Claude,
        }
    }

    fn is_api_key(self) -> bool {
        self != Self::SetupToken
    }

    fn email_prefix(self) -> &'static str {
        match self {
            Self::SetupToken => "setup-token",
            _ => "api-key",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::SetupToken => "from token",
            _ => "from API key",
        }
    }

    fn what(self) -> &'static str {
        match self {
            Self::SetupToken => "token",
            _ => "API key",
        }
    }
}

fn slot_arg(slot: Option<i64>) -> Result<Option<u32>> {
    match slot {
        None => Ok(None),
        Some(n) if n < 1 => Err(CcswError::config("Slot number must be >= 1")),
        Some(n) => u32::try_from(n)
            .map(Some)
            .map_err(|_| CcswError::config("Slot number must be >= 1")),
    }
}

fn account_label(slot: u32, email: &str) -> String {
    format!("Account-{slot} ({email})")
}

/// The email shown for a live login: the JWT's, else a placeholder.
fn live_email(live: &AuthJson) -> String {
    match live.kind() {
        AuthKind::ChatGpt => live
            .identity()
            .map(|id| id.email)
            .or_else(|| live.account_info().email.map(|e| e.to_lowercase()))
            .unwrap_or_else(|| "unknown".to_string()),
        AuthKind::ApiKey => "api-key".to_string(),
        AuthKind::Unknown => "unknown".to_string(),
    }
}

/// Splice `oauthAccount` into the global config, leaving every other key.
fn splice_oauth_account(live_api: &ClaudeLive, account: Value) -> Result<()> {
    #[cfg(test)]
    if tests::FAIL_CONFIG_SPLICE.with(std::cell::Cell::get) {
        return Err(CcswError::credential_write("injected config failure"));
    }
    live_api.update_global_config(|config| {
        config.insert(OAUTH_ACCOUNT_KEY.to_string(), account);
    })
}

/// The email shown for a live Claude login: `oauthAccount`'s, else a placeholder.
fn claude_live_email(live: &LiveLogin) -> String {
    let email = live
        .oauth_account
        .as_ref()
        .map(|a| a.email_address().to_lowercase())
        .unwrap_or_default();
    if !email.is_empty() {
        return email;
    }
    match live.credential.as_ref().map(ClaudeCredential::kind) {
        Some(CredentialKind::ApiKey) => "api-key".to_string(),
        _ => "unknown".to_string(),
    }
}

fn empty_entry(sentinel: Option<UsageSentinel>) -> UsageEntry {
    UsageEntry {
        sentinel,
        last_good: None,
        fetched_at: None,
        age_s: None,
        last_attempt_at: None,
        consecutive_failures: 0,
        last_error: None,
        backoff_until: None,
        next_poll_at: None,
        poll_interval_s: None,
        last_429_at: None,
        auth_dead_strikes: 0,
        struck_fingerprint: None,
        trust_extended: false,
    }
}

fn rename_if_exists(from: &Path, to: &Path) -> Result<()> {
    if !from.exists() {
        return Ok(());
    }
    fs::rename(from, to).map_err(|err| {
        CcswError::config(format!(
            "could not move {} to {}: {err}",
            from.display(),
            to.display()
        ))
    })
}

/// Exchange two paths (either may be absent) through a staging name.
fn swap_paths(a: &Path, b: &Path) -> Result<()> {
    if !a.exists() {
        return rename_if_exists(b, a);
    }
    if !b.exists() {
        return rename_if_exists(a, b);
    }
    let staging = PathBuf::from(format!("{}.swapping", a.display()));
    rename_if_exists(a, &staging)?;
    rename_if_exists(b, a)?;
    rename_if_exists(&staging, b)
}

/// The roster holds a Codex record (or cannot be read), or Codex has a live
/// `auth.json`.
fn codex_in_use(paths: &crate::paths::Paths) -> bool {
    paths.live_auth_file().exists()
        || roster::read(paths).map_or(true, |roster| {
            roster.is_some_and(|r| !r.slots_of(Provider::Codex).is_empty())
        })
}

enum Placement {
    Refreshed(u32),
    Placed { slot: u32, moved_from: Option<u32> },
    Cancelled,
}

impl Switcher {
    /// The store from the environment, with the Codex credential-store gate
    /// once Codex is in use (spec §5): a Claude-only store skips it.
    pub fn from_env() -> Result<Self> {
        let store = Store::from_env()?;
        if codex_in_use(&store.paths) {
            store.paths.validate_credential_store()?;
        }
        Ok(Self::open(store))
    }

    pub fn open(store: Store) -> Self {
        let settings = Settings::load(&store.paths);
        Self {
            store,
            settings,
            ui: Box::new(ConsoleUi),
        }
    }

    /// Strict roster read; absent → `No accounts are managed yet`.
    pub fn roster(&self) -> Result<Roster> {
        roster::read(&self.store.paths)?.ok_or_else(no_accounts)
    }

    /// Strict roster read; `None` when no roster exists yet.
    pub fn roster_opt(&self) -> Result<Option<Roster>> {
        roster::read(&self.store.paths)
    }

    fn write_roster(&self, roster: &Roster) -> Result<()> {
        roster::write(&self.store.paths, roster)
    }

    /// What `$CODEX_HOME/auth.json` holds, resolved against the roster.
    pub fn current_account(&self) -> Result<CurrentAccount> {
        let roster = roster::read_or_empty(&self.store.paths)?;
        Ok(collect::live_login(&self.store, &roster))
    }

    /// Number → alias → email, landing on an existing record.
    pub fn resolve(&self, roster: &Roster, identifier: &str) -> Result<u32> {
        resolve_slot(roster, identifier)
    }

    fn say(&mut self, line: Line) {
        self.ui.say(line);
    }

    /// The managed slot holding this live credential: identity for a ChatGPT
    /// login, the key string for an API key.
    fn slot_of_live(&self, roster: &Roster, live: &AuthJson) -> Option<u32> {
        match live.kind() {
            AuthKind::ChatGpt => live
                .identity()
                .and_then(|id| roster.find_slot(Provider::Codex, &id)),
            AuthKind::ApiKey => {
                let key = live.api_key()?;
                roster.sequence.iter().copied().find(|slot| {
                    roster.record(*slot).is_some_and(|r| r.is_api_key())
                        && credentials::read(&self.store, *slot)
                            .ok()
                            .flatten()
                            .is_some_and(|v| AuthJson::from_value(v).api_key() == Some(key))
                })
            }
            AuthKind::Unknown => None,
        }
    }

    /// Caller holds the store lock. Capture Keychain-only rotations before a
    /// path change makes the old hashed service inaccessible.
    fn prepare_session_change(&self, slot: u32, record: &AccountRecord) -> Result<()> {
        if record.provider == Provider::Claude {
            let profile = self.store.paths.session_dir(slot, &record.email);
            crate::claude::session::require_quiescent(&profile)?;
            crate::claude::session::reconcile_locked(&self.store, slot, record, &SystemSecurity)?;
            crate::claude::session::materialize(&self.store, &profile, &SystemSecurity)?;
        }
        Ok(())
    }

    fn remove_session_dir(&self, slot: u32, email: &str) {
        let dir = self.store.paths.session_dir(slot, email);
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!("could not remove {}: {err}", dir.display()),
        }
    }

    // -- resolution with cswap's interactive disambiguation ------------------

    /// `switch <id>` / `remove <id>` resolution: digits, alias or a
    /// format-valid email; an email matching several accounts prompts in
    /// interactive mode. `None` means the prompt was cancelled.
    fn resolve_for_prompt(
        &mut self,
        roster: &Roster,
        identifier: &str,
        action: &str,
        interactive: bool,
    ) -> Result<Option<u32>> {
        let aliased = alias_owner(roster, identifier).is_some();
        if !is_digits(identifier) && !aliased && !valid_email(identifier) {
            return Err(CcswError::validation(format!(
                "Invalid account identifier: {identifier}"
            )));
        }
        let matches: Vec<u32> = if is_digits(identifier) || aliased {
            Vec::new()
        } else {
            roster
                .sorted_slots()
                .into_iter()
                .filter(|slot| roster.record(*slot).is_some_and(|r| r.email == identifier))
                .collect()
        };
        if matches.len() < 2 || !interactive {
            return resolve_slot(roster, identifier).map(Some);
        }
        self.say(Line::plain(format!(
            "Multiple accounts found for '{identifier}':"
        )));
        for slot in &matches {
            let record = roster.record(*slot).expect("matched record");
            self.say(
                Line::plain(format!("  {slot}: {} ", record.email))
                    .push(Style::Muted, format!("[{}]", record.display_tag())),
            );
        }
        let answer = self
            .ui
            .ask(&format!("Enter account number to {action}: "))
            .and_then(|a| a.trim().parse::<u32>().ok())
            .filter(|n| matches.contains(n));
        if answer.is_none() {
            self.say(Line::dimmed("Cancelled"));
        }
        Ok(answer)
    }

    // -- add ------------------------------------------------------------------

    /// The live login of `provider`, resolved against the roster.
    pub fn current_account_for(&self, provider: Provider) -> Result<CurrentAccount> {
        let roster = roster::read_or_empty(&self.store.paths)?;
        Ok(collect::live_login_for(&self.store, &roster, provider))
    }

    fn capture_live(&self, provider: Provider) -> Result<Option<Capture>> {
        match provider {
            Provider::Codex => {
                Ok(AuthJson::read(&self.store.paths.live_auth_file())?.map(Capture::Codex))
            }
            Provider::Claude => {
                let login = ClaudeLive::new(&self.store.paths, &SystemSecurity).read()?;
                if login.credential.is_none() && login.keychain_unavailable {
                    return Err(CcswError::credential_read(
                        "The macOS Keychain is unavailable, so the current Claude login cannot be read. Unlock the Keychain and retry.",
                    ));
                }
                Ok(login.credential.is_some().then_some(Capture::Claude(login)))
            }
        }
    }

    /// Snapshot the live login(s) into the store: both providers without a
    /// selector, each new login added and each managed one refreshed in place.
    pub fn add_accounts(
        &mut self,
        provider: Option<Provider>,
        slot: Option<i64>,
        alias: Option<&str>,
    ) -> Result<Vec<(Provider, AddOutcome)>> {
        let requested = slot_arg(slot)?;
        let alias = alias
            .map(|a| normalize_alias(a).map_err(|e| CcswError::validation(e.to_string())))
            .transpose()?;
        let providers: Vec<Provider> = provider.map_or_else(|| Provider::ALL.to_vec(), |p| vec![p]);
        if provider == Some(Provider::Codex) {
            self.store.paths.validate_credential_store()?;
        }
        let mut captures = Vec::new();
        for candidate in providers {
            if let Some(capture) = self.capture_live(candidate)? {
                captures.push((candidate, capture));
            }
        }
        if captures.is_empty() {
            return Err(no_login_error(provider));
        }
        if captures.len() > 1 && (requested.is_some() || alias.is_some()) {
            return Err(CcswError::validation(
                "--slot/--alias need a single login; both a Codex and a Claude login were found. Say which: ccsw add codex … or ccsw add claude …",
            ));
        }
        let _lock = self.store.lock()?;
        let mut outcomes = Vec::new();
        for (candidate, capture) in captures {
            let outcome = match capture {
                Capture::Codex(auth) => self.add_auth(&auth, requested, alias.clone())?,
                Capture::Claude(login) => self.add_claude(&login, requested, alias.clone())?,
            };
            outcomes.push((candidate, outcome));
        }
        if provider.is_none()
            && outcomes.len() == 2
            && outcomes
                .iter()
                .all(|(_, o)| matches!(o, AddOutcome::Updated { .. }))
        {
            let slot_of = |wanted: Provider| {
                outcomes
                    .iter()
                    .find_map(|(p, o)| match o {
                        AddOutcome::Updated { slot } if *p == wanted => Some(*slot),
                        _ => None,
                    })
                    .unwrap_or_default()
            };
            self.say(Line::dimmed(format!(
                "Both current logins were already managed: Account-{} (codex), Account-{} (claude) — nothing new was added.",
                slot_of(Provider::Codex),
                slot_of(Provider::Claude)
            )));
        }
        Ok(outcomes)
    }

    /// `add` for one provider.
    pub fn add_account(
        &mut self,
        provider: Provider,
        slot: Option<i64>,
        alias: Option<&str>,
    ) -> Result<AddOutcome> {
        Ok(self
            .add_accounts(Some(provider), slot, alias)?
            .into_iter()
            .next()
            .map_or(AddOutcome::Cancelled, |(_, outcome)| outcome))
    }

    /// The managed Claude slot holding this live login.
    fn claude_slot_of_live(&self, roster: &Roster, live: &LiveLogin) -> Option<u32> {
        let credential = live.credential.as_ref()?;
        match credential.kind() {
            CredentialKind::ApiKey => self.claude_slot_of_key(roster, credential.api_key()),
            CredentialKind::OAuth | CredentialKind::SetupToken => live
                .identity()
                .and_then(|id| roster.find_slot(Provider::Claude, &id)),
            CredentialKind::Unknown => None,
        }
    }

    fn claude_slot_of_key(&self, roster: &Roster, key: Option<&str>) -> Option<u32> {
        let key = key?;
        roster.slots_of(Provider::Claude).into_iter().find(|slot| {
            roster.record(*slot).is_some_and(|r| r.is_api_key())
                && credentials::read(&self.store, *slot)
                    .ok()
                    .flatten()
                    .and_then(|v| SlotFile::from_value(&v).ok())
                    .is_some_and(|s| s.credential.api_key() == Some(key))
        })
    }

    /// The caller holds the store lock.
    fn add_claude(
        &mut self,
        login: &LiveLogin,
        requested: Option<u32>,
        alias: Option<String>,
    ) -> Result<AddOutcome> {
        let mut roster = roster::init_if_absent(&self.store.paths)?;
        let credential = login
            .credential
            .as_ref()
            .ok_or_else(|| no_login_error(Some(Provider::Claude)))?;
        let (record, existing, suffix) = match credential.kind() {
            CredentialKind::OAuth | CredentialKind::SetupToken => {
                let account = login.oauth_account.as_ref().ok_or_else(|| {
                    CcswError::credential_read(
                        "the Claude Code login carries no oauthAccount; log in with Claude Code first",
                    )
                })?;
                let identity = account.identity().ok_or_else(|| {
                    CcswError::credential_read("the Claude Code login carries no email address")
                })?;
                let mut record = AccountRecord::new(identity.email.clone());
                record.provider = Provider::Claude;
                record.uuid = account.account_uuid();
                record.organization_uuid = identity.account_id.clone();
                record.organization_name = account.organization_name();
                let existing = roster.find_slot(Provider::Claude, &identity);
                (record, existing, None)
            }
            CredentialKind::ApiKey => {
                let existing = self.claude_slot_of_key(&roster, credential.api_key());
                let slot = existing
                    .or(requested)
                    .unwrap_or_else(|| roster.next_free_slot());
                let mut record = AccountRecord::new(format!("api-key-{slot}@token.local"));
                record.provider = Provider::Claude;
                record.kind = Some(AccountKind::ApiKey);
                (record, existing, Some("from API key"))
            }
            CredentialKind::Unknown => {
                return Err(CcswError::credential_read(
                    "the Claude Code login holds neither an OAuth credential nor an API key",
                ));
            }
        };
        let oauth_account = match (credential.kind(), &login.oauth_account) {
            (CredentialKind::ApiKey, _) | (_, None) => OauthAccount::synthesized(&record.email),
            (_, Some(account)) => account.clone(),
        };
        let value = SlotFile::new(credential, oauth_account).to_value();
        let placement = self.place(&mut roster, record, existing, requested, alias, &value)?;
        Ok(self.announce_placement(&roster, placement, suffix, "credentials"))
    }

    /// Save and activate a completed browser login under one store lock.
    pub fn add_browser_account(&mut self, auth: &AuthJson) -> Result<AddOutcome> {
        crate::codex::login::validate_login(auth)?;
        self.store.paths.validate_credential_store()?;
        let live_path = self.store.paths.live_auth_file();
        let lock = self.store.lock()?;
        let roster = roster::read_or_empty(&self.store.paths)?;
        // Preserve rotations from the departing account. For the same account,
        // newly issued tokens win even when the old timestamp is ahead.
        if let Some(live) = AuthJson::read(&live_path)?
            && live.identity() != auth.identity()
            && let Some(slot) = self.slot_of_live(&roster, &live)
        {
            let stored = credentials::read(&self.store, slot)?.map(AuthJson::from_value);
            if stored.is_none_or(|stored| live.is_newer_than(&stored)) {
                credentials::write(&self.store, slot, &live.0)?;
            }
        }
        let before = snapshot_live_auth(&live_path);
        let outcome = self.add_auth(auth, None, None)?;
        backup_live(&live_path)?;
        auth.write(&live_path)?;
        let roster = self.roster()?;
        drop(lock);
        if let AddOutcome::Added { slot } | AddOutcome::Updated { slot } = outcome {
            let restart = restart_daemon_if_live_auth_changed(&before, &live_path);
            if let Some(followup) = restart.message(&format!("Account-{slot}")) {
                self.say(Line::plain(followup));
            }
            if let Some(record) = roster.record(slot) {
                self.replan_active(slot, record);
            }
        }
        Ok(outcome)
    }

    /// The caller holds the store lock.
    fn add_auth(
        &mut self,
        live: &AuthJson,
        requested: Option<u32>,
        alias: Option<String>,
    ) -> Result<AddOutcome> {
        let live_path = self.store.paths.live_auth_file();
        let mut roster = roster::init_if_absent(&self.store.paths)?;
        let (record, existing, from_api_key) = match live.kind() {
            AuthKind::ChatGpt => {
                let info = live.account_info();
                let identity = live.identity().ok_or_else(|| {
                    CcswError::credential_read(format!(
                        "{}: the login carries no email or account id",
                        live_path.display()
                    ))
                })?;
                let mut record = AccountRecord::new(identity.email.clone());
                record.uuid = info.user_id.unwrap_or_default();
                record.organization_uuid = identity.account_id.clone();
                record.organization_name = info.workspace_name.unwrap_or_default();
                record.plan_type = info.plan_type;
                let existing = roster.find_slot(Provider::Codex, &identity);
                (record, existing, false)
            }
            AuthKind::ApiKey => {
                let existing = self.slot_of_live(&roster, live);
                let slot = existing
                    .or(requested)
                    .unwrap_or_else(|| roster.next_free_slot());
                let mut record = AccountRecord::new(format!("api-key-{slot}@token.local"));
                record.kind = Some(AccountKind::ApiKey);
                (record, existing, true)
            }
            AuthKind::Unknown => {
                return Err(CcswError::credential_read(format!(
                    "{}: holds neither a ChatGPT login nor an API key",
                    live_path.display()
                )));
            }
        };
        let placement = self.place(&mut roster, record, existing, requested, alias, &live.0)?;
        Ok(self.announce_placement(
            &roster,
            placement,
            from_api_key.then_some("from API key"),
            "credentials",
        ))
    }

    /// Register an API key or setup-token, routed by its prefix.
    pub fn add_token(
        &mut self,
        token: &str,
        email: Option<&str>,
        slot: Option<i64>,
    ) -> Result<AddOutcome> {
        let requested = slot_arg(slot)?;
        let token = token.trim();
        if token.is_empty() {
            return Err(CcswError::validation("Token cannot be empty"));
        }
        if token.starts_with('{') {
            return Err(CcswError::validation(
                "Token must be an API key or setup-token, not a JSON object",
            ));
        }
        if let Some(email) = email
            && !valid_email(email)
        {
            return Err(CcswError::validation(format!(
                "Invalid email format: {email}"
            )));
        }
        let kind = TokenKind::detect(token);
        if kind.provider() == Provider::Codex {
            self.store.paths.validate_credential_store()?;
        }
        let _lock = self.store.lock()?;
        let mut roster = roster::init_if_absent(&self.store.paths)?;
        let email = match email {
            Some(email) => email.to_string(),
            None => format!(
                "{}-{}@token.local",
                kind.email_prefix(),
                requested.unwrap_or_else(|| roster.next_free_slot())
            ),
        };
        let identity = Identity::new(email.clone(), "");
        let existing = roster.find_slot(kind.provider(), &identity);
        if let Some(slot) = existing
            && roster
                .record(slot)
                .is_some_and(|r| r.is_api_key() != kind.is_api_key())
        {
            return Err(CcswError::validation(format!(
                "'{email}' already exists as an OAuth account (slot {slot}); cannot add it as an API-key account. Pass a distinct --email."
            )));
        }
        let mut record = AccountRecord::new(email.clone());
        record.provider = kind.provider();
        if kind.is_api_key() {
            record.kind = Some(AccountKind::ApiKey);
        }
        let creds = match kind {
            TokenKind::OpenAiKey => AuthJson::api_key_auth(token).0,
            TokenKind::ClaudeApiKey => SlotFile::new(
                &ClaudeCredential::managed_key(token),
                OauthAccount::synthesized(&email),
            )
            .to_value(),
            TokenKind::SetupToken => SlotFile::new(
                &ClaudeCredential::wrap_setup_token(token),
                OauthAccount::synthesized(&email),
            )
            .to_value(),
        };
        let placement = self.place(&mut roster, record, existing, requested, None, &creds)?;
        Ok(self.announce_placement(&roster, placement, Some(kind.suffix()), kind.what()))
    }

    /// Put `record` + `creds` into a slot per cswap's rules: refresh in place,
    /// migrate the same identity, or displace a different occupant after a prompt.
    fn place(
        &mut self,
        roster: &mut Roster,
        mut record: AccountRecord,
        existing: Option<u32>,
        requested: Option<u32>,
        alias: Option<String>,
        creds: &Value,
    ) -> Result<Placement> {
        let target = match (existing, requested) {
            (Some(slot), None) => {
                return self
                    .refresh_in_place(roster, slot, record, alias, creds)
                    .map(Placement::Refreshed);
            }
            (Some(slot), Some(requested)) if slot == requested => {
                return self
                    .refresh_in_place(roster, slot, record, alias, creds)
                    .map(Placement::Refreshed);
            }
            (_, Some(requested)) => requested,
            (None, None) => roster.next_free_slot(),
        };
        if let Some(alias) = &alias
            && let Some(owner) = alias_owner(roster, alias)
            && Some(owner) != existing
            && owner != target
        {
            return Err(CcswError::validation(format!(
                "Alias '{alias}' is already used by account {owner}"
            )));
        }
        if let Some(occupant) = roster.record(target).cloned() {
            self.say(Line::warning(format!("Slot {target} already occupied")));
            self.say(
                Line::plain(format!("{} ", occupant.email))
                    .push(Style::Muted, format!("[{}]", occupant.display_tag())),
            );
            if !self.ui.confirm(&format!("Overwrite slot {target}? [y/N] ")) {
                self.say(Line::dimmed("Cancelled"));
                return Ok(Placement::Cancelled);
            }
            self.prepare_session_change(target, &occupant)?;
            credentials::delete(&self.store, target)?;
            self.remove_session_dir(target, &occupant.email);
            roster.remove_slot(target);
            let mut mappings = MappingStore::load(&self.store.paths);
            let pruned = mappings.prune(occupant.provider, &occupant.identity());
            if pruned > 0 {
                mappings.save()?;
                self.say(Line::dimmed(format!(
                    "Removed {pruned} directory mapping(s) for this account"
                )));
            }
        }
        if let Some(old) = existing {
            let old_record = roster.remove_slot(old).ok_or_else(|| missing(old))?;
            if alias.is_none() {
                record.alias = old_record.alias.clone();
            }
            record.disabled = old_record.disabled;
            credentials::delete(&self.store, old)?;
            self.remove_session_dir(old, &old_record.email);
            if record.is_api_key() {
                // A live API key keeps the email it was registered under.
                record.email = old_record.email;
            }
        }
        if let Some(alias) = alias {
            record.alias = Some(alias);
        }
        credentials::write(&self.store, target, creds)?;
        let provider = record.provider;
        roster.add_record(target, record);
        roster.set_active_for(provider, Some(target));
        self.write_roster(roster)?;
        UsageStore::new(&self.store.paths).clear_dead_token(&[target])?;
        Ok(Placement::Placed {
            slot: target,
            moved_from: existing,
        })
    }

    fn refresh_in_place(
        &mut self,
        roster: &mut Roster,
        slot: u32,
        fresh: AccountRecord,
        alias: Option<String>,
        creds: &Value,
    ) -> Result<u32> {
        if let Some(alias) = &alias
            && let Some(owner) = alias_owner(roster, alias)
            && owner != slot
        {
            return Err(CcswError::validation(format!(
                "Alias '{alias}' is already used by account {owner}"
            )));
        }
        credentials::write(&self.store, slot, creds)?;
        let record = roster.record_mut(slot).ok_or_else(|| missing(slot))?;
        if !fresh.organization_name.is_empty() {
            record.organization_name = fresh.organization_name;
        }
        if fresh.plan_type.is_some() {
            record.plan_type = fresh.plan_type;
        }
        if record.uuid.is_empty() {
            record.uuid = fresh.uuid;
        }
        if let Some(alias) = alias {
            record.alias = Some(alias);
        }
        let provider = roster.record(slot).map(|r| r.provider).unwrap_or_default();
        roster.set_active_for(provider, Some(slot));
        self.write_roster(roster)?;
        UsageStore::new(&self.store.paths).clear_dead_token(&[slot])?;
        Ok(slot)
    }

    fn announce_placement(
        &mut self,
        roster: &Roster,
        placement: Placement,
        suffix: Option<&str>,
        what: &str,
    ) -> AddOutcome {
        let describe = |roster: &Roster, slot: u32| {
            let record = roster.record(slot).expect("placed record");
            (record.email.clone(), record.display_tag())
        };
        match placement {
            Placement::Cancelled => AddOutcome::Cancelled,
            Placement::Refreshed(slot) => {
                let (email, tag) = describe(roster, slot);
                self.say(
                    Line::new()
                        .push(Style::Accent, format!("Updated {what}"))
                        .push(Style::Plain, format!(" for Account {slot} ({email} "))
                        .push(Style::Muted, format!("[{tag}]"))
                        .push(Style::Plain, ")."),
                );
                AddOutcome::Updated { slot }
            }
            Placement::Placed { slot, moved_from } => {
                if let Some(old) = moved_from {
                    self.say(Line::dimmed(format!("Moved from slot {old} → {slot}")));
                }
                let (email, tag) = describe(roster, slot);
                let mut line = Line::new()
                    .push(Style::Accent, "Added")
                    .push(Style::Plain, format!(" Account {slot}: {email} "))
                    .push(Style::Muted, format!("[{tag}]"));
                if let Some(suffix) = suffix {
                    line = line.push(Style::Plain, format!(" ({suffix})"));
                }
                self.say(line);
                AddOutcome::Added { slot }
            }
        }
    }

    // -- remove / disable / enable ------------------------------------------

    /// Returns `false` when the user cancelled.
    pub fn remove(&mut self, identifier: &str, interactive: bool) -> Result<bool> {
        let roster = self.roster()?;
        let Some(slot) = self.resolve_for_prompt(&roster, identifier, "remove", interactive)?
        else {
            return Ok(false);
        };
        let record = roster.record(slot).cloned().ok_or_else(|| missing(slot))?;
        if self.current_account_for(record.provider)?.slot() == Some(slot) {
            self.say(Line::warning(format!(
                "Warning: {} is currently active",
                account_label(slot, &record.email)
            )));
        }
        if !self.ui.confirm(&format!(
            "Are you sure you want to permanently remove {}? [y/N] ",
            account_label(slot, &record.email)
        )) {
            self.say(Line::dimmed("Cancelled"));
            return Ok(false);
        }
        let _lock = self.store.lock()?;
        let mut roster = self.roster()?;
        self.prepare_session_change(slot, &record)?;
        credentials::delete(&self.store, slot)?;
        self.remove_session_dir(slot, &record.email);
        roster.remove_slot(slot);
        self.write_roster(&roster)?;
        tracing::info!("Removed account {slot}: {}", record.email);
        self.say(Line::new().push(Style::Accent, "Removed").push(
            Style::Plain,
            format!(" {}", account_label(slot, &record.email)),
        ));
        let mut mappings = MappingStore::load(&self.store.paths);
        let pruned = mappings.prune(record.provider, &record.identity());
        if pruned > 0 {
            mappings.save()?;
            self.say(Line::dimmed(format!(
                "Removed {pruned} directory mapping(s) for this account"
            )));
        }
        Ok(true)
    }

    pub fn set_disabled(&mut self, identifier: &str, disabled: bool) -> Result<()> {
        let _lock = self.store.lock()?;
        let mut roster = self.roster()?;
        let slot = resolve_slot(&roster, identifier)?;
        let (email, provider) = roster
            .record(slot)
            .map(|r| (r.email.clone(), r.provider))
            .ok_or_else(|| missing(slot))?;
        let label = account_label(slot, &email);
        let state = if disabled { "disabled" } else { "enabled" };
        if !roster.set_disabled(slot, disabled)? {
            self.say(Line::dimmed(format!("{label} is already {state}.")));
            return Ok(());
        }
        self.write_roster(&roster)?;
        if disabled {
            self.say(
                Line::new()
                    .push(Style::Accent, "Disabled")
                    .push(Style::Plain, format!(" {label}.")),
            );
            if self.current_account_for(provider)?.slot() == Some(slot) {
                self.say(Line::dimmed(
                    "  It is the active account — it stays live until you switch away; it just won't be an automatic switch target.",
                ));
            }
            if roster
                .switchable_slots(|s| credentials::exists(&self.store, s))
                .is_empty()
            {
                self.say(Line::warning(
                    "  No accounts remain in rotation — auto-switch and bare switch have nothing to pick. Re-enable one with ccsw enable <num|email>.",
                ));
            }
        } else {
            self.say(
                Line::new()
                    .push(Style::Accent, "Enabled")
                    .push(Style::Plain, format!(" {label}.")),
            );
            self.say(Line::dimmed("  It is back in the rotation."));
        }
        Ok(())
    }

    // -- alias ----------------------------------------------------------------

    pub fn alias_list(&mut self) -> Result<()> {
        let roster = self.roster_opt()?.unwrap_or_else(Roster::empty);
        let aliased: Vec<(u32, String, String)> = roster
            .sorted_slots()
            .into_iter()
            .filter_map(|slot| {
                let record = roster.record(slot)?;
                let alias = record.alias.as_deref().filter(|a| !a.is_empty())?;
                Some((slot, alias.to_string(), record.email.clone()))
            })
            .collect();
        if aliased.is_empty() {
            self.say(Line::dimmed("No aliases set"));
            return Ok(());
        }
        self.say(Line::new().push(Style::Bold, "Aliases:"));
        for (slot, alias, email) in aliased {
            self.say(
                Line::plain(format!("  {slot}: {alias} ")).push(Style::Muted, format!("({email})")),
            );
        }
        Ok(())
    }

    pub fn alias_set(&mut self, identifier: &str, name: &str) -> Result<()> {
        let _lock = self.store.lock()?;
        let mut roster = self.roster()?;
        let slot = resolve_slot(&roster, identifier)?;
        let alias = roster.set_alias(slot, name)?;
        self.write_roster(&roster)?;
        self.say(
            Line::new()
                .push(Style::Accent, "Set alias")
                .push(Style::Plain, format!(" '{alias}' for Account {slot}")),
        );
        Ok(())
    }

    pub fn alias_unset(&mut self, identifier: &str) -> Result<()> {
        let _lock = self.store.lock()?;
        let mut roster = self.roster()?;
        let slot = resolve_slot(&roster, identifier)?;
        if roster.unset_alias(slot)? {
            self.write_roster(&roster)?;
        }
        self.say(
            Line::new()
                .push(Style::Accent, "Removed alias")
                .push(Style::Plain, format!(" for Account {slot}")),
        );
        Ok(())
    }

    // -- move / swap ----------------------------------------------------------

    pub fn move_account(&mut self, identifier: &str, target: &str) -> Result<()> {
        let target_text = target.trim();
        let parsed = if is_digits(target_text) {
            target_text.parse::<u32>().ok().filter(|n| *n >= 1)
        } else {
            None
        };
        let Some(target_slot) = parsed else {
            return Err(CcswError::validation(format!(
                "Target slot must be a positive slot number, got: '{target}' (use `swap` to trade two accounts by identifier)"
            )));
        };
        let _lock = self.store.lock()?;
        let mut roster = self.roster()?;
        let src = resolve_slot(&roster, identifier)?;
        let src_email = roster
            .record(src)
            .map(|r| r.email.clone())
            .ok_or_else(|| missing(src))?;
        let target_email = roster.record(target_slot).map(|r| r.email.clone());
        for slot in [src, target_slot] {
            if let Some(record) = roster.record(slot) {
                self.prepare_session_change(slot, record)?;
            }
        }
        match roster.move_slot(src, target_slot)? {
            MoveOutcome::NoOp => self.say(
                Line::new()
                    .push(Style::Dimmed, "Already in")
                    .push(Style::Plain, format!(" slot {src}: {src_email}")),
            ),
            MoveOutcome::Relocated => {
                self.relocate_files(src, target_slot, &src_email)?;
                self.write_roster(&roster)?;
                tracing::info!("Moved slot: {src} ({src_email}) -> {target_slot}");
                self.say(
                    Line::new()
                        .push(Style::Accent, "Moved")
                        .push(Style::Plain, format!(" {src_email} to slot {target_slot}")),
                );
            }
            MoveOutcome::Swapped => {
                let target_email = target_email.ok_or_else(|| missing(target_slot))?;
                self.swap_files(src, &src_email, target_slot, &target_email)?;
                self.write_roster(&roster)?;
                tracing::info!(
                    "Swapped slots: {src} ({src_email}) <-> {target_slot} ({target_email})"
                );
                self.announce_swap(&roster, src, target_slot);
            }
        }
        Ok(())
    }

    pub fn swap_accounts(&mut self, first: &str, second: &str) -> Result<()> {
        let _lock = self.store.lock()?;
        let mut roster = self.roster()?;
        let a = resolve_slot(&roster, first)?;
        let b = resolve_slot(&roster, second)?;
        let email_of = |roster: &Roster, slot: u32| {
            roster
                .record(slot)
                .map(|r| r.email.clone())
                .ok_or_else(|| missing(slot))
        };
        let email_a = email_of(&roster, a)?;
        let email_b = email_of(&roster, b)?;
        for slot in [a, b] {
            if let Some(record) = roster.record(slot) {
                self.prepare_session_change(slot, record)?;
            }
        }
        roster.swap_slots(a, b)?;
        self.swap_files(a, &email_a, b, &email_b)?;
        self.write_roster(&roster)?;
        tracing::info!("Swapped slots: {a} ({email_a}) <-> {b} ({email_b})");
        self.announce_swap(&roster, a, b);
        Ok(())
    }

    fn announce_swap(&mut self, roster: &Roster, a: u32, b: u32) {
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        self.say(
            Line::new()
                .push(Style::Accent, "Swapped")
                .push(Style::Plain, format!(" Account {lo} and Account {hi}:")),
        );
        for slot in [lo, hi] {
            let email = roster.record(slot).map(|r| r.email.as_str()).unwrap_or("");
            self.say(Line::plain(format!("  {slot}: {email}")));
        }
    }

    fn relocate_files(&self, src: u32, dst: u32, email: &str) -> Result<()> {
        let paths = &self.store.paths;
        rename_if_exists(&paths.credential_file(src), &paths.credential_file(dst))?;
        rename_if_exists(
            &paths.credential_prev_file(src),
            &paths.credential_prev_file(dst),
        )?;
        rename_if_exists(
            &paths.session_dir(src, email),
            &paths.session_dir(dst, email),
        )
    }

    fn swap_files(&self, a: u32, email_a: &str, b: u32, email_b: &str) -> Result<()> {
        let paths = &self.store.paths;
        swap_paths(&paths.credential_file(a), &paths.credential_file(b))?;
        swap_paths(
            &paths.credential_prev_file(a),
            &paths.credential_prev_file(b),
        )?;
        if paths.session_dir(a, email_a) == paths.session_dir(a, email_b) {
            // Same slug on both sides: one exchange, not two renames onto each other.
            return swap_paths(
                &paths.session_dir(a, email_a),
                &paths.session_dir(b, email_a),
            );
        }
        rename_if_exists(
            &paths.session_dir(a, email_a),
            &paths.session_dir(b, email_a),
        )?;
        rename_if_exists(
            &paths.session_dir(b, email_b),
            &paths.session_dir(a, email_b),
        )
    }

    // -- purge ----------------------------------------------------------------

    /// Returns `false` when the user cancelled.
    pub fn purge(&mut self) -> Result<bool> {
        let root = self.store.paths.backup_root.clone();
        self.say(Line::warning(
            "This will remove ALL ccsw data from your system:",
        ));
        self.say(Line::plain(format!(
            "  - Backup directory: {}",
            root.display()
        )));
        self.say(Line::plain("  - All stored account credential files"));
        let has_sessions = fs::read_dir(self.store.paths.sessions_dir())
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false);
        if has_sessions {
            self.say(Line::plain("  - All session profiles"));
        }
        self.say(Line::plain(""));
        self.say(Line::dimmed(
            "Note: This does NOT affect your current Codex login.",
        ));
        self.say(Line::plain(""));
        if !self
            .ui
            .confirm("Are you sure you want to purge all data? [y/N] ")
        {
            self.say(Line::dimmed("Cancelled"));
            return Ok(false);
        }
        if root.exists() {
            fs::remove_dir_all(&root).map_err(|err| {
                CcswError::config(format!("could not remove {}: {err}", root.display()))
            })?;
            self.say(Line::new().push(Style::Accent, "Removed:"));
            self.say(Line::plain(format!("  - {}", root.display())));
        } else {
            self.say(Line::dimmed("No ccsw data found to remove."));
        }
        self.say(Line::new().push(Style::Accent, "Purge complete."));
        Ok(true)
    }

    // -- switching ------------------------------------------------------------

    /// `switch <id>`. `None` when the disambiguation prompt was cancelled.
    pub fn switch_to(
        &mut self,
        identifier: &str,
        force: bool,
        interactive: bool,
    ) -> Result<Option<SwitchReport>> {
        let roster = self.roster()?;
        let Some(slot) = self.resolve_for_prompt(&roster, identifier, "switch to", interactive)?
        else {
            return Ok(None);
        };
        let report = self.perform_switch(roster, slot, "direct", force)?;
        if report.outcome.reason == "already-active" {
            self.say(Line::new().push(Style::Accent, &report.outcome.message));
            self.say(Line::dimmed(format!(
                "To rewrite the live login from the stored backup (e.g. after --import), run: ccsw switch {slot} --force"
            )));
        }
        Ok(Some(report))
    }

    /// The switch body (spec §7.2): fold the live login back, back it up,
    /// write the target, mark it active, then restart the daemon if needed.
    fn perform_switch(
        &mut self,
        mut roster: Roster,
        target: u32,
        strategy: &str,
        force: bool,
    ) -> Result<SwitchReport> {
        let record = roster
            .record(target)
            .cloned()
            .ok_or_else(|| missing(target))?;
        if record.provider == Provider::Claude {
            return self.perform_claude_switch(roster, target, record, strategy, force);
        }
        let mut stored = credentials::read(&self.store, target)?
            .map(AuthJson::from_value)
            .ok_or_else(|| {
                CcswError::switch(format!(
                    "Account-{target} has no stored credentials. Re-add with: ccsw add --slot {target}"
                ))
            })?;
        let live_path = self.store.paths.live_auth_file();
        let mut warnings = Vec::new();
        let to = AccountRef {
            number: Some(target),
            email: record.email.clone(),
        };

        let lock = self.store.lock()?;
        let live = AuthJson::read(&live_path)?;
        let live_slot = live.as_ref().and_then(|l| self.slot_of_live(&roster, l));
        let from = live.as_ref().map(|live| match live_slot {
            Some(slot) => AccountRef {
                number: Some(slot),
                email: roster
                    .record(slot)
                    .map(|r| r.email.clone())
                    .unwrap_or_else(|| live_email(live)),
            },
            None => AccountRef {
                number: None,
                email: live_email(live),
            },
        });
        let same_slot = live_slot == Some(target);
        if same_slot && !force && live.as_ref() == Some(&stored) {
            return Ok(SwitchReport {
                outcome: SwitchOutcome {
                    switched: false,
                    provider: Provider::Codex,
                    from: from.clone(),
                    to: Some(to),
                    strategy: strategy.to_string(),
                    reason: "already-active".to_string(),
                    message: format!("Already on {}", account_label(target, &record.email)),
                    warnings,
                },
                followup: None,
                show_list: false,
            });
        }
        if !force && let Some(live) = &live {
            match live_slot {
                Some(slot) => {
                    let kept = credentials::read(&self.store, slot)?.map(AuthJson::from_value);
                    if kept.is_none_or(|kept| live.is_newer_than(&kept)) {
                        credentials::write(&self.store, slot, &live.0)?;
                        tracing::info!("folded the live login back into slot {slot}");
                        if slot == target {
                            stored = live.clone();
                        }
                    }
                }
                None => warnings.push(
                    "The live login does not match a managed account; it was left in place."
                        .to_string(),
                ),
            }
        }
        let before = snapshot_live_auth(&live_path);
        backup_live(&live_path)?;
        stored.write(&live_path)?;
        roster.set_active(Some(target));
        self.write_roster(&roster)?;
        drop(lock);

        let restart = restart_daemon_if_live_auth_changed(&before, &live_path);
        let followup = restart.message(&format!("Account-{target}"));
        if restart.is_failure()
            && let Some(message) = &followup
        {
            warnings.push(message.clone());
        }
        self.replan_active(target, &record);
        tracing::info!(
            "Switched from account {} to {target}",
            from.as_ref()
                .and_then(|f| f.number)
                .map_or_else(|| "none".to_string(), |n| n.to_string())
        );

        let switched = from.as_ref().and_then(|f| f.number) != Some(target);
        let label = account_label(target, &record.email);
        let (reason, verb, message, show_list) = match (switched, live_slot.is_some()) {
            (true, true) => (
                "switched",
                "Switched to",
                format!("Switched to {label}"),
                true,
            ),
            (true, false) => ("switched", "Activated", format!("Activated {label}"), false),
            (false, _) => (
                "activated",
                "Activated",
                format!("Activated {label} from stored backup"),
                false,
            ),
        };
        for warning in &warnings {
            self.say(Line::warning(warning));
        }
        self.say(
            Line::new()
                .push(Style::Accent, verb)
                .push(Style::Plain, message[verb.len()..].to_string()),
        );
        Ok(SwitchReport {
            outcome: SwitchOutcome {
                switched,
                provider: Provider::Codex,
                from,
                to: Some(to),
                strategy: strategy.to_string(),
                reason: reason.to_string(),
                message,
                warnings,
            },
            followup,
            show_list,
        })
    }

    /// The Claude switch body (spec §7): Claude Code's locks, fold-back, backup,
    /// write across backends, `oauthAccount` splice, follow-up by backend.
    fn perform_claude_switch(
        &mut self,
        mut roster: Roster,
        target: u32,
        record: AccountRecord,
        strategy: &str,
        force: bool,
    ) -> Result<SwitchReport> {
        let to = AccountRef {
            number: Some(target),
            email: record.email.clone(),
        };
        let mut warnings = Vec::new();
        let live_api = ClaudeLive::new(&self.store.paths, &SystemSecurity);

        let store_lock = self.store.lock()?;
        let profile = self.store.paths.session_dir(target, &record.email);
        crate::claude::session::require_quiescent(&profile)?;
        crate::claude::session::reconcile_locked(&self.store, target, &record, &SystemSecurity)?;
        let stored = credentials::read(&self.store, target)?.ok_or_else(|| {
            CcswError::switch(format!(
                "Account-{target} has no stored credentials. Re-add with: ccsw add claude --slot {target}"
            ))
        })?;
        let stored = SlotFile::from_value(&stored).map_err(|err| {
            CcswError::switch(format!(
                "Account-{target}'s stored credentials are unusable ({err}). Re-add with: ccsw add claude --slot {target}"
            ))
        })?;
        if stored.credential.kind() == CredentialKind::Unknown {
            return Err(CcswError::switch(format!(
                "Account-{target}'s stored credentials are unusable (no login in the slot file). Re-add with: ccsw add claude --slot {target}"
            )));
        }
        let claude_locks = crate::claude::locks::acquire(&self.store.paths)?;
        let live = live_api.read()?;
        let live_slot = self.claude_slot_of_live(&roster, &live);
        let from = live.credential.as_ref().map(|_| match live_slot {
            Some(slot) => AccountRef {
                number: Some(slot),
                email: roster
                    .record(slot)
                    .map(|r| r.email.clone())
                    .unwrap_or_else(|| claude_live_email(&live)),
            },
            None => AccountRef {
                number: None,
                email: claude_live_email(&live),
            },
        });
        let same_slot = live_slot == Some(target);
        let live_login_only = live.credential.as_ref().map(ClaudeCredential::oauth_only);
        if same_slot && !force && live_login_only.as_ref() == Some(&stored.credential) {
            return Ok(SwitchReport {
                outcome: SwitchOutcome {
                    switched: false,
                    provider: Provider::Claude,
                    from: from.clone(),
                    to: Some(to),
                    strategy: strategy.to_string(),
                    reason: "already-active".to_string(),
                    message: format!("Already on {}", account_label(target, &record.email)),
                    warnings,
                },
                followup: None,
                show_list: false,
            });
        }
        let mut target_credential = stored.credential.clone();
        if !force && let Some(live_credential) = &live.credential {
            match live_slot {
                Some(slot) => {
                    let kept = credentials::read(&self.store, slot)?
                        .and_then(|v| SlotFile::from_value(&v).ok());
                    let incoming = live_credential.oauth_only();
                    if kept
                        .as_ref()
                        .is_none_or(|kept| incoming.is_newer_than(&kept.credential))
                    {
                        let account = live
                            .oauth_account
                            .clone()
                            .or_else(|| kept.as_ref().map(|k| k.oauth_account.clone()))
                            .unwrap_or_else(|| {
                                OauthAccount::synthesized(
                                    &roster
                                        .record(slot)
                                        .map(|r| r.email.clone())
                                        .unwrap_or_default(),
                                )
                            });
                        let folded = SlotFile::new(live_credential, account);
                        credentials::write(&self.store, slot, &folded.to_value())?;
                        tracing::info!("folded the live Claude login back into slot {slot}");
                        if slot == target {
                            target_credential = folded.credential;
                        }
                    }
                }
                None => warnings.push(
                    "The live login does not match a managed account; it was left in place."
                        .to_string(),
                ),
            }
        }
        backup_claude_live(&self.store.paths, &live)?;
        // What a failed splice below restores: the login exactly as it was.
        let previous = live.raw.clone().or_else(|| live.credential.clone());
        let backend = match target_credential.kind() {
            CredentialKind::ApiKey => {
                live_api.write_managed_key(target_credential.api_key().unwrap_or_default())?
            }
            _ => {
                let mut object = live
                    .raw
                    .clone()
                    .filter(|c| c.kind() != CredentialKind::ApiKey)
                    .unwrap_or_else(|| ClaudeCredential::from_value(serde_json::json!({})));
                object.replace_oauth_from(&target_credential);
                live_api.write_oauth(&object)?
            }
        };
        let account = stored.oauth_account.0.clone();
        if let Err(err) = splice_oauth_account(&live_api, account) {
            // Live tokens would belong to one account while `oauthAccount`
            // names another; put the previous login back (best effort).
            let restored = match live.credential.as_ref() {
                Some(c) if c.kind() == CredentialKind::ApiKey => {
                    live_api.write_managed_key(c.api_key().unwrap_or_default())
                }
                Some(_) => match &previous {
                    Some(previous) => live_api.write_oauth(previous),
                    None => Ok(backend),
                },
                None => Ok(backend),
            };
            if let Err(rollback) = restored {
                tracing::warn!(
                    "could not restore the previous Claude login ({rollback}); it is saved under {}",
                    self.store.paths.claude_backups_dir().display()
                );
            }
            return Err(err);
        }
        roster.set_active_for(Provider::Claude, Some(target));
        self.write_roster(&roster)?;
        drop(claude_locks);
        drop(store_lock);

        let followup = Some(backend.followup().to_string());
        self.replan_active(target, &record);
        tracing::info!(
            "Switched from account {} to {target} (claude)",
            from.as_ref()
                .and_then(|f| f.number)
                .map_or_else(|| "none".to_string(), |n| n.to_string())
        );
        let switched = from.as_ref().and_then(|f| f.number) != Some(target);
        let label = account_label(target, &record.email);
        let (reason, verb, message, show_list) = match (switched, live_slot.is_some()) {
            (true, true) => (
                "switched",
                "Switched to",
                format!("Switched to {label}"),
                true,
            ),
            (true, false) => ("switched", "Activated", format!("Activated {label}"), false),
            (false, _) => (
                "activated",
                "Activated",
                format!("Activated {label} from stored backup"),
                false,
            ),
        };
        for warning in &warnings {
            self.say(Line::warning(warning));
        }
        self.say(
            Line::new()
                .push(Style::Accent, verb)
                .push(Style::Plain, message[verb.len()..].to_string()),
        );
        Ok(SwitchReport {
            outcome: SwitchOutcome {
                switched,
                provider: Provider::Claude,
                from,
                to: Some(to),
                strategy: strategy.to_string(),
                reason: reason.to_string(),
                message,
                warnings,
            },
            followup,
            show_list,
        })
    }

    /// Pull the new active account's poll plan forward (best-effort).
    fn replan_active(&self, slot: u32, record: &AccountRecord) {
        let now = now_unix() as f64;
        let usage_store = UsageStore::new(&self.store.paths);
        let identities = BTreeMap::from([(slot, record.identity())]);
        if let Some(entry) = usage_store.entries(&identities, now).get(&slot)
            && let Some(fetched_at) = entry.fetched_at
        {
            let (next, interval) = replan_new_active(fetched_at, now);
            if let Err(err) = usage_store.set_poll_plan(slot, &record.identity(), next, interval) {
                tracing::warn!("could not replan the active account's polling: {err}");
            }
        }
    }

    /// Bare `switch` and `switch --strategy`.
    pub fn switch(
        &mut self,
        provider: Option<Provider>,
        strategy: Strategy,
        models: &[String],
        interactive: bool,
    ) -> Result<SwitchReport> {
        let roster = self.roster()?;
        let provider = self.resolve_provider(&roster, provider, "switch")?;
        let name = strategy.name();
        let noop = |from: Option<AccountRef>, reason: &str, message: String| SwitchReport {
            outcome: SwitchOutcome {
                switched: false,
                provider,
                from: from.clone(),
                to: from,
                strategy: name.to_string(),
                reason: reason.to_string(),
                message,
                warnings: Vec::new(),
            },
            followup: None,
            show_list: false,
        };
        let current = self.current_account_for(provider)?;
        let live_slot = match &current {
            CurrentAccount::NoLogin => return self.activate_fresh(roster, provider, name),
            CurrentAccount::Unmanaged { email } => {
                if !interactive {
                    let from = AccountRef {
                        number: None,
                        email: email.clone(),
                    };
                    return Ok(noop(
                        Some(from),
                        "unmanaged-account",
                        "Active account is not managed; run ccsw add".to_string(),
                    ));
                }
                self.say(Line::plain(format!(
                    "Notice: Active account '{email}' was not managed."
                )));
                let slot = match self.add_account(provider, None, None)? {
                    AddOutcome::Added { slot } | AddOutcome::Updated { slot } => slot,
                    AddOutcome::Cancelled => {
                        return Err(CcswError::switch("Adding the active account was cancelled"));
                    }
                };
                self.say(Line::plain(format!(
                    "It has been automatically added as Account-{slot}."
                )));
                self.say(Line::plain(
                    "Please run the switch command again to switch to the next account.",
                ));
                let from = AccountRef {
                    number: Some(slot),
                    email: email.clone(),
                };
                return Ok(noop(
                    Some(from),
                    "unmanaged-account",
                    format!("Active account was not managed; added as Account-{slot}"),
                ));
            }
            CurrentAccount::Managed { slot, .. } => *slot,
        };
        let current_ref = AccountRef {
            number: Some(live_slot),
            email: current.email().unwrap_or_default().to_string(),
        };
        if roster.slots_of(provider).len() <= 1 {
            let message = "Only one account is managed. Add more accounts to switch between.";
            self.say(Line::dimmed(message));
            return Ok(noop(
                Some(current_ref),
                "only-one-account",
                message.to_string(),
            ));
        }
        match strategy {
            Strategy::Rotation => {
                self.rotate(roster, provider, live_slot, current_ref, None, models)
            }
            Strategy::NextAvailable => {
                let entries = self.collect_for_switch(&roster, provider, live_slot, models)?;
                self.rotate(
                    roster,
                    provider,
                    live_slot,
                    current_ref,
                    Some(&entries),
                    models,
                )
            }
            Strategy::Best => {
                let entries = self.collect_for_switch(&roster, provider, live_slot, models)?;
                self.best(roster, provider, live_slot, current_ref, &entries, models)
            }
        }
    }

    /// The provider a bare verb acts on: the selector, else the only provider
    /// with accounts, else the user has to say (spec §6.1).
    fn resolve_provider(
        &self,
        roster: &Roster,
        explicit: Option<Provider>,
        verb: &str,
    ) -> Result<Provider> {
        if let Some(provider) = explicit {
            return Ok(provider);
        }
        let present: Vec<Provider> = Provider::ALL
            .into_iter()
            .filter(|p| !roster.slots_of(*p).is_empty())
            .collect();
        match present.as_slice() {
            [] => Ok(Provider::Codex),
            [only] => Ok(*only),
            _ => Err(CcswError::config(format!(
                "Both Codex and Claude accounts are managed — say which: ccsw {verb} codex | ccsw {verb} claude"
            ))),
        }
    }

    /// No live login: activate the recorded active slot, else the first usable one.
    fn activate_fresh(
        &mut self,
        roster: Roster,
        provider: Provider,
        strategy: &str,
    ) -> Result<SwitchReport> {
        let in_provider = roster.slots_of(provider);
        let preferred = roster
            .active_for(provider)
            .filter(|n| in_provider.contains(n));
        let order: Vec<u32> = preferred
            .into_iter()
            .chain(
                in_provider
                    .iter()
                    .copied()
                    .filter(|s| Some(*s) != preferred),
            )
            .collect();
        let mut warnings = Vec::new();
        let mut any_disabled = false;
        for slot in order {
            let Some(record) = roster.record(slot) else {
                continue;
            };
            if record.disabled {
                any_disabled = true;
                self.skip(&mut warnings, slot, "disabled", "disabled");
                continue;
            }
            if !credentials::exists(&self.store, slot) {
                self.skip_no_credentials(&mut warnings, slot);
                continue;
            }
            let mut report = self.perform_switch(roster.clone(), slot, strategy, false)?;
            report.outcome.warnings.splice(0..0, warnings);
            return Ok(report);
        }
        Err(if any_disabled {
            CcswError::config(
                "No accounts remain in rotation. Re-enable one with: ccsw enable <num|email>",
            )
        } else {
            CcswError::config(
                "No managed accounts have valid stored credentials. Re-add a slot with: ccsw add --slot <number>",
            )
        })
    }

    fn skip(&mut self, warnings: &mut Vec<String>, slot: u32, human: &str, json: &str) {
        self.say(Line::dimmed(format!("Skipping Account-{slot} ({human})")));
        warnings.push(format!("Skipped Account-{slot} ({json})"));
    }

    fn skip_no_credentials(&mut self, warnings: &mut Vec<String>, slot: u32) {
        self.skip(
            warnings,
            slot,
            &format!("no stored credentials, re-add with ccsw add --slot {slot}"),
            "no stored credentials",
        );
    }

    fn collect_for_switch(
        &mut self,
        roster: &Roster,
        provider: Provider,
        live_slot: u32,
        models: &[String],
    ) -> Result<BTreeMap<u32, UsageEntry>> {
        let candidates: Vec<u32> = roster
            .switchable_slots(|s| credentials::exists(&self.store, s))
            .into_iter()
            .filter(|s| roster.record(*s).is_some_and(|r| r.provider == provider))
            .collect();
        let collected = collect::run_pass(
            &self.store,
            roster,
            CollectOptions {
                mode: CollectMode::OnDemand,
                actives: vec![live_slot],
                candidates: &candidates,
                threshold: self.settings.autoswitch.threshold,
                models,
            },
        )?;
        for failure in &collected.token_persist_failures {
            self.say(Line::warning(failure));
        }
        Ok(collected.entries)
    }

    fn rotate(
        &mut self,
        roster: Roster,
        provider: Provider,
        live_slot: u32,
        current_ref: AccountRef,
        entries: Option<&BTreeMap<u32, UsageEntry>>,
        models: &[String],
    ) -> Result<SwitchReport> {
        let strategy = if entries.is_some() {
            Strategy::NextAvailable
        } else {
            Strategy::Rotation
        };
        let sequence = roster.slots_of(provider);
        let anchor = match strategy {
            Strategy::Rotation => roster
                .active_for(provider)
                .filter(|n| sequence.contains(n))
                .unwrap_or(live_slot),
            _ => live_slot,
        };
        let start = sequence
            .iter()
            .position(|s| *s == anchor)
            .map_or(0, |i| i + 1);
        let mut warnings = Vec::new();
        let mut exhausted = false;
        let mut target = None;
        for k in 0..sequence.len() {
            let slot = sequence[(start + k) % sequence.len()];
            if slot == anchor {
                continue;
            }
            let Some(record) = roster.record(slot) else {
                continue;
            };
            if record.disabled {
                self.skip(&mut warnings, slot, "disabled", "disabled");
                continue;
            }
            if !credentials::exists(&self.store, slot) {
                self.skip_no_credentials(&mut warnings, slot);
                continue;
            }
            if let Some(entries) = entries
                && let Some(label) = at_limit_label(entries.get(&slot), models)
            {
                let text = format!("at {label} limit");
                self.skip(&mut warnings, slot, &text, &text);
                exhausted = true;
                continue;
            }
            target = Some(slot);
            break;
        }
        let noop = |reason: &str, message: String, warnings: Vec<String>| SwitchReport {
            outcome: SwitchOutcome {
                switched: false,
                provider,
                from: Some(current_ref.clone()),
                to: Some(current_ref.clone()),
                strategy: strategy.name().to_string(),
                reason: reason.to_string(),
                message,
                warnings,
            },
            followup: None,
            show_list: false,
        };
        let Some(target) = target else {
            if exhausted {
                let message = format!(
                    "All other accounts are at their {} — staying on Account-{live_slot}.",
                    limits_label(models)
                );
                self.say(Line::warning(&message));
                return Ok(noop("candidates-exhausted", message, warnings));
            }
            let message = "No other accounts have valid stored credentials.";
            self.say(Line::dimmed(format!(
                "{message}\nRe-add a skipped slot with: ccsw add --slot <number>"
            )));
            return Ok(noop("no-valid-target", message.to_string(), warnings));
        };
        if target == live_slot {
            let message = format!(
                "Already on {}",
                account_label(live_slot, &current_ref.email)
            );
            self.say(Line::new().push(Style::Accent, &message));
            return Ok(noop("already-active", message, warnings));
        }
        let mut report = self.perform_switch(roster, target, strategy.name(), false)?;
        report.outcome.warnings.splice(0..0, warnings);
        Ok(report)
    }

    fn best(
        &mut self,
        roster: Roster,
        provider: Provider,
        live_slot: u32,
        current_ref: AccountRef,
        entries: &BTreeMap<u32, UsageEntry>,
        models: &[String],
    ) -> Result<SwitchReport> {
        let head = |slot: u32| {
            entries
                .get(&slot)
                .and_then(UsageEntry::decision_value)
                .and_then(|usage| headroom(usage, models))
        };
        let noop = |reason: &str, message: String| SwitchReport {
            outcome: SwitchOutcome {
                switched: false,
                provider,
                from: Some(current_ref.clone()),
                to: Some(current_ref.clone()),
                strategy: "best".to_string(),
                reason: reason.to_string(),
                message,
                warnings: Vec::new(),
            },
            followup: None,
            show_list: false,
        };
        let Some(current_head) = head(live_slot) else {
            let message = format!(
                "Current account usage is unavailable — staying on Account-{live_slot}. Run ccsw switch to rotate."
            );
            self.say(Line::dimmed(&message));
            return Ok(noop("usage-unavailable", message));
        };
        let candidates: Vec<u32> = roster
            .switchable_slots(|s| credentials::exists(&self.store, s))
            .into_iter()
            .filter(|s| {
                *s != live_slot && roster.record(*s).is_some_and(|r| r.provider == provider)
            })
            .collect();
        let known: Vec<(u32, f64)> = candidates
            .iter()
            .filter_map(|slot| head(*slot).map(|h| (*slot, h)))
            .collect();
        if known.is_empty() {
            let message = format!(
                "No other account has usage data to compare — staying on Account-{live_slot}. Run ccsw switch to rotate."
            );
            self.say(Line::dimmed(&message));
            return Ok(noop("usage-unavailable", message));
        }
        // Greatest headroom; ties go to the earliest slot.
        let (best_slot, best_head) = known
            .iter()
            .copied()
            .max_by(|a, b| {
                a.1.partial_cmp(&b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.0.cmp(&a.0))
            })
            .expect("known is non-empty");
        if best_head > current_head {
            return self.perform_switch(roster, best_slot, "best", false);
        }
        if known.len() < candidates.len() {
            let message = format!(
                "No account with known usage has more remaining quota; some usage is unavailable — staying on Account-{live_slot}."
            );
            self.say(Line::dimmed(&message));
            return Ok(noop("usage-unavailable", message));
        }
        if current_head <= 0.0 && known.iter().all(|(_, h)| *h <= 0.0) {
            let message = format!(
                "All accounts are at their {} — staying on Account-{live_slot}.",
                limits_label(models)
            );
            self.say(Line::warning(&message));
            return Ok(noop("candidates-exhausted", message));
        }
        let message =
            format!("Already on the account with the most remaining quota (Account-{live_slot}).");
        self.say(Line::new().push(Style::Accent, &message));
        Ok(noop("already-best", message))
    }

    // -- snapshots ------------------------------------------------------------

    /// Rows for `list`; `None` when no roster exists (first run).
    pub fn list_snapshot(&self, fetch: bool) -> Result<Option<ListSnapshot>> {
        let Some(roster) = self.roster_opt()? else {
            return Ok(None);
        };
        let mut actives = ActiveSlots::default();
        for provider in Provider::ALL {
            actives.set(
                provider,
                collect::live_login_for(&self.store, &roster, provider).slot(),
            );
        }
        let slots: Vec<u32> = roster
            .sequence
            .iter()
            .copied()
            .filter(|s| roster.record(*s).is_some())
            .collect();
        let mode = if fetch {
            CollectMode::OnDemand
        } else {
            CollectMode::StoreOnly
        };
        let mut collected = collect::run_pass(
            &self.store,
            &roster,
            CollectOptions {
                mode,
                actives: actives.iter().filter_map(|(_, slot)| slot).collect(),
                candidates: &slots,
                threshold: self.settings.autoswitch.threshold,
                models: &self.settings.autoswitch.model_names(),
            },
        )?;
        let rows = slots
            .into_iter()
            .map(|slot| {
                let record = roster.record(slot).cloned().expect("filtered above");
                let is_active = actives.get(record.provider) == Some(slot);
                AccountRow {
                    slot,
                    record,
                    usage: collected
                        .entries
                        .remove(&slot)
                        .unwrap_or_else(|| self.fallback_entry(slot)),
                    is_active,
                }
            })
            .collect();
        Ok(Some(ListSnapshot {
            actives,
            rows,
            warnings: collected.token_persist_failures,
        }))
    }

    pub fn status(&self) -> Result<StatusSnapshot> {
        let roster = self.roster_opt()?.unwrap_or_else(Roster::empty);
        let total = roster.sorted_slots().len();
        let mut providers = Vec::new();
        for provider in Provider::ALL {
            let current = collect::live_login_for(&self.store, &roster, provider);
            let row = match (
                current.slot(),
                current.slot().and_then(|s| roster.record(s)),
            ) {
                (Some(slot), Some(record)) => {
                    let mut collected = collect::run_pass(
                        &self.store,
                        &roster,
                        CollectOptions {
                            mode: CollectMode::OnDemand,
                            actives: vec![slot],
                            candidates: &[],
                            threshold: self.settings.autoswitch.threshold,
                            models: &self.settings.autoswitch.model_names(),
                        },
                    )?;
                    Some(AccountRow {
                        slot,
                        record: record.clone(),
                        usage: collected
                            .entries
                            .remove(&slot)
                            .unwrap_or_else(|| self.fallback_entry(slot)),
                        is_active: true,
                    })
                }
                _ => None,
            };
            providers.push(ProviderStatus {
                provider,
                current,
                row,
            });
        }
        Ok(StatusSnapshot { providers, total })
    }

    fn fallback_entry(&self, slot: u32) -> UsageEntry {
        let sentinel =
            (!credentials::exists(&self.store, slot)).then_some(UsageSentinel::NoCredentials);
        empty_entry(sentinel)
    }
}

/// `5h/7d limit` unless model pools take part, then `usage limits`.
fn limits_label(models: &[String]) -> &'static str {
    if models.is_empty() {
        "5h/7d limit"
    } else {
        "usage limits"
    }
}

/// The names of the windows at or over 100 %, joined with `/`; `None` when
/// the account is not at its limit or its usage is unknown.
fn at_limit_label(entry: Option<&UsageEntry>, models: &[String]) -> Option<String> {
    let usage = entry?.decision_value()?;
    let names: Vec<String> = relevant_windows(usage, models)
        .into_iter()
        .filter(|(_, pct, _)| *pct >= 100.0)
        .map(|(name, _, _)| name)
        .collect();
    (!names.is_empty()).then(|| names.join("/"))
}

// wired to Task E's trait
impl crate::autoswitch::AutoFacade for Switcher {
    fn store(&self) -> &Store {
        &self.store
    }

    fn roster(&mut self) -> Result<Roster> {
        Switcher::roster_opt(self).map(|roster| roster.unwrap_or_else(Roster::empty))
    }

    fn current_account(&mut self, provider: Provider) -> Result<CurrentAccount> {
        Switcher::current_account_for(self, provider)
    }

    fn switch_to(&mut self, slot: u32) -> Result<SwitchOutcome> {
        Switcher::switch_to(self, &slot.to_string(), false, false)?
            .map(|report| report.outcome)
            .ok_or_else(|| CcswError::switch("switch cancelled"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::temp_store;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// Records lines and answers prompts from a queue.
    #[derive(Default)]
    struct Recorder {
        lines: Rc<RefCell<Vec<String>>>,
        answers: Rc<RefCell<VecDeque<String>>>,
    }

    impl Ui for Recorder {
        fn say(&mut self, line: Line) {
            self.lines.borrow_mut().push(line.text());
        }
        fn confirm(&mut self, prompt: &str) -> bool {
            self.lines.borrow_mut().push(format!("PROMPT {prompt}"));
            self.answers
                .borrow_mut()
                .pop_front()
                .is_some_and(|a| matches!(a.as_str(), "y" | "yes"))
        }
        fn ask(&mut self, prompt: &str) -> Option<String> {
            self.lines.borrow_mut().push(format!("PROMPT {prompt}"));
            self.answers.borrow_mut().pop_front()
        }
    }

    thread_local! {
        pub(super) static FAIL_CONFIG_SPLICE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        switcher: Switcher,
        lines: Rc<RefCell<Vec<String>>>,
        answers: Rc<RefCell<VecDeque<String>>>,
    }

    fn fixture() -> Fixture {
        let (dir, store) = temp_store();
        let recorder = Recorder::default();
        let lines = recorder.lines.clone();
        let answers = recorder.answers.clone();
        let mut switcher = Switcher::open(store);
        switcher.ui = Box::new(recorder);
        Fixture {
            _dir: dir,
            switcher,
            lines,
            answers,
        }
    }

    impl Fixture {
        fn lines(&self) -> Vec<String> {
            self.lines.borrow().clone()
        }
        fn answer(&self, text: &str) {
            self.answers.borrow_mut().push_back(text.to_string());
        }
        fn write_live(&self, auth: &Value) {
            let path = self.switcher.store.paths.live_auth_file();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, auth.to_string()).unwrap();
        }
        fn roster(&self) -> Roster {
            roster::read(&self.switcher.store.paths).unwrap().unwrap()
        }
        fn write_claude_live(&self, email: &str, org: &str, name: &str, refresh: &str) {
            let paths = &self.switcher.store.paths;
            fs::create_dir_all(&paths.claude_home).unwrap();
            fs::write(
                paths.claude_credentials_file(),
                json!({
                    "claudeAiOauth": {"accessToken": format!("cat-{refresh}"), "refreshToken": refresh, "expiresAt": 4_102_444_800_000i64, "scopes": ["user:inference"]},
                    "mcpOAuth": {"srv": {"accessToken": "m"}}
                })
                .to_string(),
            )
            .unwrap();
            fs::write(
                paths.claude_global_config_file(),
                json!({"oauthAccount": {"emailAddress": email, "organizationUuid": org, "organizationName": name, "accountUuid": "u"}, "projects": {"/p": {}}})
                    .to_string(),
            )
            .unwrap();
        }
        fn claude_credentials(&self) -> Value {
            serde_json::from_str(
                &fs::read_to_string(self.switcher.store.paths.claude_credentials_file()).unwrap(),
            )
            .unwrap()
        }
        fn claude_config(&self) -> Value {
            serde_json::from_str(
                &fs::read_to_string(self.switcher.store.paths.claude_global_config_file()).unwrap(),
            )
            .unwrap()
        }
        fn live(&self) -> Value {
            serde_json::from_str(
                &fs::read_to_string(self.switcher.store.paths.live_auth_file()).unwrap(),
            )
            .unwrap()
        }
        fn credential(&self, slot: u32) -> Value {
            credentials::read(&self.switcher.store, slot)
                .unwrap()
                .unwrap()
        }
    }

    fn jwt(email: &str, account_id: &str) -> String {
        let claims = json!({
            "email": email,
            "exp": 4_102_444_800i64,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_user_id": format!("user-{email}"),
                "chatgpt_plan_type": "plus",
                "organizations": [{"id": account_id, "title": if account_id == "acct-team" { "Acme" } else { "" }}]
            }
        });
        format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }

    fn chatgpt(email: &str, account_id: &str, refresh: &str) -> Value {
        json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "tokens": {"id_token": jwt(email, account_id), "access_token": "at", "refresh_token": refresh, "account_id": account_id},
            "last_refresh": "2026-09-29T10:00:00Z"
        })
    }

    #[test]
    fn add_accounts_captures_both_logins_even_with_the_same_email() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("me@example.com", "acct-1", "rt-codex"));
        fx.write_claude_live("Me@Example.com", "org-1", "Acme", "crt-1");
        let outcomes = fx.switcher.add_accounts(None, None, None).unwrap();
        assert_eq!(
            outcomes,
            vec![
                (Provider::Codex, AddOutcome::Added { slot: 1 }),
                (Provider::Claude, AddOutcome::Added { slot: 2 })
            ]
        );
        assert_eq!(
            fx.lines(),
            [
                "Added Account 1: me@example.com [Plus]",
                "Added Account 2: me@example.com [Acme]"
            ]
        );
        let roster = fx.roster();
        assert_eq!(roster.record(1).unwrap().provider, Provider::Codex);
        let claude = roster.record(2).unwrap();
        assert_eq!(claude.provider, Provider::Claude);
        assert_eq!(claude.organization_uuid, "org-1");
        assert_eq!(claude.uuid, "u");
        assert_eq!(roster.active_for(Provider::Codex), Some(1));
        assert_eq!(roster.active_for(Provider::Claude), Some(2));
        let stored = fx.credential(2);
        assert_eq!(stored["claudeAiOauth"]["refreshToken"], "crt-1");
        assert_eq!(stored["oauthAccount"]["emailAddress"], "Me@Example.com");
        assert!(stored.get("mcpOAuth").is_none());
    }

    #[test]
    fn add_accounts_reports_when_both_logins_are_already_managed() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        fx.write_claude_live("c@example.com", "org", "", "crt-1");
        fx.switcher.add_accounts(None, None, None).unwrap();
        fx.lines.borrow_mut().clear();
        let outcomes = fx.switcher.add_accounts(None, None, None).unwrap();
        assert_eq!(
            outcomes,
            vec![
                (Provider::Codex, AddOutcome::Updated { slot: 1 }),
                (Provider::Claude, AddOutcome::Updated { slot: 2 })
            ]
        );
        assert_eq!(
            fx.lines(),
            [
                "Updated credentials for Account 1 (a@example.com [Plus]).",
                "Updated credentials for Account 2 (c@example.com [personal]).",
                "Both current logins were already managed: Account-1 (codex), Account-2 (claude) — nothing new was added.",
            ]
        );
    }

    #[test]
    fn add_accounts_selector_missing_login_and_flags() {
        let mut fx = fixture();
        let err = fx.switcher.add_accounts(None, None, None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "No active Codex or Claude login found. Log in first."
        );
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        let err = fx
            .switcher
            .add_accounts(Some(Provider::Claude), None, None)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "No active Claude account found. Please log in first."
        );
        assert_eq!(
            fx.switcher
                .add_account(Provider::Codex, Some(3), Some("Work"))
                .unwrap(),
            AddOutcome::Added { slot: 3 }
        );
        fx.write_claude_live("c@example.com", "org", "", "crt-1");
        let err = fx.switcher.add_accounts(None, None, Some("x")).unwrap_err();
        assert_eq!(err.type_name(), "ValidationError");
        assert!(
            err.to_string()
                .starts_with("--slot/--alias need a single login"),
            "{err}"
        );
        assert_eq!(fx.switcher.add_accounts(None, None, None).unwrap().len(), 2);
    }

    #[test]
    fn add_token_routes_by_prefix() {
        let mut fx = fixture();
        assert_eq!(
            fx.switcher
                .add_token("sk-ant-api03-key", None, None)
                .unwrap(),
            AddOutcome::Added { slot: 1 }
        );
        assert_eq!(
            fx.switcher
                .add_token("sk-ant-oat01-tok", Some("me@example.com"), None)
                .unwrap(),
            AddOutcome::Added { slot: 2 }
        );
        assert_eq!(
            fx.switcher.add_token("sk-openai", None, None).unwrap(),
            AddOutcome::Added { slot: 3 }
        );
        assert_eq!(
            fx.lines(),
            [
                "Added Account 1: api-key-1@token.local [personal] (from API key)",
                "Added Account 2: me@example.com [personal] (from token)",
                "Added Account 3: api-key-3@token.local [personal] (from API key)",
            ]
        );
        let roster = fx.roster();
        assert_eq!(roster.record(1).unwrap().provider, Provider::Claude);
        assert!(roster.record(1).unwrap().is_api_key());
        assert_eq!(roster.record(2).unwrap().provider, Provider::Claude);
        assert!(
            !roster.record(2).unwrap().is_api_key(),
            "a setup-token is an OAuth-shaped login"
        );
        assert_eq!(roster.record(3).unwrap().provider, Provider::Codex);
        assert_eq!(fx.credential(1)["primaryApiKey"], "sk-ant-api03-key");
        assert_eq!(
            fx.credential(1)["oauthAccount"]["emailAddress"],
            "api-key-1@token.local"
        );
        assert_eq!(
            fx.credential(2)["claudeAiOauth"]["accessToken"],
            "sk-ant-oat01-tok"
        );
        assert_eq!(
            fx.credential(2)["claudeAiOauth"]["scopes"],
            json!(["user:inference"])
        );
        assert_eq!(fx.credential(3)["OPENAI_API_KEY"], "sk-openai");
        fx.lines.borrow_mut().clear();
        assert_eq!(
            fx.switcher
                .add_token("sk-ant-oat01-tok2", Some("me@example.com"), None)
                .unwrap(),
            AddOutcome::Updated { slot: 2 }
        );
        assert_eq!(
            fx.lines(),
            ["Updated token for Account 2 (me@example.com [personal])."]
        );
    }

    #[test]
    fn email_validation_matches_cswap() {
        assert!(valid_email("a.b+c@example.co"));
        assert!(valid_email("x@sub.example.com"));
        assert!(!valid_email("nope"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a@b.c"));
        assert!(!valid_email("a b@example.com"));
        assert!(!valid_email("a@b@example.com"));
        assert!(!valid_email("a@example.c0m"));
    }

    #[test]
    fn line_text_and_slot_arg() {
        let line = Line::new()
            .push(Style::Accent, "Added")
            .push(Style::Plain, " Account 1");
        assert_eq!(line.text(), "Added Account 1");
        assert_eq!(slot_arg(None).unwrap(), None);
        assert_eq!(slot_arg(Some(3)).unwrap(), Some(3));
        assert_eq!(
            slot_arg(Some(0)).unwrap_err().to_string(),
            "Slot number must be >= 1"
        );
    }

    #[test]
    fn add_account_first_second_and_refresh() {
        let mut f = fixture();
        assert_eq!(
            f.switcher
                .add_account(Provider::Codex, None, None)
                .unwrap_err()
                .to_string(),
            "No active Codex account found. Please log in first."
        );
        f.write_live(&chatgpt("A@Example.com", "acct-1", "rt-1"));
        assert_eq!(
            f.switcher
                .add_account(Provider::Codex, None, Some("Dev"))
                .unwrap(),
            AddOutcome::Added { slot: 1 }
        );
        assert_eq!(f.lines(), ["Added Account 1: a@example.com [Plus]"]);
        let roster = f.roster();
        assert_eq!(roster.active_account_number, Some(1));
        let record = roster.record(1).unwrap();
        assert_eq!(record.email, "a@example.com");
        assert_eq!(record.organization_uuid, "acct-1");
        assert_eq!(record.uuid, "user-A@Example.com");
        assert_eq!(record.plan_type.as_deref(), Some("plus"));
        assert_eq!(record.alias.as_deref(), Some("dev"));
        assert!(credentials::exists(&f.switcher.store, 1));

        f.write_live(&chatgpt("b@example.com", "acct-team", "rt-2"));
        assert_eq!(
            f.switcher.add_account(Provider::Codex, None, None).unwrap(),
            AddOutcome::Added { slot: 2 }
        );
        assert_eq!(f.lines()[1], "Added Account 2: b@example.com [Acme]");
        assert_eq!(f.roster().record(2).unwrap().organization_name, "Acme");

        // Same identity again: refreshed in place, alias kept, active moves.
        f.write_live(&chatgpt("a@example.com", "acct-1", "rt-3"));
        assert_eq!(
            f.switcher.add_account(Provider::Codex, None, None).unwrap(),
            AddOutcome::Updated { slot: 1 }
        );
        assert_eq!(
            f.lines()[2],
            "Updated credentials for Account 1 (a@example.com [Plus])."
        );
        let roster = f.roster();
        assert_eq!(roster.active_account_number, Some(1));
        assert_eq!(roster.record(1).unwrap().alias.as_deref(), Some("dev"));
        let stored = credentials::read(&f.switcher.store, 1).unwrap().unwrap();
        assert_eq!(stored["tokens"]["refresh_token"], "rt-3");

        // Alias collision on a refresh of another slot and on a new add.
        f.write_live(&chatgpt("b@example.com", "acct-team", "rt-5"));
        assert_eq!(
            f.switcher
                .add_account(Provider::Codex, None, Some("dev"))
                .unwrap_err()
                .to_string(),
            "Alias 'dev' is already used by account 1"
        );
        f.write_live(&chatgpt("c@example.com", "acct-3", "rt-4"));
        let err = f
            .switcher
            .add_account(Provider::Codex, None, Some("dev"))
            .unwrap_err();
        assert_eq!(err.type_name(), "ValidationError");
        assert!(f.roster().record(3).is_none());
    }

    #[test]
    fn add_account_slot_overwrite_and_migration() {
        let mut f = fixture();
        f.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        f.switcher.add_account(Provider::Codex, None, None).unwrap();
        f.write_live(&chatgpt("b@example.com", "acct-2", "rt-2"));
        f.switcher.add_account(Provider::Codex, None, None).unwrap();
        let mut mappings = MappingStore::load(&f.switcher.store.paths);
        mappings.set(
            Provider::Codex,
            Path::new("/tmp/proj"),
            &Identity::new("a@example.com", "acct-1"),
        );
        mappings.save().unwrap();

        // Different identity onto an occupied slot: prompt, decline.
        f.write_live(&chatgpt("c@example.com", "acct-3", "rt-3"));
        f.answer("n");
        assert_eq!(
            f.switcher
                .add_account(Provider::Codex, Some(1), None)
                .unwrap(),
            AddOutcome::Cancelled
        );
        assert_eq!(
            f.lines()[2..],
            [
                "Slot 1 already occupied",
                "a@example.com [Plus]",
                "PROMPT Overwrite slot 1? [y/N] ",
                "Cancelled",
            ]
        );
        assert_eq!(f.roster().record(1).unwrap().email, "a@example.com");

        // Accept: the occupant and its mappings go away.
        f.answer("y");
        assert_eq!(
            f.switcher
                .add_account(Provider::Codex, Some(1), None)
                .unwrap(),
            AddOutcome::Added { slot: 1 }
        );
        assert_eq!(
            f.lines()[9..],
            [
                "Removed 1 directory mapping(s) for this account",
                "Added Account 1: c@example.com [Plus]",
            ]
        );
        assert_eq!(f.roster().record(1).unwrap().email, "c@example.com");
        assert!(MappingStore::load(&f.switcher.store.paths).is_empty());

        // Same identity to another slot: migrated.
        f.switcher.alias_set("2", "work").unwrap();
        f.write_live(&chatgpt("b@example.com", "acct-2", "rt-9"));
        assert_eq!(
            f.switcher
                .add_account(Provider::Codex, Some(5), None)
                .unwrap(),
            AddOutcome::Added { slot: 5 }
        );
        let lines = f.lines();
        assert_eq!(
            lines[lines.len() - 2..],
            [
                "Moved from slot 2 → 5",
                "Added Account 5: b@example.com [Plus]",
            ]
        );
        let roster = f.roster();
        assert!(roster.record(2).is_none());
        assert_eq!(roster.record(5).unwrap().alias.as_deref(), Some("work"));
        assert_eq!(roster.sequence, vec![1, 5]);
        assert!(!credentials::exists(&f.switcher.store, 2));
        assert!(credentials::exists(&f.switcher.store, 5));

        assert_eq!(
            f.switcher
                .add_account(Provider::Codex, Some(0), None)
                .unwrap_err()
                .to_string(),
            "Slot number must be >= 1"
        );
    }

    #[test]
    fn add_account_accepts_a_live_api_key() {
        let mut f = fixture();
        f.write_live(&json!({"auth_mode": "apikey", "OPENAI_API_KEY": "sk-live"}));
        assert_eq!(
            f.switcher.add_account(Provider::Codex, None, None).unwrap(),
            AddOutcome::Added { slot: 1 }
        );
        assert_eq!(
            f.lines(),
            ["Added Account 1: api-key-1@token.local [personal] (from API key)"]
        );
        let record = f.roster().record(1).unwrap().clone();
        assert!(record.is_api_key());
        assert_eq!(record.organization_uuid, "");
        // The same key again refreshes in place.
        assert_eq!(
            f.switcher.add_account(Provider::Codex, None, None).unwrap(),
            AddOutcome::Updated { slot: 1 }
        );
        assert_eq!(
            f.lines()[1],
            "Updated credentials for Account 1 (api-key-1@token.local [personal])."
        );
        f.write_live(&json!({"nonsense": true}));
        assert_eq!(
            f.switcher
                .add_account(Provider::Codex, None, None)
                .unwrap_err()
                .type_name(),
            "CredentialReadError"
        );
    }

    #[test]
    fn add_token_rules() {
        let mut f = fixture();
        for (token, email, message) in [
            ("  ", None, "Token cannot be empty"),
            (
                "{\"x\":1}",
                None,
                "Token must be an API key or setup-token, not a JSON object",
            ),
            ("sk-1", Some("bad"), "Invalid email format: bad"),
        ] {
            let err = f.switcher.add_token(token, email, None).unwrap_err();
            assert_eq!(err.type_name(), "ValidationError");
            assert_eq!(err.to_string(), message);
        }
        assert_eq!(
            f.switcher.add_token(" sk-1 ", None, None).unwrap(),
            AddOutcome::Added { slot: 1 }
        );
        assert_eq!(
            f.lines(),
            ["Added Account 1: api-key-1@token.local [personal] (from API key)"]
        );
        let stored = credentials::read(&f.switcher.store, 1).unwrap().unwrap();
        assert_eq!(
            stored,
            json!({"auth_mode": "apikey", "OPENAI_API_KEY": "sk-1"})
        );
        assert_eq!(
            f.switcher.add_token("sk-2", None, Some(1)).unwrap(),
            AddOutcome::Updated { slot: 1 }
        );
        assert_eq!(
            f.lines()[1],
            "Updated API key for Account 1 (api-key-1@token.local [personal])."
        );
        assert_eq!(
            f.switcher.add_token("sk-3", Some("me@x.io"), None).unwrap(),
            AddOutcome::Added { slot: 2 }
        );
        assert_eq!(
            f.lines()[2],
            "Added Account 2: me@x.io [personal] (from API key)"
        );

        // Cross-kind collision with an OAuth record that has an empty account id.
        let mut roster = f.roster();
        roster.add_record(7, AccountRecord::new("oauth@x.io"));
        roster::write(&f.switcher.store.paths, &roster).unwrap();
        assert_eq!(
            f.switcher
                .add_token("sk-4", Some("oauth@x.io"), None)
                .unwrap_err()
                .to_string(),
            "'oauth@x.io' already exists as an OAuth account (slot 7); cannot add it as an API-key account. Pass a distinct --email."
        );
    }

    #[test]
    fn alias_move_swap_and_files_follow() {
        let mut f = fixture();
        assert_eq!(
            f.switcher.alias_set("1", "x").unwrap_err().to_string(),
            "No accounts are managed yet"
        );
        f.switcher.alias_list().unwrap();
        assert_eq!(f.lines(), ["No aliases set"]);
        f.switcher.add_token("sk-1", Some("a@x.io"), None).unwrap();
        f.switcher.add_token("sk-2", Some("b@x.io"), None).unwrap();
        f.lines.borrow_mut().clear();

        f.switcher.alias_set("a@x.io", "Dev").unwrap();
        f.switcher.alias_list().unwrap();
        f.switcher.alias_unset("dev").unwrap();
        f.switcher.alias_unset("1").unwrap();
        assert_eq!(
            f.lines(),
            [
                "Set alias 'dev' for Account 1",
                "Aliases:",
                "  1: dev (a@x.io)",
                "Removed alias for Account 1",
                "Removed alias for Account 1",
            ]
        );
        f.lines.borrow_mut().clear();

        let sessions = f.switcher.store.paths.sessions_dir();
        fs::create_dir_all(sessions.join("1-a_x.io")).unwrap();
        fs::write(sessions.join("1-a_x.io/marker"), "a").unwrap();

        f.switcher.move_account("1", "1").unwrap();
        f.switcher.move_account("1", "3").unwrap();
        assert_eq!(
            f.lines(),
            ["Already in slot 1: a@x.io", "Moved a@x.io to slot 3"]
        );
        assert!(!credentials::exists(&f.switcher.store, 1));
        assert!(credentials::exists(&f.switcher.store, 3));
        assert!(sessions.join("3-a_x.io/marker").exists());
        assert_eq!(f.roster().sequence, vec![2, 3]);
        f.lines.borrow_mut().clear();

        f.switcher.move_account("b@x.io", "3").unwrap();
        assert_eq!(
            f.lines(),
            [
                "Swapped Account 2 and Account 3:",
                "  2: a@x.io",
                "  3: b@x.io",
            ]
        );
        assert!(sessions.join("2-a_x.io/marker").exists());
        assert_eq!(
            credentials::read(&f.switcher.store, 2).unwrap().unwrap()["OPENAI_API_KEY"],
            "sk-1"
        );
        assert_eq!(
            credentials::read(&f.switcher.store, 3).unwrap().unwrap()["OPENAI_API_KEY"],
            "sk-2"
        );
        f.lines.borrow_mut().clear();

        f.switcher.swap_accounts("2", "3").unwrap();
        assert_eq!(f.lines()[0], "Swapped Account 2 and Account 3:");
        assert_eq!(f.roster().record(2).unwrap().email, "b@x.io");
        assert!(sessions.join("3-a_x.io/marker").exists());
        assert_eq!(
            f.switcher.swap_accounts("2", "2").unwrap_err().to_string(),
            "Cannot swap an account with itself"
        );
        let err = f.switcher.move_account("2", "x").unwrap_err();
        assert_eq!(err.type_name(), "ValidationError");
        assert!(
            err.to_string()
                .starts_with("Target slot must be a positive slot number, got: 'x'")
        );
        assert_eq!(
            f.switcher.move_account("2", "0").unwrap_err().to_string(),
            "Target slot must be a positive slot number, got: '0' (use `swap` to trade two accounts by identifier)"
        );
    }

    #[test]
    fn purge_prompts_and_removes_the_store() {
        let mut f = fixture();
        f.switcher.add_token("sk-1", None, None).unwrap();
        f.lines.borrow_mut().clear();
        f.answer("n");
        assert!(!f.switcher.purge().unwrap());
        let lines = f.lines();
        assert_eq!(lines[0], "This will remove ALL ccsw data from your system:");
        assert!(lines[1].starts_with("  - Backup directory: "));
        assert_eq!(lines[2], "  - All stored account credential files");
        assert_eq!(
            lines[4],
            "Note: This does NOT affect your current Codex login."
        );
        assert_eq!(
            lines[6],
            "PROMPT Are you sure you want to purge all data? [y/N] "
        );
        assert_eq!(lines[7], "Cancelled");
        assert!(f.switcher.store.paths.backup_root.exists());
        f.lines.borrow_mut().clear();
        f.answer("yes");
        assert!(f.switcher.purge().unwrap());
        assert!(!f.switcher.store.paths.backup_root.exists());
        let lines = f.lines();
        assert_eq!(lines[lines.len() - 3], "Removed:");
        assert_eq!(lines[lines.len() - 1], "Purge complete.");
    }

    #[test]
    fn at_limit_labels() {
        let mut entry = empty_entry(None);
        assert_eq!(at_limit_label(Some(&entry), &[]), None);
        entry.last_good = Some(crate::model::NormalizedUsage {
            five_hour: Some(crate::model::WindowUsage {
                pct: 100.0,
                resets_at: None,
            }),
            seven_day: Some(crate::model::WindowUsage {
                pct: 40.0,
                resets_at: None,
            }),
            scoped: vec![crate::model::ScopedWindow {
                name: "Spark".into(),
                pct: 100.0,
                resets_at: None,
            }],
            ..Default::default()
        });
        entry.age_s = Some(10.0);
        assert_eq!(at_limit_label(Some(&entry), &[]).as_deref(), Some("5h"));
        assert_eq!(
            at_limit_label(Some(&entry), &["spark".to_string()]).as_deref(),
            Some("5h/Spark")
        );
        entry.age_s = Some(9999.0);
        assert_eq!(
            at_limit_label(Some(&entry), &[]),
            None,
            "stale usage is unknown"
        );
        assert_eq!(limits_label(&[]), "5h/7d limit");
        assert_eq!(limits_label(&["x".into()]), "usage limits");
    }

    #[test]
    fn active_profile_blocks_switch_remove_and_move() {
        let mut fx = fixture();
        fx.write_claude_live("session@example.com", "org", "", "rt-session");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        fx.write_claude_live("default@example.com", "org-default", "", "rt-default");
        let profile = fx
            .switcher
            .store
            .paths
            .session_dir(1, "session@example.com");
        crate::fsutil::write_json_private(
            &profile.join("sessions/owner.json"),
            &json!({"pid": std::process::id()}),
        )
        .unwrap();
        assert!(fx.switcher.switch_to("1", false, true).is_err());
        assert!(fx.switcher.move_account("1", "2").is_err());
        fx.answer("y");
        assert!(fx.switcher.remove("1", false).is_err());
        assert!(fx.roster().record(1).is_some());
        assert_eq!(
            fx.claude_credentials()["claudeAiOauth"]["refreshToken"],
            "rt-default"
        );
    }

    #[test]
    fn switch_to_a_claude_slot_rewrites_the_live_login_and_keeps_siblings() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-codex"));
        fx.write_claude_live("one@example.com", "org-1", "", "crt-1");
        fx.switcher.add_accounts(None, None, None).unwrap();
        fx.write_claude_live("two@example.com", "org-2", "Acme", "crt-2");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        let codex_before = fx.live();
        fx.lines.borrow_mut().clear();

        let report = fx.switcher.switch_to("2", false, true).unwrap().unwrap();
        assert!(report.outcome.switched);
        assert_eq!(report.outcome.provider, Provider::Claude);
        assert_eq!(report.outcome.from.as_ref().unwrap().number, Some(3));
        assert_eq!(report.outcome.to.as_ref().unwrap().email, "one@example.com");
        assert_eq!(
            report.followup.as_deref(),
            Some(crate::claude::live::FILE_FOLLOWUP)
        );
        assert!(report.show_list);
        assert_eq!(fx.lines(), ["Switched to Account-2 (one@example.com)"]);

        let creds = fx.claude_credentials();
        assert_eq!(creds["claudeAiOauth"]["refreshToken"], "crt-1");
        assert_eq!(
            creds["mcpOAuth"]["srv"]["accessToken"], "m",
            "siblings survive"
        );
        let config = fx.claude_config();
        assert_eq!(config["oauthAccount"]["emailAddress"], "one@example.com");
        assert_eq!(config["oauthAccount"]["organizationUuid"], "org-1");
        assert_eq!(
            config["projects"]["/p"],
            json!({}),
            "other config keys survive"
        );
        assert_eq!(fx.live(), codex_before, "the Codex login is untouched");
        assert_eq!(fx.roster().active_for(Provider::Claude), Some(2));
        assert_eq!(fx.roster().active_for(Provider::Codex), Some(1));
        let backups = fs::read_dir(fx.switcher.store.paths.claude_backups_dir())
            .unwrap()
            .count();
        assert_eq!(backups, 1);
        let paths = &fx.switcher.store.paths;
        assert!(
            !paths.claude_refresh_lock_dir().exists()
                && !paths.claude_legacy_lock_dir().exists()
                && !paths.claude_config_lock_dir().exists()
        );

        fx.lines.borrow_mut().clear();
        let again = fx.switcher.switch_to("2", false, true).unwrap().unwrap();
        assert_eq!(again.outcome.reason, "already-active");
        assert_eq!(fx.lines()[0], "Already on Account-2 (one@example.com)");
    }

    #[test]
    fn switch_folds_a_rotated_live_claude_login_back_first() {
        let mut fx = fixture();
        fx.write_claude_live("one@example.com", "org-1", "", "crt-1");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        fx.write_claude_live("two@example.com", "org-2", "", "crt-2");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        // Claude Code rotated account two's token while it was live.
        let paths = fx.switcher.store.paths.clone();
        fs::write(
            paths.claude_credentials_file(),
            json!({"claudeAiOauth": {"accessToken": "cat-rotated", "refreshToken": "crt-2b", "expiresAt": 4_102_444_801_000i64}}).to_string(),
        )
        .unwrap();
        fx.switcher.switch_to("1", false, true).unwrap();
        assert_eq!(
            fx.credential(2)["claudeAiOauth"]["refreshToken"],
            "crt-2b",
            "folded back before leaving"
        );
        assert_eq!(
            fx.claude_credentials()["claudeAiOauth"]["refreshToken"],
            "crt-1"
        );
        let err = fx.switcher.switch_to("9", false, true).unwrap_err();
        assert_eq!(err.to_string(), "Account-9 does not exist");
    }

    fn claude_slot_file(fx: &Fixture, slot: u32, value: Value) {
        credentials::write(&fx.switcher.store, slot, &value).unwrap();
    }

    #[test]
    fn switch_refuses_a_slot_without_a_login_and_changes_nothing() {
        let mut fx = fixture();
        fx.write_claude_live("one@example.com", "org-1", "", "crt-1");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        fx.write_claude_live("two@example.com", "org-2", "", "crt-2");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        let account = fx.credential(1)["oauthAccount"].clone();
        claude_slot_file(&fx, 1, json!({"oauthAccount": account}));
        let creds_before = fs::read(fx.switcher.store.paths.claude_credentials_file()).unwrap();
        let config_before = fs::read(fx.switcher.store.paths.claude_global_config_file()).unwrap();

        let err = fx.switcher.switch_to("1", false, true).unwrap_err();
        assert!(err.to_string().contains("unusable"), "{err}");
        let paths = &fx.switcher.store.paths;
        assert_eq!(
            fs::read(paths.claude_credentials_file()).unwrap(),
            creds_before
        );
        assert_eq!(
            fs::read(paths.claude_global_config_file()).unwrap(),
            config_before
        );
        assert!(!paths.claude_backups_dir().exists());
        assert!(
            !paths.claude_refresh_lock_dir().exists() && !paths.claude_config_lock_dir().exists()
        );
    }

    #[test]
    fn mcp_oauth_siblings_survive_a_round_trip_through_an_api_key_slot() {
        let mut fx = fixture();
        fx.write_claude_live("one@example.com", "org-1", "", "crt-1");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        fx.switcher
            .add_token("sk-ant-api03-xyz", None, None)
            .unwrap();
        let mcp_before = fx.claude_credentials()["mcpOAuth"].clone();

        fx.switcher.switch_to("2", false, true).unwrap();
        assert_eq!(fx.claude_credentials()["mcpOAuth"], mcp_before);
        fx.switcher.switch_to("1", false, true).unwrap();
        let creds = fx.claude_credentials();
        assert_eq!(
            creds["mcpOAuth"], mcp_before,
            "siblings survive the round trip"
        );
        assert_eq!(creds["claudeAiOauth"]["refreshToken"], "crt-1");
        let mut names: Vec<_> = fs::read_dir(fx.switcher.store.paths.claude_backups_dir())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        let first: Value = serde_json::from_str(&fs::read_to_string(&names[0]).unwrap()).unwrap();
        assert_eq!(
            first["credentials"]["mcpOAuth"], mcp_before,
            "the backup keeps siblings"
        );
    }

    #[test]
    fn a_failed_config_splice_restores_the_previous_live_login() {
        let mut fx = fixture();
        fx.write_claude_live("one@example.com", "org-1", "", "crt-1");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        fx.write_claude_live("two@example.com", "org-2", "", "crt-2");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        let creds_before = fx.claude_credentials();
        let active_before = fx.roster().active_for(Provider::Claude);

        FAIL_CONFIG_SPLICE.with(|f| f.set(true));
        let result = fx.switcher.switch_to("1", false, true);
        FAIL_CONFIG_SPLICE.with(|f| f.set(false));
        assert!(result.is_err());
        assert_eq!(fx.claude_credentials(), creds_before);
        assert_eq!(fx.roster().active_for(Provider::Claude), active_before);
    }

    #[test]
    fn rotation_needs_a_selector_when_both_providers_have_accounts() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        fx.switcher
            .add_account(Provider::Codex, None, None)
            .unwrap();
        fx.write_live(&chatgpt("b@example.com", "acct-2", "rt-2"));
        fx.switcher
            .add_account(Provider::Codex, None, None)
            .unwrap();
        let report = fx
            .switcher
            .switch(None, Strategy::Rotation, &[], false)
            .unwrap();
        assert_eq!(
            report.outcome.to.as_ref().unwrap().number,
            Some(1),
            "one provider: rotation as before"
        );
        fx.write_claude_live("c@example.com", "org", "", "crt-1");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        let err = fx
            .switcher
            .switch(None, Strategy::Rotation, &[], false)
            .unwrap_err();
        assert_eq!(err.type_name(), "ConfigError");
        assert_eq!(
            err.to_string(),
            "Both Codex and Claude accounts are managed — say which: ccsw switch codex | ccsw switch claude"
        );
        fx.lines.borrow_mut().clear();
        let report = fx
            .switcher
            .switch(Some(Provider::Claude), Strategy::Rotation, &[], false)
            .unwrap();
        assert_eq!(report.outcome.reason, "only-one-account");
        assert_eq!(report.outcome.provider, Provider::Claude);
        let report = fx
            .switcher
            .switch(Some(Provider::Codex), Strategy::Rotation, &[], false)
            .unwrap();
        assert_eq!(report.outcome.to.as_ref().unwrap().number, Some(2));
        assert_eq!(report.outcome.provider, Provider::Codex);
    }

    #[test]
    fn rotation_stays_inside_the_selected_provider() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        fx.switcher
            .add_account(Provider::Codex, None, None)
            .unwrap();
        fx.write_claude_live("one@example.com", "org-1", "", "crt-1");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        fx.write_claude_live("two@example.com", "org-2", "", "crt-2");
        fx.switcher
            .add_account(Provider::Claude, None, None)
            .unwrap();
        let report = fx
            .switcher
            .switch(Some(Provider::Claude), Strategy::Rotation, &[], false)
            .unwrap();
        assert_eq!(report.outcome.from.as_ref().unwrap().number, Some(3));
        assert_eq!(
            report.outcome.to.as_ref().unwrap().number,
            Some(2),
            "slot 1 (Codex) is never a Claude target"
        );
        let report = fx
            .switcher
            .switch(Some(Provider::Claude), Strategy::Rotation, &[], false)
            .unwrap();
        assert_eq!(report.outcome.to.as_ref().unwrap().number, Some(3));
        assert_eq!(
            fx.roster().active_for(Provider::Codex),
            Some(1),
            "the Codex marker never moved"
        );
    }

    #[test]
    fn list_and_status_report_one_active_per_provider() {
        let mut fx = fixture();
        fx.write_live(&chatgpt("a@example.com", "acct-1", "rt-1"));
        fx.write_claude_live("c@example.com", "org", "Acme", "crt-1");
        fx.switcher.add_accounts(None, None, None).unwrap();
        let list = fx.switcher.list_snapshot(false).unwrap().unwrap();
        assert_eq!(
            list.actives,
            ActiveSlots {
                codex: Some(1),
                claude: Some(2)
            }
        );
        assert!(list.rows.iter().all(|row| row.is_active));
        assert_eq!(list.rows[1].record.provider, Provider::Claude);
        let status = fx.switcher.status().unwrap();
        assert_eq!(status.total, 2);
        assert_eq!(status.providers.len(), 2);
        assert_eq!(status.providers[0].provider, Provider::Codex);
        assert_eq!(status.providers[0].current.slot(), Some(1));
        assert_eq!(status.providers[1].provider, Provider::Claude);
        assert_eq!(status.providers[1].current.slot(), Some(2));
        assert_eq!(
            status.providers[1].row.as_ref().unwrap().record.email,
            "c@example.com"
        );
        fx.switcher.remove("2", false).unwrap_or_default();
    }
}
