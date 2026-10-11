//! Preconditions for a switch requested from an older account snapshot.

use serde_json::json;
use sha2::{Digest, Sha256};

use super::*;

pub struct SwitchState {
    pub revision: String,
    pub actives: ActiveSlots,
    pub unmanaged: Vec<Provider>,
}

pub(super) struct SwitchGuard<'a> {
    pub provider: Provider,
    pub revision: &'a str,
    pub acknowledge_interruption: bool,
}

impl Switcher {
    pub fn switch_state(&self) -> Result<SwitchState> {
        let _lock = self.store.lock()?;
        self.switch_state_locked(&self.roster_opt()?.unwrap_or_else(Roster::empty))
    }

    fn switch_state_locked(&self, roster: &Roster) -> Result<SwitchState> {
        let mut actives = ActiveSlots::default();
        let mut unmanaged = Vec::new();
        let codex = AuthJson::read(&self.store.paths.live_auth_file())?;
        if let Some(live) = &codex {
            if live.kind() == AuthKind::Unknown {
                return Err(CcswError::Conflict("login_unavailable"));
            }
            let slot = self.slot_of_live(roster, live);
            actives.codex = slot;
            if slot.is_none() {
                unmanaged.push(Provider::Codex);
            }
        }
        let claude = ClaudeLive::new(&self.store.paths, &SystemSecurity).read()?;
        if claude.keychain_unavailable {
            return Err(CcswError::Conflict("login_unavailable"));
        }
        if claude.credential.is_some() {
            let slot = self.claude_slot_of_live(roster, &claude);
            actives.claude = slot;
            if slot.is_none() {
                unmanaged.push(Provider::Claude);
            }
        }
        let rows: Vec<_> = roster
            .accounts
            .iter()
            .map(|(slot, record)| {
                json!([
                    slot,
                    record.provider,
                    record.email,
                    record.organization_uuid,
                    record.uuid,
                    record.added,
                    record.alias,
                    record.kind,
                    record.disabled
                ])
            })
            .collect();
        // Token rotation and usage collection must not invalidate a selection.
        let identity = json!([roster.sequence, rows, actives, unmanaged]);
        let revision = hex::encode(Sha256::digest(identity.to_string().as_bytes()));
        Ok(SwitchState {
            revision,
            actives,
            unmanaged,
        })
    }

    pub fn switch_guarded(
        &mut self,
        slot: u32,
        provider: Provider,
        revision: &str,
        acknowledge_interruption: bool,
    ) -> Result<SwitchReport> {
        let guard = SwitchGuard {
            provider,
            revision,
            acknowledge_interruption,
        };
        let roster = self.roster_opt()?.unwrap_or_else(Roster::empty);
        self.perform_switch_checked(roster, slot, "direct", false, Some(&guard))
    }

    pub(super) fn check_switch_guard(
        &self,
        roster: &Roster,
        target: u32,
        guard: &SwitchGuard<'_>,
    ) -> Result<()> {
        let state = self.switch_state_locked(roster)?;
        if state.revision != guard.revision {
            return Err(CcswError::Conflict("state_changed"));
        }
        let record = roster
            .record(target)
            .ok_or(CcswError::Conflict("state_changed"))?;
        if record.provider != guard.provider {
            return Err(CcswError::Conflict("state_changed"));
        }
        if record.disabled {
            return Err(CcswError::Conflict("account_disabled"));
        }
        if state.unmanaged.contains(&guard.provider) {
            return Err(CcswError::Conflict("unmanaged_login"));
        }
        if guard.provider == Provider::Codex && !guard.acknowledge_interruption {
            return Err(CcswError::Conflict("interruption_required"));
        }
        if guard.provider == Provider::Codex {
            self.store.paths.validate_credential_store()?;
        }
        Ok(())
    }

    pub(crate) fn check_switch_credentials(
        &self,
        target: u32,
        record: &AccountRecord,
    ) -> Result<()> {
        let raw = credentials::read(&self.store, target)?
            .ok_or(CcswError::Conflict("credentials_unavailable"))?;
        let (valid, expired, fingerprint) = match record.provider {
            Provider::Codex => {
                let fingerprint = credentials::fingerprint(&raw);
                let auth = AuthJson::from_value(raw);
                let valid = match auth.kind() {
                    AuthKind::ApiKey => record.is_api_key(),
                    AuthKind::ChatGpt => {
                        !record.is_api_key() && auth.identity().as_ref() == Some(&record.identity())
                    }
                    AuthKind::Unknown => false,
                };
                let expired = auth
                    .access_token()
                    .is_some_and(|token| crate::codex::jwt::is_expiring(token, 0) == Some(true));
                (valid, expired, fingerprint)
            }
            Provider::Claude => SlotFile::from_value(&raw).map_or((false, false, None), |file| {
                let valid = file.credential.kind() != CredentialKind::Unknown
                    && (file.credential.kind() == CredentialKind::ApiKey) == record.is_api_key()
                    && file.identity().as_ref() == Some(&record.identity());
                (
                    valid,
                    file.credential.is_expired(now_unix() * 1000),
                    file.credential.fingerprint(),
                )
            }),
        };
        let identities = BTreeMap::from([(target, record.identity())]);
        let entries = UsageStore::new(&self.store.paths).entries(&identities, now_unix() as f64);
        let dead = !record.is_api_key()
            && entries.get(&target).is_some_and(|entry| {
                entry.token_dead(
                    crate::store::usage_store::AUTH_DEAD_STRIKES,
                    fingerprint.as_deref(),
                )
            });
        if !valid {
            return Err(CcswError::Conflict("credentials_unavailable"));
        }
        if dead {
            return Err(CcswError::Conflict("relogin_required"));
        }
        if expired {
            return Err(CcswError::Conflict("token_expired"));
        }
        Ok(())
    }
}
