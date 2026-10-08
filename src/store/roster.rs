//! `sequence.json` read/write and slot operations.
//!
//! The roster is read *strictly*: a present-but-broken file is an error naming the
//! file, never silently replaced by an empty roster.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

use crate::errors::{CswitchError, Result};
use crate::fsutil::write_json_private;
use crate::model::{AccountRecord, Roster};
use crate::paths::Paths;
use crate::provider::Provider;

/// Read `sequence.json`; `Ok(None)` when it does not exist.
pub fn read(paths: &Paths) -> Result<Option<Roster>> {
    let path = paths.sequence_file();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(CswitchError::config(format!(
                "{} exists but could not be read ({err}). Fix what is blocking the read, then retry.",
                path.display()
            )));
        }
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|err| unparseable(&path, &err))?;
    if !value.is_object() {
        return Err(CswitchError::config(format!(
            "{} holds {}, not a JSON object. Repair or move it, then retry.",
            path.display(),
            python_type_name(&value)
        )));
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|err| unparseable(&path, &err))
}

fn unparseable(path: &Path, err: &dyn std::fmt::Display) -> CswitchError {
    CswitchError::config(format!(
        "{} exists but could not be parsed ({err}). Repair or move it, then retry — refusing to overwrite it unread.",
        path.display()
    ))
}

// The message mirrors cswap, which names Python's type.
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

/// Strict read, with an empty roster standing in for an absent file.
pub fn read_or_empty(paths: &Paths) -> Result<Roster> {
    Ok(read(paths)?.unwrap_or_else(Roster::empty))
}

/// Write `sequence.json` atomically (0600, parent 0700).
pub fn write(paths: &Paths, roster: &Roster) -> Result<()> {
    let path = paths.sequence_file();
    let value = serde_json::to_value(roster)
        .map_err(|err| CswitchError::config(format!("Generated invalid JSON: {err}")))?;
    write_json_private(&path, &value)
        .map_err(|err| CswitchError::config(format!("{}: {err}", path.display())))
}

/// Return the roster, writing the initial empty file when none exists.
pub fn init_if_absent(paths: &Paths) -> Result<Roster> {
    if let Some(roster) = read(paths)? {
        return Ok(roster);
    }
    let roster = Roster::empty();
    write(paths, &roster)?;
    Ok(roster)
}

