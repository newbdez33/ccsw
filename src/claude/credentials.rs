//! Claude Code's credential object (`{"claudeAiOauth": …}` plus machine-scoped
//! siblings, or a managed `sk-ant-api…` key), the global config's
//! `oauthAccount` (the identity), and the slot file cswitch stores for a
//! Claude account (spec §5).

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::errors::{CswitchError, Result};
use crate::model::Identity;

pub const OAUTH_KEY: &str = "claudeAiOauth";
pub const MANAGED_KEY: &str = "primaryApiKey";
pub const OAUTH_ACCOUNT_KEY: &str = "oauthAccount";
/// A token is "expiring" this long before `expiresAt` (cswap's buffer).
pub const EXPIRY_BUFFER_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    /// Access + refresh token.
    OAuth,
    /// `claude setup-token`: an access token with no refresh token, never refreshed.
    SetupToken,
    /// A managed `sk-ant-api…` key.
    ApiKey,
    Unknown,
}

/// A bare `sk-ant-api…` key; a JSON object never is.
pub fn looks_like_api_key(text: &str) -> bool {
    let text = text.trim();
    text.starts_with("sk-ant-api") && !text.starts_with('{')
}

pub fn looks_like_setup_token(text: &str) -> bool {
    text.trim().starts_with("sk-ant-oat")
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeCredential(pub Value);

impl ClaudeCredential {
    pub fn from_value(value: Value) -> Self {
        Self(value)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text)
            .map_err(|err| CswitchError::credential_read(format!("invalid JSON: {err}")))?;
        if !value.is_object() {
            return Err(CswitchError::credential_read("not a JSON object"));
        }
        Ok(Self(value))
    }

    /// The object cswap writes for a setup-token.
    pub fn wrap_setup_token(token: &str) -> Self {
        Self(json!({OAUTH_KEY: {"accessToken": token.trim(), "scopes": ["user:inference"]}}))
    }

    /// A managed API key, under the key Claude Code uses in its global config.
    pub fn managed_key(key: &str) -> Self {
        Self(json!({MANAGED_KEY: key.trim()}))
    }

    fn oauth(&self) -> Option<&Map<String, Value>> {
        self.0.get(OAUTH_KEY)?.as_object()
    }

    fn oauth_text(&self, key: &str) -> Option<&str> {
        self.oauth()?.get(key)?.as_str().filter(|s| !s.is_empty())
    }

    pub fn kind(&self) -> CredentialKind {
        if self.access_token().is_some() {
            return if self.refresh_token().is_some() {
                CredentialKind::OAuth
            } else {
                CredentialKind::SetupToken
            };
        }
        if self.api_key().is_some() {
            return CredentialKind::ApiKey;
        }
        CredentialKind::Unknown
    }

    pub fn access_token(&self) -> Option<&str> {
        self.oauth_text("accessToken")
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.oauth_text("refreshToken")
    }

    /// `expiresAt` in milliseconds (a number; a float is truncated).
    pub fn expires_at_ms(&self) -> Option<i64> {
        let value = self.oauth()?.get("expiresAt")?;
        value.as_i64().or_else(|| value.as_f64().map(|f| f as i64))
    }

    pub fn scopes(&self) -> Vec<String> {
        self.oauth()
            .and_then(|o| o.get("scopes"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn api_key(&self) -> Option<&str> {
        self.0
            .get(MANAGED_KEY)?
            .as_str()
            .filter(|k| !k.trim().is_empty())
    }

    /// Within the 5-minute buffer of `expiresAt`; never without one.
    pub fn is_expiring(&self, now_ms: i64) -> bool {
        self.expires_at_ms()
            .is_some_and(|expires_at| now_ms + EXPIRY_BUFFER_MS >= expires_at)
    }

    pub fn is_expired(&self, now_ms: i64) -> bool {
        self.expires_at_ms()
            .is_some_and(|expires_at| expires_at <= now_ms)
    }

    /// Replace this object's `claudeAiOauth` with `fresh`'s; every other
    /// top-level key (Claude Code's `mcpOAuth` tokens, …) stays as it is.
    pub fn replace_oauth_from(&mut self, fresh: &ClaudeCredential) {
        if !self.0.is_object() {
            self.0 = json!({});
        }
        let map = self.0.as_object_mut().expect("object");
        match fresh.0.get(OAUTH_KEY) {
            Some(oauth) => {
                map.insert(OAUTH_KEY.to_string(), oauth.clone());
            }
            None => {
                map.remove(OAUTH_KEY);
            }
        }
    }

    /// Just the login: `{"claudeAiOauth": …}` or `{"primaryApiKey": …}`.
    pub fn oauth_only(&self) -> ClaudeCredential {
        match self.kind() {
            CredentialKind::ApiKey => Self::managed_key(self.api_key().unwrap_or_default()),
            _ => Self(json!({OAUTH_KEY: self.0.get(OAUTH_KEY).cloned().unwrap_or(Value::Null)})),
        }
    }

    /// The freshness rule for folding a live copy back: a different refresh
    /// token with a strictly later `expiresAt` (a missing stamp is not proof);
    /// API keys are newer when the key differs.
    pub fn is_newer_than(&self, other: &ClaudeCredential) -> bool {
        if self.kind() == CredentialKind::ApiKey || other.kind() == CredentialKind::ApiKey {
            return self.kind() == CredentialKind::ApiKey && self.api_key() != other.api_key();
        }
        if self.refresh_token().is_none() || self.refresh_token() == other.refresh_token() {
            return false;
        }
        match (self.expires_at_ms(), other.expires_at_ms()) {
            (Some(mine), Some(theirs)) => mine > theirs,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// `sha256:<refresh token>` when present, else `sha256-full:<canonical JSON>`.
    pub fn fingerprint(&self) -> Option<String> {
        if self.0.is_null() {
            return None;
        }
        if let Some(token) = self.refresh_token() {
            return Some(format!(
                "sha256:{}",
                hex::encode(Sha256::digest(token.as_bytes()))
            ));
        }
        let canonical = serde_json::to_string(&self.0).ok()?;
        Some(format!(
            "sha256-full:{}",
            hex::encode(Sha256::digest(canonical.as_bytes()))
        ))
    }
}

/// The `oauthAccount` object of Claude Code's global config, kept verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct OauthAccount(pub Value);

impl OauthAccount {
    /// What token accounts get (cswap's shape).
    pub fn synthesized(email: &str) -> Self {
        Self(json!({
            "emailAddress": email, "accountUuid": "",
            "organizationUuid": null, "organizationName": null
        }))
    }

    fn text(&self, key: &str) -> String {
        self.0
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    pub fn email_address(&self) -> String {
        self.text("emailAddress")
    }

    pub fn account_uuid(&self) -> String {
        self.text("accountUuid")
    }

    pub fn organization_uuid(&self) -> String {
        self.text("organizationUuid")
    }

    pub fn organization_name(&self) -> String {
        self.text("organizationName")
    }

    /// `(email lowercased, organizationUuid)`; `None` without an email.
    pub fn identity(&self) -> Option<Identity> {
        let email = self.email_address();
        if email.is_empty() {
            return None;
        }
        Some(Identity::new(
            email.to_lowercase(),
            self.organization_uuid(),
        ))
    }
}

/// `credentials/<n>.json` for a Claude account: the login plus its identity.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotFile {
    pub credential: ClaudeCredential,
    pub oauth_account: OauthAccount,
}

impl SlotFile {
    /// Stores only the login part of `credential` (no siblings).
    pub fn new(credential: &ClaudeCredential, oauth_account: OauthAccount) -> Self {
        Self {
            credential: credential.oauth_only(),
            oauth_account,
        }
    }

    pub fn to_value(&self) -> Value {
        let mut value = self.credential.0.clone();
        if !value.is_object() {
            value = json!({});
        }
        value[OAUTH_ACCOUNT_KEY] = self.oauth_account.0.clone();
        value
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let mut map = value.as_object().cloned().ok_or_else(|| {
            CswitchError::credential_read("stored Claude credentials are not a JSON object")
        })?;
        let account = map
            .remove(OAUTH_ACCOUNT_KEY)
            .filter(Value::is_object)
            .ok_or_else(|| {
                CswitchError::credential_read("stored Claude credentials carry no oauthAccount")
            })?;
        Ok(Self {
            credential: ClaudeCredential(Value::Object(map)),
            oauth_account: OauthAccount(account),
        })
    }

    pub fn identity(&self) -> Option<Identity> {
        self.oauth_account.identity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn oauth(access: &str, refresh: Option<&str>, expires_at: Option<i64>) -> ClaudeCredential {
        let mut inner =
            json!({"accessToken": access, "scopes": ["user:inference", "user:profile"]});
        if let Some(refresh) = refresh {
            inner["refreshToken"] = json!(refresh);
        }
        if let Some(expires_at) = expires_at {
            inner["expiresAt"] = json!(expires_at);
        }
        ClaudeCredential::from_value(
            json!({OAUTH_KEY: inner, "mcpOAuth": {"srv": {"accessToken": "m"}}}),
        )
    }

    #[test]
    fn kinds_and_accessors() {
        let full = oauth("at", Some("rt"), Some(1_000));
        assert_eq!(full.kind(), CredentialKind::OAuth);
        assert_eq!(full.access_token(), Some("at"));
        assert_eq!(full.refresh_token(), Some("rt"));
        assert_eq!(full.expires_at_ms(), Some(1_000));
        assert_eq!(full.scopes(), vec!["user:inference", "user:profile"]);
        assert_eq!(full.api_key(), None);
        let setup = ClaudeCredential::wrap_setup_token("sk-ant-oat01-x");
        assert_eq!(setup.kind(), CredentialKind::SetupToken);
        assert_eq!(setup.access_token(), Some("sk-ant-oat01-x"));
        assert_eq!(setup.scopes(), vec!["user:inference"]);
        assert_eq!(setup.expires_at_ms(), None);
        let key = ClaudeCredential::managed_key("sk-ant-api03-k");
        assert_eq!(key.kind(), CredentialKind::ApiKey);
        assert_eq!(key.api_key(), Some("sk-ant-api03-k"));
        assert_eq!(
            ClaudeCredential::from_value(json!({})).kind(),
            CredentialKind::Unknown
        );
        assert_eq!(
            ClaudeCredential::from_value(json!({OAUTH_KEY: {"accessToken": ""}})).kind(),
            CredentialKind::Unknown
        );
        assert!(ClaudeCredential::parse("[1]").is_err());
        assert!(ClaudeCredential::parse("{").is_err());
        assert!(looks_like_api_key("sk-ant-api03-abc"));
        assert!(!looks_like_api_key("{\"sk-ant-api\": 1}"));
        assert!(!looks_like_api_key("sk-ant-oat01-abc"));
        assert!(looks_like_setup_token(" sk-ant-oat01-abc"));
        assert!(!looks_like_setup_token("sk-openai"));
    }

    #[test]
    fn expiry_uses_the_five_minute_buffer() {
        let cred = oauth("at", Some("rt"), Some(1_000_000));
        assert!(!cred.is_expiring(1_000_000 - EXPIRY_BUFFER_MS - 1));
        assert!(cred.is_expiring(1_000_000 - EXPIRY_BUFFER_MS));
        assert!(!cred.is_expired(999_999));
        assert!(cred.is_expired(1_000_000));
        let setup = ClaudeCredential::wrap_setup_token("t");
        assert!(
            !setup.is_expiring(i64::MAX / 2),
            "no expiresAt never expires"
        );
        assert!(!setup.is_expired(i64::MAX / 2));
    }

    #[test]
    fn replace_oauth_keeps_siblings_and_oauth_only_strips_them() {
        let mut live = oauth("old", Some("rt-old"), Some(1));
        let fresh = oauth("new", Some("rt-new"), Some(2));
        live.replace_oauth_from(&fresh);
        assert_eq!(live.access_token(), Some("new"));
        assert_eq!(live.0["mcpOAuth"]["srv"]["accessToken"], "m");
        let only = fresh.oauth_only();
        assert_eq!(only.0, json!({OAUTH_KEY: fresh.0[OAUTH_KEY]}));
        let key_only = ClaudeCredential::managed_key("k").oauth_only();
        assert_eq!(key_only.0, json!({MANAGED_KEY: "k"}));
        let mut empty = ClaudeCredential::from_value(json!({}));
        empty.replace_oauth_from(&fresh);
        assert_eq!(empty.kind(), CredentialKind::OAuth);
    }

    #[test]
    fn freshness_rule() {
        let stored = oauth("a", Some("rt-1"), Some(100));
        assert!(
            !oauth("b", Some("rt-1"), Some(200)).is_newer_than(&stored),
            "same refresh token"
        );
        assert!(oauth("b", Some("rt-2"), Some(101)).is_newer_than(&stored));
        assert!(
            !oauth("b", Some("rt-2"), Some(100)).is_newer_than(&stored),
            "equal expiry is not newer"
        );
        assert!(!oauth("b", Some("rt-2"), None).is_newer_than(&stored));
        assert!(oauth("b", Some("rt-2"), Some(1)).is_newer_than(&oauth("a", Some("rt-1"), None)));
        assert!(
            !ClaudeCredential::wrap_setup_token("t").is_newer_than(&stored),
            "no refresh token"
        );
        let key_a = ClaudeCredential::managed_key("a");
        let key_b = ClaudeCredential::managed_key("b");
        assert!(key_b.is_newer_than(&key_a));
        assert!(!key_a.is_newer_than(&key_a.clone()));
        assert!(!stored.is_newer_than(&key_a));
    }

    #[test]
    fn fingerprint_prefers_the_refresh_token() {
        use sha2::{Digest, Sha256};
        let cred = oauth("a", Some("rt-1"), None);
        assert_eq!(
            cred.fingerprint(),
            Some(format!("sha256:{}", hex::encode(Sha256::digest(b"rt-1"))))
        );
        assert!(
            ClaudeCredential::managed_key("k")
                .fingerprint()
                .unwrap()
                .starts_with("sha256-full:")
        );
        assert_eq!(
            ClaudeCredential::from_value(serde_json::Value::Null).fingerprint(),
            None
        );
    }

    #[test]
    fn oauth_account_identity_and_slot_file_round_trip() {
        let account = OauthAccount(json!({
            "accountUuid": "acc-1", "emailAddress": "Me@Example.com",
            "organizationUuid": "org-1", "organizationName": "Acme", "billingType": "stripe"
        }));
        assert_eq!(
            account.identity(),
            Some(Identity::new("me@example.com", "org-1"))
        );
        assert_eq!(account.organization_name(), "Acme");
        assert_eq!(account.account_uuid(), "acc-1");
        let synthesized = OauthAccount::synthesized("k@token.local");
        assert_eq!(
            synthesized.identity(),
            Some(Identity::new("k@token.local", ""))
        );
        assert_eq!(synthesized.organization_name(), "");
        assert_eq!(OauthAccount(json!({})).identity(), None);

        let slot = SlotFile::new(&oauth("a", Some("rt"), Some(5)), account.clone());
        let value = slot.to_value();
        assert_eq!(value[OAUTH_KEY]["refreshToken"], "rt");
        assert!(value.get("mcpOAuth").is_none(), "siblings are not stored");
        assert_eq!(
            value[OAUTH_ACCOUNT_KEY]["billingType"], "stripe",
            "oauthAccount is kept verbatim"
        );
        let back = SlotFile::from_value(&value).unwrap();
        assert_eq!(back, slot);
        assert_eq!(back.identity(), account.identity());
        assert!(
            SlotFile::from_value(&json!({OAUTH_KEY: {}})).is_err(),
            "oauthAccount is required"
        );
        let key = SlotFile::new(&ClaudeCredential::managed_key("k"), synthesized);
        assert_eq!(key.to_value()[MANAGED_KEY], "k");
    }
}
