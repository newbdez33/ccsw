//! id_token claim extraction, plan labels, expiry.
//!
//! Signatures are never verified: the claims are a routing hint (who the login
//! belongs to), and the usage API is what authenticates the credential.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

use super::auth::AuthJson;

const AUTH_CLAIM: &str = "https://api.openai.com/auth";
const PROFILE_CLAIM: &str = "https://api.openai.com/profile";

/// One entry of the `organizations[]` claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgInfo {
    pub id: String,
    pub title: String,
    pub role: String,
    pub is_default: bool,
}

/// What the id_token says about the login.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountInfo {
    pub email: Option<String>,
    /// Raw plan wire value (`plus`, `pro`, `team`, …).
    pub plan_type: Option<String>,
    /// `chatgpt_account_id` claim, else `tokens.account_id`.
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub is_fedramp: bool,
    /// Title of the organization whose id is `account_id`.
    pub workspace_name: Option<String>,
    pub organizations: Vec<OrgInfo>,
}

impl AccountInfo {
    pub fn from_auth(auth: &AuthJson) -> Self {
        Self::from_value(&auth.0)
    }

    /// Claims from `tokens.id_token`; every field is `None` when the token is
    /// missing or not a JWT.
    pub fn from_value(auth: &Value) -> Self {
        let id_token = auth
            .pointer("/tokens/id_token")
            .and_then(Value::as_str)
            .unwrap_or("");
        let account_id_from_tokens = auth
            .pointer("/tokens/account_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let claims = decode_payload(id_token).unwrap_or_default();

        // Root claim first, then the profile claim, as Codex 0.144.1 does.
        let email = claims
            .get("email")
            .or_else(|| claims.get(PROFILE_CLAIM).and_then(|p| p.get("email")))
            .and_then(Value::as_str)
            .map(str::to_string);

        let auth_claims = claims.get(AUTH_CLAIM);
        let string_claim = |key: &str| {
            auth_claims
                .and_then(|a| a.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let plan_type = string_claim("chatgpt_plan_type");
        let user_id = string_claim("chatgpt_user_id").or_else(|| string_claim("user_id"));
        let account_id = string_claim("chatgpt_account_id")
            .filter(|s| !s.trim().is_empty())
            .or(account_id_from_tokens);
        let is_fedramp = auth_claims
            .and_then(|a| a.get("chatgpt_account_is_fedramp"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let organizations = extract_organizations(auth_claims);
        let workspace_name = account_id.as_deref().and_then(|account_id| {
            organizations
                .iter()
                .find(|org| org.id == account_id && !org.title.is_empty())
                .map(|org| org.title.clone())
        });

        Self {
            email,
            plan_type,
            account_id,
            user_id,
            is_fedramp,
            workspace_name,
            organizations,
        }
    }

    /// Human plan label (`Plus`, `Pro 20×`, …); `None` without a plan claim.
    pub fn plan_label(&self) -> Option<String> {
        crate::model::plan_label(self.plan_type.as_deref())
    }
}

fn extract_organizations(auth_claims: Option<&Value>) -> Vec<OrgInfo> {
    let Some(orgs) = auth_claims
        .and_then(|a| a.get("organizations"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let text = |org: &Value, key: &str| {
        org.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    orgs.iter()
        .filter_map(|org| {
            let id = text(org, "id");
            if id.is_empty() {
                return None;
            }
            Some(OrgInfo {
                id,
                title: text(org, "title"),
                role: text(org, "role"),
                is_default: org
                    .get("is_default")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// The JWT payload segment as JSON (base64url, no padding); `None` when the
/// token is not a JWT.
pub fn decode_payload(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&decoded).ok()
}

/// The `exp` claim (unix seconds).
pub fn token_expires_at(token: &str) -> Option<i64> {
    decode_payload(token)?.get("exp")?.as_i64()
}

/// `Some(true)` when the token is expired or expires within `margin_secs`,
/// `Some(false)` when it is still valid, `None` without an `exp` claim.
pub fn is_expiring(token: &str, margin_secs: i64) -> Option<bool> {
    let exp = token_expires_at(token)?;
    Some(crate::model::now_unix() + margin_secs >= exp)
}

#[cfg(test)]
pub(crate) fn make_jwt(claims: &Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    format!("header.{payload}.signature")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn auth_with(claims: Value, tokens_account_id: Option<&str>) -> Value {
        let mut tokens = json!({ "id_token": make_jwt(&claims) });
        if let Some(id) = tokens_account_id {
            tokens["account_id"] = json!(id);
        }
        json!({ "tokens": tokens })
    }

    #[test]
    fn claims_are_read_from_the_documented_paths() {
        let auth = auth_with(
            json!({
                "email": "User@Example.com",
                "https://api.openai.com/auth": {
                    "chatgpt_plan_type": "pro",
                    "chatgpt_account_id": "acct-claim",
                    "chatgpt_user_id": "user-1",
                    "chatgpt_account_is_fedramp": true,
                    "organizations": [
                        {"id": "acct-other", "title": "Other", "role": "member", "is_default": true},
                        {"id": "acct-claim", "title": "Platform Team", "role": "owner"},
                        {"id": "", "title": "dropped"}
                    ]
                }
            }),
            Some("acct-tokens"),
        );
        let info = AccountInfo::from_value(&auth);
        assert_eq!(info.email.as_deref(), Some("User@Example.com"));
        assert_eq!(info.plan_type.as_deref(), Some("pro"));
        assert_eq!(info.account_id.as_deref(), Some("acct-claim"));
        assert_eq!(info.user_id.as_deref(), Some("user-1"));
        assert!(info.is_fedramp);
        assert_eq!(info.workspace_name.as_deref(), Some("Platform Team"));
        assert_eq!(info.organizations.len(), 2);
        assert!(info.organizations[0].is_default);
        assert_eq!(info.organizations[1].role, "owner");
        assert_eq!(info.plan_label().as_deref(), Some("Pro 20×"));
    }

    #[test]
    fn fallbacks_profile_email_tokens_account_id_and_user_id() {
        let auth = auth_with(
            json!({
                "https://api.openai.com/profile": {"email": "ws@example.com"},
                "https://api.openai.com/auth": {"user_id": "legacy-user", "chatgpt_account_id": "  "}
            }),
            Some("acct-tokens"),
        );
        let info = AccountInfo::from_value(&auth);
        assert_eq!(info.email.as_deref(), Some("ws@example.com"));
        assert_eq!(info.account_id.as_deref(), Some("acct-tokens"));
        assert_eq!(info.user_id.as_deref(), Some("legacy-user"));
        assert!(!info.is_fedramp);
        assert_eq!(info.plan_label(), None);
    }

    #[test]
    fn workspace_name_ignores_the_default_org_when_ids_differ() {
        let auth = auth_with(
            json!({
                "https://api.openai.com/auth": {
                    "chatgpt_plan_type": "team",
                    "chatgpt_account_id": "acct-team",
                    "organizations": [{"id": "acct-personal", "title": "Personal", "is_default": true}]
                }
            }),
            None,
        );
        let info = AccountInfo::from_value(&auth);
        assert_eq!(info.workspace_name, None);
        assert_eq!(info.plan_label().as_deref(), Some("Team"));
    }

    #[test]
    fn missing_or_garbage_token_yields_defaults() {
        assert_eq!(
            AccountInfo::from_value(&json!({"tokens": {"id_token": ""}})),
            AccountInfo::default()
        );
        assert_eq!(
            AccountInfo::from_value(&json!({"OPENAI_API_KEY": "sk-x"})),
            AccountInfo::default()
        );
        assert_eq!(decode_payload("not-a-jwt"), None);
        assert_eq!(decode_payload("a.!!!.c"), None);
    }

    #[test]
    fn expiry_helpers() {
        let now = crate::model::now_unix();
        let soon = make_jwt(&json!({"exp": now + 30}));
        let later = make_jwt(&json!({"exp": now + 7200}));
        let no_exp = make_jwt(&json!({"sub": "x"}));
        assert_eq!(token_expires_at(&soon), Some(now + 30));
        assert_eq!(is_expiring(&soon, 60), Some(true));
        assert_eq!(is_expiring(&later, 60), Some(false));
        assert_eq!(is_expiring(&make_jwt(&json!({"exp": 0})), 0), Some(true));
        assert_eq!(is_expiring(&no_exp, 60), None);
        assert_eq!(is_expiring("nope", 60), None);
        assert_eq!(token_expires_at(&no_exp), None);
    }
}