fn missing(slot: u32) -> CswitchError {
    CswitchError::AccountNotFound(format!("Account-{slot} does not exist"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveOutcome {
    /// Source and target are the same slot.
    NoOp,
    /// The target slot was empty; the account now lives there.
    Relocated,
    /// The target slot was occupied; the two accounts traded places.
    Swapped,
}

impl Roster {
    /// Slot numbers of every record, ascending.
    pub fn sorted_slots(&self) -> Vec<u32> {
        let mut slots: Vec<u32> = self
            .accounts
            .keys()
            .filter_map(|key| key.parse().ok())
            .collect();
        slots.sort_unstable();
        slots
    }

    pub fn set_active(&mut self, slot: Option<u32>) {
        self.set_active_for(Provider::Codex, slot);
    }

    /// Insert (or replace) the record for `slot` and keep it in the sorted sequence.
    pub fn add_record(&mut self, slot: u32, record: AccountRecord) {
        self.accounts.insert(slot.to_string(), record);
        if !self.sequence.contains(&slot) {
            self.sequence.push(slot);
        }
        self.sequence.sort_unstable();
        self.touch();
    }

    /// Drop the record and its sequence entry, and clear every active marker
    /// (`activeAccountNumber` and each `activeByProvider` entry) that pointed
    /// at it. The active login is re-derived from the live files on every
    /// command, so these fields are a cached hint and must never name a
    /// record that no longer exists.
    pub fn remove_slot(&mut self, slot: u32) -> Option<AccountRecord> {
        let record = self.accounts.remove(&slot.to_string())?;
        self.sequence.retain(|s| *s != slot);
        self.active_by_provider.retain(|_, active| *active != slot);
        if self.active_account_number == Some(slot) {
            self.active_account_number = None;
        }
        self.touch();
        Some(record)
    }

    /// Set a validated alias; returns the normalized alias.
    pub fn set_alias(&mut self, slot: u32, alias: &str) -> Result<String> {
        let alias = normalize_alias(alias)?;
        if self.record(slot).is_none() {
            return Err(missing(slot));
        }
        if let Some(owner) = alias_owner(self, &alias)
            && owner != slot
        {
            return Err(CswitchError::config(format!(
                "Alias '{alias}' is already used by account {owner}"
            )));
        }
        let record = self.record_mut(slot).ok_or_else(|| missing(slot))?;
        record.alias = Some(alias.clone());
        self.touch();
        Ok(alias)
    }

    /// Returns whether an alias was set.
    pub fn unset_alias(&mut self, slot: u32) -> Result<bool> {
        let record = self.record_mut(slot).ok_or_else(|| missing(slot))?;
        let was_set = record.alias.take().is_some();
        if was_set {
            self.touch();
        }
        Ok(was_set)
    }

    /// Returns whether the flag changed.
    pub fn set_disabled(&mut self, slot: u32, disabled: bool) -> Result<bool> {
        let record = self.record_mut(slot).ok_or_else(|| missing(slot))?;
        if record.disabled == disabled {
            return Ok(false);
        }
        record.disabled = disabled;
        self.touch();
        Ok(true)
    }

    /// `move ACCOUNT TARGET`: an empty target relocates, an occupied one swaps.
    /// Everything keyed on the slot (alias, disabled, sequence membership, the active
    /// marker) travels with the record; credential files are the caller's job.
    pub fn move_slot(&mut self, src: u32, target: u32) -> Result<MoveOutcome> {
        if self.record(src).is_none() {
            return Err(missing(src));
        }
        if target < 1 {
            return Err(CswitchError::validation(format!(
                "Target slot must be a positive slot number, got: '{target}' (use `swap` to trade two accounts by identifier)"
            )));
        }
        let cap = self.sorted_slots().last().copied().unwrap_or(0).max(99);
        if target > cap {
            return Err(CswitchError::validation(format!(
                "Target slot {target} is out of range (1-{cap}): new accounts are numbered from the highest slot, so a large target would inflate future account numbers"
            )));
        }
        if src == target {
            return Ok(MoveOutcome::NoOp);
        }
        if self.record(target).is_some() {
            self.swap_slots(src, target)?;
            return Ok(MoveOutcome::Swapped);
        }
        let record = self
            .accounts
            .remove(&src.to_string())
            .ok_or_else(|| missing(src))?;
        self.accounts.insert(target.to_string(), record);
        self.renumber(src, target);
        Ok(MoveOutcome::Relocated)
    }

    pub fn swap_slots(&mut self, a: u32, b: u32) -> Result<()> {
        if a == b {
            return Err(CswitchError::validation(
                "Cannot swap an account with itself",
            ));
        }
        for slot in [a, b] {
            if self.record(slot).is_none() {
                return Err(missing(slot));
            }
        }
        let record_a = self
            .accounts
            .remove(&a.to_string())
            .ok_or_else(|| missing(a))?;
        let record_b = self
            .accounts
            .remove(&b.to_string())
            .ok_or_else(|| missing(b))?;
        self.accounts.insert(a.to_string(), record_b);
        self.accounts.insert(b.to_string(), record_a);
        self.renumber(a, b);
        Ok(())
    }

    /// Exchange `a` and `b` in the sequence and the active marker, then re-sort.
    fn renumber(&mut self, a: u32, b: u32) {
        for slot in self.sequence.iter_mut() {
            if *slot == a {
                *slot = b;
            } else if *slot == b {
                *slot = a;
            }
        }
        self.sequence.sort_unstable();
        self.sequence.dedup();
        self.active_account_number = match self.active_account_number {
            Some(n) if n == a => Some(b),
            Some(n) if n == b => Some(a),
            other => other,
        };
        for value in self.active_by_provider.values_mut() {
            if *value == a {
                *value = b;
            } else if *value == b {
                *value = a;
            }
        }
        self.touch();
    }

    /// Candidate universe for rotation, strategies and auto-switch: sequence order,
    /// record present, credentials stored, not disabled.
    pub fn switchable_slots(&self, has_credentials: impl Fn(u32) -> bool) -> Vec<u32> {
        self.sequence
            .iter()
            .copied()
            .filter(|slot| {
                self.record(*slot)
                    .is_some_and(|record| !record.disabled && has_credentials(*slot))
            })
            .collect()
    }
}

/// Strip, lowercase, validate: the alias grammar shared by `alias`, `add --alias` and
/// import.
pub fn normalize_alias(name: &str) -> Result<String> {
    let alias = name.trim().to_lowercase();
    if alias.is_empty() {
        return Err(CswitchError::validation("alias cannot be empty"));
    }
    if alias.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(CswitchError::validation(format!(
            "alias '{alias}' cannot be purely numeric (reserved for slot numbers)"
        )));
    }
    if alias.starts_with('-') {
        return Err(CswitchError::validation(format!(
            "alias '{alias}' cannot start with '-' (would be read as a command flag)"
        )));
    }
    if Provider::parse_selector(&alias).is_some() {
        return Err(CswitchError::validation(format!(
            "alias '{alias}' is reserved for the provider selector"
        )));
    }
    if !alias
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '.' | '-'))
    {
        return Err(CswitchError::validation(format!(
            "alias '{alias}' may only contain letters, digits, '-', '_', and '.'"
        )));
    }
    Ok(alias)
}

/// The slot whose alias equals `alias` (case-insensitive); empty aliases never match.
pub fn alias_owner(roster: &Roster, alias: &str) -> Option<u32> {
    if alias.is_empty() {
        return None;
    }
    let wanted = alias.to_lowercase();
    roster.sorted_slots().into_iter().find(|slot| {
        roster
            .record(*slot)
            .and_then(|record| record.alias.as_deref())
            .is_some_and(|a| !a.is_empty() && a.to_lowercase() == wanted)
    })
}

/// Precedence number → alias → email. A numeric identifier is returned as-is (the
/// record may not exist); an email matching several accounts is an error.
pub fn resolve_identifier(roster: &Roster, identifier: &str) -> Result<Option<u32>> {
    if !identifier.is_empty() && identifier.chars().all(|ch| ch.is_ascii_digit()) {
        return Ok(identifier.parse().ok());
    }
    if let Some(slot) = alias_owner(roster, identifier) {
        return Ok(Some(slot));
    }
    let matches: Vec<u32> = roster
        .sorted_slots()
        .into_iter()
        .filter(|slot| roster.record(*slot).is_some_and(|r| r.email == identifier))
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [slot] => Ok(Some(*slot)),
        many => {
            let listed: Vec<String> = many
                .iter()
                .map(|slot| {
                    let tag = roster
                        .record(*slot)
                        .map(|r| r.display_tag())
                        .unwrap_or_default();
                    format!("{slot} [{tag}]")
                })
                .collect();
            Err(CswitchError::config(format!(
                "Email '{identifier}' is ambiguous — matches accounts: {}. Use account number instead (e.g., cswitch switch 1).",
                listed.join(", ")
            )))
        }
    }
}

/// `resolve_identifier` that must land on an existing record.
pub fn resolve_slot(roster: &Roster, identifier: &str) -> Result<u32> {
    let slot = resolve_identifier(roster, identifier)?
        .ok_or_else(|| CswitchError::not_found(identifier))?;
    if roster.record(slot).is_none() {
        return Err(missing(slot));
    }
    Ok(slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AccountRecord;
    use crate::provider::Provider;
    use crate::store::temp_store;

    fn record(email: &str, account_id: &str) -> AccountRecord {
        let mut record = AccountRecord::new(email);
        record.organization_uuid = account_id.into();
        record
    }

    fn roster_with(slots: &[(u32, &str, &str)]) -> Roster {
        let mut roster = Roster::empty();
        for (slot, email, account_id) in slots {
            roster.add_record(*slot, record(email, account_id));
        }
        roster
    }

    #[test]
    fn remove_slot_clears_every_active_marker_that_pointed_at_it() {
        let mut roster = roster_with(&[(1, "a@x.com", ""), (2, "b@x.com", "")]);
        roster.record_mut(2).unwrap().provider = Provider::Claude;
        roster.set_active_for(Provider::Codex, Some(1));
        roster.set_active_for(Provider::Claude, Some(2));
        roster.remove_slot(2);
        assert_eq!(roster.active_for(Provider::Codex), Some(1));
        assert_eq!(roster.active_for(Provider::Claude), None);
        roster.remove_slot(1);
        assert_eq!(roster.active_for(Provider::Codex), None);
        assert_eq!(roster.active_account_number, None);
    }

    #[test]
    fn alias_cannot_be_a_provider_selector() {
        for word in ["codex", "Claude"] {
            let err = normalize_alias(word).unwrap_err();
            assert_eq!(err.type_name(), "ValidationError");
            assert_eq!(
                err.to_string(),
                format!(
                    "alias '{}' is reserved for the provider selector",
                    word.to_lowercase()
                )
            );
        }
    }

    #[test]
    fn move_and_swap_renumber_every_provider_marker() {
        let mut roster = roster_with(&[(1, "a@x.com", ""), (2, "b@x.com", "")]);
        roster.record_mut(2).unwrap().provider = Provider::Claude;
        roster.set_active_for(Provider::Codex, Some(1));
        roster.set_active_for(Provider::Claude, Some(2));
        roster.swap_slots(1, 2).unwrap();
        assert_eq!(roster.active_for(Provider::Codex), Some(2));
        assert_eq!(roster.active_for(Provider::Claude), Some(1));
        assert_eq!(roster.active_account_number, Some(2));
        assert_eq!(roster.move_slot(1, 7).unwrap(), MoveOutcome::Relocated);
        assert_eq!(roster.active_for(Provider::Claude), Some(7));
    }

    #[test]
    fn read_absent_is_none_and_init_writes_the_initial_file() {
        let (_dir, store) = temp_store();
        assert_eq!(read(&store.paths).unwrap(), None);
        let roster = init_if_absent(&store.paths).unwrap();
        assert!(roster.accounts.is_empty());
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.paths.sequence_file()).unwrap()).unwrap();
        assert_eq!(raw["activeAccountNumber"], serde_json::Value::Null);
        assert_eq!(raw["sequence"], serde_json::json!([]));
        assert_eq!(raw["accounts"], serde_json::json!({}));
        assert!(raw["lastUpdated"].as_str().unwrap().ends_with('Z'));
        // A second init keeps the existing file.
        let mut roster = roster;
        roster.add_record(1, record("a@x.com", ""));
        write(&store.paths, &roster).unwrap();
        assert_eq!(init_if_absent(&store.paths).unwrap(), roster);
        assert_eq!(read_or_empty(&store.paths).unwrap(), roster);
    }

    #[test]
    fn strict_read_errors_name_the_file() {
        let (_dir, store) = temp_store();
        let path = store.paths.sequence_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        let err = read(&store.paths).unwrap_err();
        assert_eq!(err.type_name(), "ConfigError");
        let msg = err.to_string();
        assert!(msg.starts_with(&format!(
            "{} exists but could not be parsed (",
            path.display()
        )));
        assert!(
            msg.ends_with("). Repair or move it, then retry — refusing to overwrite it unread.")
        );

        std::fs::write(&path, "[1, 2]").unwrap();
        assert_eq!(
            read(&store.paths).unwrap_err().to_string(),
            format!(
                "{} holds list, not a JSON object. Repair or move it, then retry.",
                path.display()
            )
        );

        std::fs::write(&path, "{\"accounts\": {\"1\": {\"uuid\": \"no-email\"}}}").unwrap();
        assert!(
            read(&store.paths)
                .unwrap_err()
                .to_string()
                .contains("could not be parsed")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_roster_is_a_config_error() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, store) = temp_store();
        let path = store.paths.sequence_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let msg = read(&store.paths).unwrap_err().to_string();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(msg.starts_with(&format!(
            "{} exists but could not be read (",
            path.display()
        )));
        assert!(msg.ends_with("). Fix what is blocking the read, then retry."));
    }

    #[test]
    fn add_and_remove_keep_the_sequence_sorted() {
        let mut roster = roster_with(&[(3, "c@x.com", ""), (1, "a@x.com", "")]);
        assert_eq!(roster.sequence, vec![1, 3]);
        roster.add_record(2, record("b@x.com", ""));
        assert_eq!(roster.sequence, vec![1, 2, 3]);
        roster.add_record(2, record("b2@x.com", ""));
        assert_eq!(
            roster.sequence,
            vec![1, 2, 3],
            "re-adding a slot does not duplicate it"
        );
        assert_eq!(roster.record(2).unwrap().email, "b2@x.com");
        assert_eq!(roster.remove_slot(2).unwrap().email, "b2@x.com");
        assert_eq!(roster.sequence, vec![1, 3]);
        assert!(roster.remove_slot(2).is_none());
        assert_eq!(roster.sorted_slots(), vec![1, 3]);
    }

    #[test]
    fn set_active_touches_last_updated() {
        let mut roster = roster_with(&[(1, "a@x.com", "")]);
        roster.last_updated = "old".into();
        roster.set_active(Some(1));
        assert_eq!(roster.active_account_number, Some(1));
        assert_ne!(roster.last_updated, "old");
    }

    #[test]
    fn alias_normalization_messages() {
        assert_eq!(normalize_alias("  Dev ").unwrap(), "dev");
        assert_eq!(normalize_alias("a.b-c_1").unwrap(), "a.b-c_1");
        let cases = [
            ("", "alias cannot be empty"),
            ("   ", "alias cannot be empty"),
            (
                "42",
                "alias '42' cannot be purely numeric (reserved for slot numbers)",
            ),
            (
                "-x",
                "alias '-x' cannot start with '-' (would be read as a command flag)",
            ),
            (
                "a b",
                "alias 'a b' may only contain letters, digits, '-', '_', and '.'",
            ),
            (
                "Ünï",
                "alias 'ünï' may only contain letters, digits, '-', '_', and '.'",
            ),
        ];
        for (input, message) in cases {
            let err = normalize_alias(input).unwrap_err();
            assert_eq!(err.type_name(), "ValidationError", "{input:?}");
            assert_eq!(err.to_string(), message, "{input:?}");
        }
    }

    #[test]
    fn alias_set_unset_and_conflicts() {
        let mut roster = roster_with(&[(1, "a@x.com", ""), (2, "b@x.com", "")]);
        assert_eq!(roster.set_alias(1, "Dev").unwrap(), "dev");
        assert_eq!(roster.record(1).unwrap().alias.as_deref(), Some("dev"));
        assert_eq!(alias_owner(&roster, "DEV"), Some(1));
        assert_eq!(alias_owner(&roster, "nope"), None);
        // Re-setting the same alias on its owner is fine; another slot is a conflict.
        assert_eq!(roster.set_alias(1, "dev").unwrap(), "dev");
        let err = roster.set_alias(2, "dev").unwrap_err();
        assert_eq!(err.type_name(), "ConfigError");
        assert_eq!(err.to_string(), "Alias 'dev' is already used by account 1");
        assert_eq!(
            roster.set_alias(9, "x").unwrap_err().to_string(),
            "Account-9 does not exist"
        );
        assert!(roster.unset_alias(1).unwrap());
        assert!(!roster.unset_alias(1).unwrap());
        assert!(roster.record(1).unwrap().alias.is_none());
    }

    #[test]
    fn disable_enable_report_changes() {
        let mut roster = roster_with(&[(1, "a@x.com", "")]);
        assert!(roster.set_disabled(1, true).unwrap());
        assert!(!roster.set_disabled(1, true).unwrap());
        assert!(roster.record(1).unwrap().disabled);
        assert!(roster.set_disabled(1, false).unwrap());
        assert!(!roster.set_disabled(1, false).unwrap());
        assert_eq!(
            roster.set_disabled(2, true).unwrap_err().type_name(),
            "AccountNotFoundError"
        );
    }

    #[test]
    fn move_relocates_swaps_or_noops() {
        let mut roster = roster_with(&[(1, "a@x.com", ""), (2, "b@x.com", "")]);
        roster.set_active(Some(2));
        assert_eq!(roster.move_slot(2, 2).unwrap(), MoveOutcome::NoOp);

        assert_eq!(roster.move_slot(2, 5).unwrap(), MoveOutcome::Relocated);
        assert!(roster.record(2).is_none());
        assert_eq!(roster.record(5).unwrap().email, "b@x.com");
        assert_eq!(roster.sequence, vec![1, 5]);
        assert_eq!(roster.active_account_number, Some(5));

        roster.record_mut(1).unwrap().alias = Some("dev".into());
        roster.record_mut(5).unwrap().disabled = true;
        assert_eq!(roster.move_slot(5, 1).unwrap(), MoveOutcome::Swapped);
        assert_eq!(roster.record(1).unwrap().email, "b@x.com");
        assert!(roster.record(1).unwrap().disabled);
        assert_eq!(roster.record(5).unwrap().alias.as_deref(), Some("dev"));
        assert_eq!(roster.sequence, vec![1, 5]);
        assert_eq!(roster.active_account_number, Some(1));

        assert_eq!(
            roster.move_slot(7, 1).unwrap_err().to_string(),
            "Account-7 does not exist"
        );
        let err = roster.move_slot(1, 0).unwrap_err();
        assert_eq!(err.type_name(), "ValidationError");
        let err = roster.move_slot(1, 100).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Target slot 100 is out of range (1-99): new accounts are numbered from the highest slot, so a large target would inflate future account numbers"
        );
        roster.add_record(120, record("z@x.com", ""));
        assert_eq!(roster.move_slot(1, 100).unwrap(), MoveOutcome::Relocated);
    }

    #[test]
    fn swap_exchanges_records_and_active() {
        let mut roster = roster_with(&[(1, "a@x.com", ""), (3, "c@x.com", "")]);
        roster.set_active(Some(1));
        roster.swap_slots(1, 3).unwrap();
        assert_eq!(roster.record(1).unwrap().email, "c@x.com");
        assert_eq!(roster.record(3).unwrap().email, "a@x.com");
        assert_eq!(roster.sequence, vec![1, 3]);
        assert_eq!(roster.active_account_number, Some(3));
        assert_eq!(
            roster.swap_slots(1, 1).unwrap_err().to_string(),
            "Cannot swap an account with itself"
        );
        assert_eq!(
            roster.swap_slots(1, 4).unwrap_err().to_string(),
            "Account-4 does not exist"
        );
    }

    #[test]
    fn switchable_slots_follow_sequence_and_skip_disabled_or_credential_less() {
        let mut roster = roster_with(&[(1, "a@x.com", ""), (2, "b@x.com", ""), (3, "c@x.com", "")]);
        roster.sequence.push(9); // stale entry without a record
        roster.set_disabled(2, true).unwrap();
        let slots = roster.switchable_slots(|slot| slot != 3);
        assert_eq!(slots, vec![1]);
        roster.set_disabled(2, false).unwrap();
        assert_eq!(roster.switchable_slots(|_| true), vec![1, 2, 3]);
    }

    #[test]
    fn identifier_resolution_precedence() {
        let mut roster = roster_with(&[
            (1, "a@x.com", ""),
            (2, "dup@x.com", "acct-1"),
            (3, "dup@x.com", "acct-2"),
        ]);
        roster.set_alias(1, "dup@x.com").unwrap_err(); // '@' is not allowed in an alias
        roster.set_alias(1, "team").unwrap();
        roster.record_mut(3).unwrap().organization_name = "Acme".into();

        assert_eq!(resolve_identifier(&roster, "1").unwrap(), Some(1));
        assert_eq!(
            resolve_identifier(&roster, "7").unwrap(),
            Some(7),
            "digits pass through"
        );
        assert_eq!(resolve_identifier(&roster, "TEAM").unwrap(), Some(1));
        assert_eq!(resolve_identifier(&roster, "a@x.com").unwrap(), Some(1));
        assert_eq!(
            resolve_identifier(&roster, "A@x.com").unwrap(),
            None,
            "email is case-sensitive"
        );
        assert_eq!(resolve_identifier(&roster, "nobody").unwrap(), None);
        let err = resolve_identifier(&roster, "dup@x.com").unwrap_err();
        assert_eq!(err.type_name(), "ConfigError");
        assert_eq!(
            err.to_string(),
            "Email 'dup@x.com' is ambiguous — matches accounts: 2 [personal], 3 [Acme]. Use account number instead (e.g., cswitch switch 1)."
        );

        assert_eq!(resolve_slot(&roster, "team").unwrap(), 1);
        assert_eq!(
            resolve_slot(&roster, "7").unwrap_err().to_string(),
            "Account-7 does not exist"
        );
        assert_eq!(
            resolve_slot(&roster, "nobody").unwrap_err().to_string(),
            "No account found with identifier: nobody"
        );
    }
}
