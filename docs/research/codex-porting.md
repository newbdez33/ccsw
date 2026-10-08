# codex-switch → ccsw porting reference (Codex CLI credential / identity / usage / refresh mechanics)

Source checkout: `/Volumes/shit/orca/workspaces/codex-switch/basketstar` (MIT). All line numbers refer to that tree.
codex-switch declares itself contract-aligned with upstream Codex **0.144.1** (`ALIGNED_CODEX_VERSION`, `src/auth.rs:15`); daemon/`--no-daemon` behaviour is aligned with Codex 0.156/0.157.

---

## 0. Constants cheat-sheet

| Constant | Value | Where |
|---|---|---|
| OAuth client id | `app_EMoamEEZ73f0CkXaXp7hrann` | `src/auth.rs:13` |
| Issuer | `https://auth.openai.com` | `src/auth.rs:25` |
| Token endpoint (refresh, code exchange, api-key exchange) | `https://auth.openai.com/oauth/token` (env override `CS_TOKEN_URL`) | `src/auth.rs:26-30` |
| Authorize URL | `https://auth.openai.com/oauth/authorize?...` | `src/login.rs:271` |
| Device usercode | `https://auth.openai.com/api/accounts/deviceauth/usercode` | `src/login.rs:531` |
| Device token poll | `https://auth.openai.com/api/accounts/deviceauth/token` | `src/login.rs:532` |
| Device verification page shown to user | `https://auth.openai.com/codex/device` | `src/login.rs:533` |
| Device-flow redirect_uri used in code exchange | `https://auth.openai.com/deviceauth/callback` | `src/login.rs:878` |
| Usage API | `GET https://chatgpt.com/backend-api/wham/usage` (env override `CS_USAGE_URL`) | `src/usage/api.rs:224` |
| Workspace name lookup | `GET https://chatgpt.com/backend-api/wham/accounts/check` | `src/workspace.rs:10` |
| Reset credits list | `GET https://chatgpt.com/backend-api/wham/rate-limit-reset-credits` | `src/usage/reset_credits.rs:16` |
| Reset credits consume | `POST https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume` | `src/usage/reset_credits.rs:17` |
| Warmup responses | `POST https://chatgpt.com/backend-api/codex/responses` | `src/warmup.rs:11` |
| Models list | `GET https://chatgpt.com/backend-api/codex/models` | `src/warmup.rs:12` |
| OAuth scope | `openid profile email offline_access api.connectors.read api.connectors.invoke` | `src/login.rs:23` |
| Originator | `codex_cli_rs` | `src/login.rs:22` |
| User-Agent | `codex_cli_rs/0.144.1 (<os>; <arch>)` e.g. `codex_cli_rs/0.144.1 (macos; aarch64)` | `src/auth.rs:18-24` |
| Callback ports | `127.0.0.1:1455`, fallback `127.0.0.1:1457`; redirect_uri host must be `localhost` | `src/login.rs:27-31` |
| Callback path | `/auth/callback` | `src/login.rs:120` |
| Browser callback timeout | 600 s; device flow timeout 900 s; device poll interval 5 s | `src/login.rs:24,534,551` |
| Proactive refresh margin | 1800 s (30 min) | `src/usage/api.rs:975` |
| Opportunistic refresh limit / concurrency / start budget | 3 tokens / 2 in flight / 8 s | `src/usage/api.rs:973-982` |
| Usage retry | `MAX_RETRIES = 3`, `RETRY_DELAY = 1s` | `src/usage/mod.rs:224-225` |
| 429 local backoff | base 30 s, ×2^n (n ≤ 4), cap 300 s, +0..20 % jitter | `src/http_retry.rs:7-8,98-108,142-149` |
| HTTP client timeouts | connect 30 s, total 60 s | `src/auth.rs:678-679` |
| Live auth backups retained | `MAX_BACKUPS = 3` | `src/auth.rs:11` |
| Lock wait | 15 s, poll 200 ms (auth/launch locks); cache lock 15 s, poll 50 ms | `src/profile.rs:102-103`, `src/cache.rs:22-23` |
| Usage cache TTL | 300 s default (`[cache] ttl`) | `src/config.rs:62-71` |
| Launch restore delay | 3 s default (`[launch] restore_delay_secs`, 0 is normalized to 3) | `src/config.rs:119-134` |
| Free plan floor | `FREE_FLOOR_PCT = 35.0` (5h remaining %) | `src/usage/mod.rs:219` |
| 7d safety margin / team priority | `safety_margin_7d = 20.0`, `team_priority = true` | `src/config.rs:101-117` |
| Windows | `WINDOW_5H_SECS = 18000`, `WINDOW_7D_SECS = 604800` | `src/usage/mod.rs:215-216` |
| Alias rules | ≤64 chars, `[A-Za-z0-9_.-]`, not `.`/`..` | `src/profile.rs:17-40` |

---

## 1. `auth.json`

### 1.1 Location and `CODEX_HOME` resolution (`src/auth.rs:41-79`)

```rust
/// User Codex home (`$CODEX_HOME`, or `~/.codex`).
pub(crate) fn user_codex_home() -> Result<PathBuf> {
    codex_home_from_values(std::env::var_os("CODEX_HOME"), dirs::home_dir())
}

/// ~/.codex/auth.json (or $CODEX_HOME/auth.json)
pub fn codex_auth_path() -> Result<PathBuf> {
    let codex_home = user_codex_home()?;
    validate_cli_auth_credentials_store(&codex_home)?;
    Ok(codex_home.join("auth.json"))
}

fn codex_home_from_values(configured_home: Option<OsString>, user_home: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(home) = configured_home.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(&home);
        if path.components().any(|component| matches!(component, std::path::Component::ParentDir)) {
            anyhow::bail!("CODEX_HOME contains '..' component which is not allowed: {}", path.display());
        }
        return Ok(path);
    }
    let home = user_home.ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    Ok(home.join(".codex"))
}
```

Rules: empty `CODEX_HOME` → default `~/.codex`; a `CODEX_HOME` containing a `..` component is rejected. Every call to `codex_auth_path()` re-validates the credential store setting (below).

### 1.2 ChatGPT OAuth login shape (what codex-switch writes; `src/login.rs:905-927`)

```rust
pub fn build_auth_json(tokens: &LoginTokens, account_id: &str) -> serde_json::Value {
    use crate::output::format_iso8601;
    let ts = crate::auth::now_unix_secs();
    // Same shape Codex 0.144.1 writes on a ChatGPT login: auth_mode is
    // persisted and an unknown account_id is null rather than "".
    let account_id_value = if account_id.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(account_id.to_string())
    };
    serde_json::json!({
        "OPENAI_API_KEY": tokens.api_key,
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": tokens.id_token,
            "access_token": tokens.access_token,
            "refresh_token": tokens.refresh_token,
            "account_id": account_id_value
        },
        "last_refresh": format_iso8601(ts)
    })
}
```

Concrete example (pretty-printed with `serde_json::to_string_pretty`, which is how `write_auth` serializes):

```json
{
  "OPENAI_API_KEY": null,
  "auth_mode": "chatgpt",
  "tokens": {
    "id_token": "eyJhbGciOi...<JWT>",
    "access_token": "eyJhbGciOi...<JWT>",
    "refresh_token": "rt_...opaque...",
    "account_id": "3f1c4b2e-...-workspace-or-account-uuid"
  },
  "last_refresh": "2026-09-29T08:15:42Z"
}
```

Field notes:
- `OPENAI_API_KEY`: string or `null`. On browser login codex-switch does the same post-login token exchange as Codex to obtain an API key (best-effort; failure leaves `null`, which Codex accepts). Device-code login leaves it `null`.
- `auth_mode`: `"chatgpt"` for ChatGPT OAuth. (codex-switch never writes an API-key-mode file; see 1.3.)
- `tokens.account_id`: the `chatgpt_account_id` claim from the id_token, or `null` if unknown. `parse_account_info` uses it only as a **fallback** when the JWT claim is missing (`src/jwt.rs:107-145`).
- `last_refresh`: RFC3339/ISO-8601 UTC seconds, format `%Y-%m-%dT%H:%M:%SZ` (`src/output.rs:263-267`):

```rust
pub fn format_iso8601(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}
```

  Parsed back with `chrono::DateTime::parse_from_rfc3339` (`src/profile.rs:689-692`, `src/launch.rs:1252-1254`), so fractional seconds / offsets written by Codex itself also parse. Comment at `src/auth.rs:570-571`: *"Codex refreshes proactively when last_refresh is older than 8 days; stamping it here keeps our refreshes recognized (matches upstream)."*

Minimal shape accepted by `validate_auth_value` (`src/auth.rs:619-667`): a `tokens` object with a non-empty `id_token` whose payload is base64url JSON, plus at least one of `access_token` / `refresh_token`, and an `account_id` derivable from the id_token claims (or `tokens.account_id`). Test fixtures use `{"OPENAI_API_KEY": null, "tokens": {...}}` with no `auth_mode` / `last_refresh` and everything still works (`tests/test_refresh_persistence.rs:344-359`).

### 1.3 API-key logins

codex-switch **does not support** API-key credentials at all: it requires ChatGPT OAuth. The only API-key-related handling is
- the `OPENAI_API_KEY` root field carried through from the post-login exchange (1.2), and
- the managed policy check `forced_login_method = "api"` in `$CODEX_HOME/config.toml`, which makes codex-switch refuse to run:

```rust
if config.get("forced_login_method").and_then(|v| v.as_str()) == Some("api") {
    anyhow::bail!("Codex managed policy requires API key login, but codex-switch requires ChatGPT OAuth");
}
```

So the API-key file shape (Codex writes `{"OPENAI_API_KEY": "sk-...", "auth_mode": "apikey", "tokens": null, ...}` or similar) is **not** modelled here; if ccsw needs it, take it from upstream Codex, not from codex-switch. Everything below assumes `tokens.id_token` exists.

### 1.4 `cli_auth_credentials_store = "file"` requirement (`src/auth.rs:81-102`)

```rust
fn validate_cli_auth_credentials_store(codex_home: &Path) -> Result<()> {
    let Some((config_path, config)) = load_codex_config(codex_home)? else {
        return Ok(());                       // no config.toml → OK (defaults to file)
    };
    match config.get("cli_auth_credentials_store") {
        None => {}                           // missing key → file store assumed
        Some(toml::Value::String(mode)) if mode == "file" => {}
        Some(_) => anyhow::bail!(
            "codex-switch requires file-based Codex credentials; set \
             cli_auth_credentials_store = \"file\" in {}",
            config_path.display()
        ),
    }
    if config.get("forced_login_method").and_then(|v| v.as_str()) == Some("api") {
        anyhow::bail!("Codex managed policy requires API key login, but codex-switch requires ChatGPT OAuth");
    }
    Ok(())
}
```

- Reads `$CODEX_HOME/config.toml` as generic TOML (`toml::from_str::<toml::Value>`); a parse error is fatal (`load_codex_config`, `src/auth.rs:104-114`).
- Explicit `keyring`, `auto`, `ephemeral` are rejected. Rationale (docs/wiki/Configuration.md §"Why only the file store is supported"): locking/atomic replace/backup rotation only exist for files; keyring entry layout is undocumented and changed in June 2026 (Windows encrypted sidecar); `ephemeral` persists nothing.
- The check runs on every `codex_auth_path()` call and explicitly before login via `ensure_file_credentials_store()` (`src/login.rs:127,650`).

### 1.5 Managed-workspace policy checks (`src/auth.rs:116-199`)

Codex managed config keys honoured: `forced_login_method` and `forced_chatgpt_workspace_id` (string or array of strings; entries trimmed, empties dropped).

```rust
fn validate_managed_auth_config(config: &toml::Value, account_id: Option<&str>) -> Result<()> {
    if config.get("forced_login_method").and_then(|v| v.as_str()) == Some("api") {
        anyhow::bail!("Codex managed policy requires API key login, but codex-switch requires ChatGPT OAuth");
    }
    let workspace_ids = forced_chatgpt_workspace_ids(config)?;
    if workspace_ids.is_empty() { return Ok(()); }
    let account_id = account_id.ok_or_else(|| {
        anyhow::anyhow!("login token has no workspace id required by Codex managed policy")
    })?;
    if !workspace_ids.iter().any(|id| id == account_id) {
        anyhow::bail!("workspace {account_id} is not allowed by Codex forced_chatgpt_workspace_id policy");
    }
    Ok(())
}
```

Entry points:
- `validate_managed_chatgpt_account(id_token)` — right after a login token exchange (`src/login.rs:174,880`).
- `validate_managed_auth_value(&auth_json)` — at **every credential-write boundary**: `update_tokens`, `switch_live_auth_locked`, `stage_profile_auth`, `write_profile_credentials`, `save_auth_value`, `replace_profile_auth_and_live_if_current`, import save, and `preserve_refreshed_launch_auth`. Doc comment: *"JWT claims are only a routing hint until a caller has otherwise authenticated the credentials."*
- `configured_forced_workspace_ids()` — best-effort read used to add `allowed_workspace_id=<ids joined by ,>` to the authorize URL (`src/login.rs:259-263`).

### 1.6 Atomic private write (`src/auth.rs:234-270`)

```rust
pub(crate) fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    #[cfg(windows)] harden_windows_acl(parent, true)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(windows)] harden_windows_acl(tmp.path(), false)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.write_all(contents)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|err| err.error)?;   // rename over target
    #[cfg(windows)] harden_windows_acl(path, false)?;
    Ok(())
}

pub fn write_auth(path: &Path, val: &serde_json::Value) -> Result<()> {
    let raw = serde_json::to_string_pretty(val)?;
    atomic_write_private(path, raw.as_bytes())
}
```

Notes:
- Parent dir forced to `0700`, file to `0600`, temp file created in the same directory, fsync, then rename. Used for auth.json, profile auth.json, backups, `current` marker, cache.json, config.toml, provider run config.
- Windows: replaces the DACL with an exact protected ACL (`D:P(A;[OICI];FA;;;<user SID>)(A;...;FA;;;S-1-5-18)(A;...;FA;;;S-1-5-32-544)`), i.e. current user + SYSTEM + Administrators only (`src/auth.rs:272-281`).
- `read_auth` returns `CsError::NoAuthFile(path)` when missing, otherwise `serde_json::Value` (`src/auth.rs:223-232`).
- `sha256_file(path) -> Option<String>` (hex) is used for byte-identity comparisons (`src/auth.rs:493-497`). Because `write_auth` always pretty-prints with serde's key order, `cmd_save` and `update_profile_from_live` first **re-write the live file in canonical form** so its SHA matches the profile copy (`src/profile.rs:931-934, 862-865`).

### 1.7 Backup rotation of the live `auth.json` (`src/auth.rs:499-541, 777-811`)

```rust
const MAX_BACKUPS: usize = 3;

pub fn backup_auth(path: &Path) -> Result<()> {
    if !path.exists() { return Ok(()); }
    let contents = std::fs::read(path)?;
    let bak = allocate_backup_path(path)?;
    atomic_write_private(&bak, &contents)?;
    cleanup_old_backups(path);
    Ok(())
}

fn allocate_backup_path(path: &Path) -> Result<PathBuf> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    for collision in 0..1000u16 {
        let candidate = if collision == 0 {
            path.with_extension(format!("json.bak.{nanos}"))
        } else {
            path.with_extension(format!("json.bak.{nanos}-{collision}"))
        };
        if !candidate.exists() { return Ok(candidate); }
    }
    anyhow::bail!("could not allocate a unique backup path for {}", path.display())
}

fn cleanup_old_backups(path: &Path) {
    // prefix = "<file_name>.bak." e.g. "auth.json.bak."
    // collect siblings starting with prefix, sort lexicographically, delete all but the newest MAX_BACKUPS
}
```

- Backup file name: `auth.json.bak.<unix_nanos>` (e.g. `auth.json.bak.1790000000123456789`), `-N` suffix on collision. Nanoseconds (not seconds) so two switches within one second keep both recovery points. Legacy 10-digit seconds names still sort correctly against 19-digit nanosecond names.
- Retention: newest 3 by lexicographic name; extra ones are deleted best-effort.
- Backups are mode 0600 (via `atomic_write_private`).
- `backup_auth(live)` is called before every live replacement in `use`/switch (`switch_live_auth_locked`), re-login replace, and `save_auth_value` when updating an existing profile. `launch` uses a **different**, per-invocation backup name: `auth.json.bak.<pid>.<unix_secs>` (`src/launch.rs:189-193`) which is removed after restore.

---

## 2. Identity extraction from the id_token JWT (`src/jwt.rs`)

### 2.1 Structs

```rust
pub struct OrgInfo { pub id: String, pub title: String, pub role: String, pub is_default: bool }

pub struct AccountInfo {
    pub email: Option<String>,
    pub plan_type: Option<String>,      // raw wire value, e.g. "plus"
    pub account_id: Option<String>,     // chatgpt_account_id claim, else tokens.account_id
    pub is_fedramp: bool,
    pub user_id: Option<String>,
    pub workspace_name: Option<String>, // org title whose id == account_id (or cache)
    pub organizations: Vec<OrgInfo>,
}
```

### 2.2 Claim paths (`src/jwt.rs:100-170`)

```rust
pub fn parse_account_info(auth: &Value) -> AccountInfo {
    let id_token = auth.pointer("/tokens/id_token").and_then(|v| v.as_str()).unwrap_or("");
    let account_id_from_tokens = auth.pointer("/tokens/account_id").and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty()).map(|s| s.to_string());

    let claims = decode_jwt_payload(id_token).unwrap_or_default();   // base64url-no-pad decode of segment [1], JSON

    // Root claim first, then the profile claim — matches Codex 0.144.1,
    // which falls back to https://api.openai.com/profile.email.
    let email = claims.get("email").and_then(|v| v.as_str())
        .or_else(|| claims.get("https://api.openai.com/profile").and_then(|p| p.get("email")).and_then(|v| v.as_str()))
        .map(|s| s.to_string());

    let auth_claims = claims.get("https://api.openai.com/auth");
    let plan_type  = auth_claims.and_then(|a| a.get("chatgpt_plan_type")).and_then(|v| v.as_str()).map(String::from);
    let user_id    = auth_claims.and_then(|a| a.get("chatgpt_user_id").or_else(|| a.get("user_id"))).and_then(|v| v.as_str()).map(String::from);
    let account_id = auth_claims.and_then(|a| a.get("chatgpt_account_id")).and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty()).map(String::from)
        .or(account_id_from_tokens);
    let is_fedramp = auth_claims.and_then(|a| a.get("chatgpt_account_is_fedramp")).and_then(|v| v.as_bool()).unwrap_or(false);

    let organizations = extract_organizations(&claims);   // auth_claims.organizations[] {id,title,role,is_default}
    let workspace_name = organizations.iter()
        .find(|o| Some(o.id.as_str()) == account_id.as_deref() && !o.title.is_empty())
        .map(|o| o.title.clone());
    AccountInfo { email, plan_type, account_id, is_fedramp, user_id, workspace_name, organizations }
}
```

Claim map summary:

| Claim path | Field |
|---|---|
| `email` (root) → fallback `https://api.openai.com/profile.email` | `email` |
| `https://api.openai.com/auth.chatgpt_account_id` → fallback `tokens.account_id` | `account_id` |
| `https://api.openai.com/auth.chatgpt_plan_type` | `plan_type` |
| `https://api.openai.com/auth.chatgpt_user_id` → fallback `.user_id` | `user_id` |
| `https://api.openai.com/auth.chatgpt_account_is_fedramp` (bool) | `is_fedramp` → adds header `X-OpenAI-Fedramp: true` |
| `https://api.openai.com/auth.organizations[] {id,title,role,is_default}` | `organizations`; `workspace_name` = title of the org whose `id == account_id` (NOT the default org) |
| `exp` | expiry (see 2.4) |

JWT signature is **never verified** (decode only). Note `id_token_add_organizations=true` is passed on the authorize URL so the organizations claim is populated.

### 2.3 Plan types and labels (`src/jwt.rs:27-98`)

```rust
pub enum PlanKind { Free, Go, Plus, ProLite, Pro, Team, Business, Enterprise, Edu, Unknown }

impl PlanKind {
    pub fn from_wire(plan_type: Option<&str>) -> Self {
        match plan_type {
            Some("free") => Self::Free,
            Some("go") => Self::Go,
            Some("plus") => Self::Plus,
            Some("prolite") => Self::ProLite,
            Some("pro") => Self::Pro,
            Some("team") => Self::Team,
            Some("self_serve_business_usage_based" | "business") => Self::Business,
            Some("enterprise_cbp_usage_based" | "enterprise") => Self::Enterprise,
            Some("education" | "edu") => Self::Edu,
            _ => Self::Unknown,
        }
    }
    fn display_name(self, raw: Option<&str>) -> String {
        match self {
            Self::Free => "Free", Self::Go => "Go", Self::Plus => "Plus",
            Self::ProLite => "Pro 5×", Self::Pro => "Pro 20×",
            Self::Team => "Team", Self::Business => "Business",
            Self::Enterprise => "Enterprise", Self::Edu => "Edu",
            Self::Unknown => raw.unwrap_or("?"),
        }.to_string()
    }
}
impl AccountInfo {
    pub fn plan_label(&self) -> String { self.plan_label_with(self.plan_type.as_deref()) }
    /// label = display_name, plus " - <workspace_name>" when a workspace name is known
    pub fn plan_label_with(&self, plan_type: Option<&str>) -> String { ... }
    pub fn is_free(&self) -> bool { matches!(self.plan_type.as_deref(), Some("free") | None) }
    pub fn is_team(&self) -> bool {
        matches!(self.plan_type.as_deref(), Some("team")) || !self.organizations.is_empty() || self.workspace_name.is_some()
    }
}
```

The usage API's top-level `plan_type` is **authoritative over the JWT** when present (handles downgrades); scoring uses `usage.plan_type` first, then `AccountInfo::is_team/is_free` (`src/usage/scoring.rs:348-354`).

### 2.4 Token expiry (`src/jwt.rs:218-231`)

```rust
pub fn token_expires_at(token: &str) -> Option<i64> { decode_jwt_payload(token)?.get("exp")?.as_i64() }

/// true if expired/expiring within margin, false if valid, None if no exp claim / not a JWT
pub fn is_token_expiring(token: &str, margin_secs: i64) -> Option<bool> {
    let exp = decode_jwt_payload(token)?.get("exp")?.as_i64()?;
    Some(crate::auth::now_unix_secs() + margin_secs >= exp)
}
```

Both the **access_token and the id_token** are JWTs and both are checked; refresh is triggered when either is within the margin (`token_needs_refresh`, `src/usage/api.rs:218-222`) so identity metadata does not go stale while the access token still works.

### 2.5 Identity for dedupe (`src/profile.rs:409-545`)

```rust
pub struct AccountIdentity { pub account_id: Option<String>, pub email: Option<String> }   // email lowercased

pub fn extract_identity(auth: &serde_json::Value) -> AccountIdentity {
    let info = parse_account_info(auth);
    AccountIdentity { account_id: info.account_id, email: info.email.map(|e| e.to_lowercase()) }
}
```

Match strengths:
- **exact**: `account_id` AND `email` both present and equal → unambiguous (`find_profile_by_identity_exact`).
- **email-only**: emails equal while one side lacks `account_id` → possibly several (Team workspaces share an email).
- `find_matching_profile(path)` = SHA-256 byte identity of the file.
- `ensure_same_account_identity(existing, incoming)`: emails must both be present and equal; account_ids must be equal when both known (either missing → OK).
- Rule: a Team workspace `account_id` alone can belong to several users, so it never authorises overwriting an existing profile; imports are create-only.

---

## 3. Profile store, locks, current marker, switch/save/import (`src/profile.rs`)

### 3.1 Layout (`CODEX_SWITCH_HOME`, default `~/.codex-switch`, empty env ignored)

| Path | Purpose |
|---|---|
| `profiles/<alias>/auth.json` | saved credentials (dir 0700, file 0600) |
| `current` | current alias marker (plain text alias, atomic private write, read trimmed) |
| `deleted-profiles/<alias>.backup-<unix_nanos>/` | `delete` renames the profile dir here (active profile cannot be deleted) |
| `recovery/rotated-import-<nanos>[-N].json` | rotated-but-unverifiable import credentials quarantine |
| `cache.json`, `cache.lock` | usage cache + its cross-process lock |
| `auth.lock`, `launch.lock`, `codex-config.lock` | cross-process locks |
| `profiles/<alias>/reset-card-consume.lock` | per-profile, zero-wait lock |
| `provider-runs/<identity_id>/<pid>-<nanos>-<seq>/` | per-launch isolated Codex homes (providers) |
| `config.toml`, `logs/` | app config, daily logs |

### 3.2 Locks (`src/profile.rs:100-269`)

- `fs4::FileExt::try_lock` polling loop; on timeout the error reports the holder written in the lock file (`"<pid> <epoch_secs>\n"`, best-effort). The lock inode is never unlinked/replaced.
- Order for every writer: **launch lock first, then auth lock** (`lock_auth_transaction`). `launch` holds the launch lock across stage→spawn→wait→restore but takes the auth lock only for each write; so `use` during a launch window blocks on the launch lock instead of racing the staged file.

```rust
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(200);
fn lock_auth_transaction_after_launch(after_launch: impl FnOnce()) -> Result<AuthTransaction> {
    let launch = lock_launch_session()?;    // $CODEX_SWITCH_HOME/launch.lock
    after_launch();
    let auth = lock_live_auth()?;           // $CODEX_SWITCH_HOME/auth.lock
    Ok(AuthTransaction { _launch: launch, _auth: auth })
}
```

### 3.3 Switch (`use <alias>`) (`src/profile.rs:271-297, 1003-1071`)

```rust
fn switch_live_auth_locked(alias: &str, src: &Path) -> Result<()> {
    let val = read_auth(src)?;
    crate::auth::validate_managed_auth_value(&val)?;
    let dst = codex_auth_path()?;
    backup_auth(&dst)?;          // auth.json.bak.<nanos>, keep 3
    write_auth(&dst, &val)?;     // atomic 0600
    write_current(alias)?;       // $CODEX_SWITCH_HOME/current
    Ok(())
}
```

- `cmd_use` first checks whether the live file is tracked (`find_matching_profile(dst)`); if untracked and interactive, prompts `Current auth.json does not belong to any saved profile -- switching will overwrite it. Continue? [y/N]`; non-interactive → error.
- `switch_profile_if_current(expected, alias)` is the CAS variant used by auto-select (only switches if `current` still equals `expected`).
- `stage_profile_auth(alias)` writes the profile to the live path **without** touching `current` (launch path; caller holds the auth lock).
- After a switch, callers call `cache::set_last_used(alias)` and then the daemon restart (section 8).

### 3.4 Save (`save [alias]`) and login write (`src/profile.rs:781-981, 1288-1345`)

`write_profile_credentials` is *the one door* into an existing profile: managed-policy check, identity check, then the **freshness guard**:

```rust
fn ensure_live_not_older(alias: &str, profile: &Value, incoming: &Value) -> Result<()> {
    if refresh_token(profile) == refresh_token(incoming) { return Ok(()); }           // same rt → fine
    if let (Some(incoming_ts), Some(profile_ts)) = (parse_last_refresh(incoming), parse_last_refresh(profile))
        && incoming_ts > profile_ts { return Ok(()); }                                // strictly newer stamp → fine
    Err(StaleLiveAuth { .. }.into())   // equal / missing / malformed stamps are a conflict, not a default
}
```

Rationale in comments: refresh_token is single-use; of two different tokens exactly one is alive; `last_refresh` is only weak evidence (wall clock, second resolution).

Alias derivation: `alias_from_email` = local part of the email with non `[A-Za-z0-9_.-]` chars → `_`, trimmed of `_`, ≤64; fallback `"account"`; collisions get `_2`, `_3`, … (`make_unique_alias`).

`save_auth_value(val, hint_alias)` (used by `login`): resolves an existing target (named alias wins; otherwise exact identity, then unique email-only; ambiguous email with no alias → error), then writes profile **and** live auth (backing up the live file first), and sets `current`. Freshness guard is deliberately skipped for freshly minted tokens; identity is still checked.

`replace_profile_auth_and_live_if_current(alias, val)` (used by `login <existing alias>` re-auth): identity check, write profile, and if `current == alias` also back up + overwrite live.

### 3.5 Compare-and-swap token update after a refresh (`src/profile.rs:299-345`)

```rust
/// Compare-and-swap a refresh rotation while holding the auth transaction.
/// A concurrent re-login supersedes the presented token and must win.
pub fn update_profile_tokens_if_refresh_matches(alias, presented_refresh_token, id_token, access_token, new_refresh_token) -> Result<bool> {
    validate_alias(alias)?;
    let profile_path = profile_auth_path(alias)?;
    let _transaction = lock_auth_transaction()?;               // launch lock + auth lock
    let profile = read_auth(&profile_path)?;
    if refresh_token(&profile) != Some(presented_refresh_token) { return Ok(false); }   // someone else rotated → do nothing
    let mut updated = profile;
    crate::auth::apply_tokens(&mut updated, id_token, access_token, new_refresh_token)?;
    crate::auth::validate_managed_auth_value(&updated)?;
    crate::auth::update_tokens(&profile_path, id_token, access_token, new_refresh_token)?;
    if read_current() == alias {
        let live = codex_auth_path()?;
        let live_auth = read_auth(&live)?;
        if refresh_token(&live_auth) == Some(presented_refresh_token) {           // CAS on the live copy too
            crate::auth::update_tokens(&live, id_token, access_token, new_refresh_token)?;
        }
    }
    Ok(true)
}
```

`apply_tokens` / `update_tokens` (`src/auth.rs:543-579`) replace `tokens.id_token/access_token/refresh_token` in place (all other keys preserved) and stamp root `last_refresh` with `format_iso8601(now)`.

### 3.6 Import validation (`src/profile.rs:1144-1256`, `src/usage/api.rs:801-911`)

Flow for `import <file|dir>` (recursively collects `*.json`):
1. `validate_auth_value` structural check (1.2).
2. `existing_import_target(source, val)`: skip if byte-identical to a stored profile **or** exact `account_id`+`email` match (prevents two copies of one account racing on the single-use refresh token).
3. `validate_import_auth(&mut val)`: if `access_token` present → call the usage API (with refresh on 401/403); if only `refresh_token` → refresh first, then usage API. Any rotation is written into `val` (`adopt_refreshed_tokens`) **even when the result is an error**, and returned as `ImportValidation { refreshed, validated_account_id, result }`.
4. Also fetches workspace name (`workspace::refresh_for_auth`) best-effort.
5. `save_imported_auth_value(val, hint_alias, validated_account_id, suggested)`: `account_id` in the JWT must equal the one the usage API accepted; managed policy; **create-only** (`create_import_profile` → unique alias, never overwrite).
6. If validation failed after a rotation: `save_recovered_import_auth_value` tries to create a profile; if that fails the rotated credentials are quarantined to `recovery/rotated-import-<nanos>.json` so the only live token is not lost.

### 3.7 Startup auth-change detection (`src/profile.rs:622-685, 870-904`)

`detect_auth_change()`: live file SHA matches a profile → `NoChange`; exact identity match with different bytes → `TokensUpdated{alias}`; single email-only match → `TokensUpdated`; multiple email-only matches → warns and `NoChange`; no match → `NewAccount`. `auto_track_current()` auto-saves an untracked live account (`cmd_save(None)`), or just re-syncs `current` on an exact match (`sync_current_from_live`).

---

## 4. Usage API (`src/usage/api.rs`, `src/usage/parse.rs`, `src/usage/mod.rs`)

### 4.1 Request

- `GET https://chatgpt.com/backend-api/wham/usage`
- Headers:
  - `Authorization: Bearer <access_token>`
  - `ChatGPT-Account-ID: <account_id>` (only when non-empty)
  - `X-OpenAI-Fedramp: true` (only when the JWT claim `chatgpt_account_is_fedramp` is true)
  - `User-Agent: codex_cli_rs/0.144.1 (<os>; <arch>)` (client default)
  - no `OpenAI-Beta`/`Originator` on this endpoint (those are on the reset-credits endpoints: `OpenAI-Beta: codex-1`, `Originator: Codex Desktop`, and header spelled `Chatgpt-Account-Id`)

```rust
pub(crate) fn apply_account_routing_headers(mut builder: reqwest::RequestBuilder, account_id: Option<&str>, is_fedramp: bool) -> reqwest::RequestBuilder {
    if let Some(account_id) = account_id.filter(|value| !value.trim().is_empty()) {
        builder = builder.header("ChatGPT-Account-ID", account_id);
    }
    if is_fedramp { builder = builder.header("X-OpenAI-Fedramp", "true"); }
    builder
}
```

### 4.2 HTTP client / proxy / CA (`src/auth.rs:669-723`, `src/config.rs:269-296`)

- `reqwest::Client` with UA above, connect timeout 30 s, total timeout 60 s.
- Proxy precedence: `--proxy` CLI > `CS_PROXY` env > `[proxy] url` in `$CODEX_SWITCH_HOME/config.toml` > reqwest's own `HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY` env. `reqwest::Proxy::all(url)`, plus `[proxy] no_proxy` list. Schemes: http, https, socks4, socks5, socks5h. Userinfo is masked in logs (`***:***@`).
- Custom CA: `CODEX_CA_CERTIFICATE` then `SSL_CERT_FILE` (PEM bundle, all certs added as roots). rustls `UnknownIssuer` errors get a hint naming `CODEX_CA_CERTIFICATE`.
- `format_reqwest_error(context, err)` prints the full source chain.

### 4.3 Retry / backoff (`src/http_retry.rs`, `src/usage/api.rs:387-584`)

`http_retry::send(request, ReplaySafety)`: up to 3 attempts, but only replays automatically on **429** and only when `ReplaySafety::Idempotent`. Usage GETs use `DeferredGet` (returned to caller with a `retry_after` hint; caller records a per-account cooldown); token/consume POSTs use `UnsafePost` (never replayed). 429 delay decision:

```rust
// 1. Retry-After header (integer seconds, "N seconds", "Ns", "Nms")  → min(…, 300s)
// 2. body /retry_after | /retry_after_seconds | /error/retry_after | /error/retry_after_seconds
//    or any nested "message" containing "try again in <n>[s| seconds]"       → min(…, 300s)
// 3. local backoff: 30s * 2^min(consecutive_429,4), cap 300s          (30,60,120,240,300)
// sleeps add 0..20% jitter when replaying
```

Outer loop `fetch_usage_retried_inner` (per alias):
1. Cache hit within TTL → return (unless `Refresh::Unattended/Forced`).
2. Read profile auth.json → `account_id`, `is_fedramp`, id/access/refresh tokens.
3. If a **recorded auth verdict** exists for this exact refresh_token (cache `auth_failures`, keyed by alias + SHA-256(refresh_token)) → return it without a network call (only `Refresh::Forced` bypasses).
4. If a per-account 429 cooldown is active (in-process map keyed by account_id or alias) → error `"HTTP 429 rate limited"`.
5. Up to `MAX_RETRIES = 3` attempts, 1 s sleep between; each attempt = `fetch_usage_with_refresh_capturing_rejection` (4.4). Any rotated tokens are persisted **before** examining the result; a persist failure aborts the account with `UsageError::token_persist_failed`.
6. A `TerminalAuthError` grants exactly **one extra "recovery round"**: after the 1 s sleep, re-read the profile; if its refresh_token differs from the one presented, a concurrent process won the rotation → retry with the stored tokens; if unchanged → remember the verdict (if memorable) and fail.
7. `UsageRateLimited` → immediate error (no retry). Other errors → retry.
8. Success → merge cached reset credits, write cache, return.

Error classification summary:

| Situation | Behaviour |
|---|---|
| 2xx | parse; `parse_usage_checked` rejects a body with no `rate_limit` windows, no `account_limited` signal and no `credits` data (`"usage response missing recognized quota fields"`) |
| 429 | record cooldown (Retry-After or 30 s), error summary `HTTP 429 rate limited`, no retry this call |
| 401 / 403 with refresh_token | refresh once, retry the GET once with the new bearer; second failure → `"Usage API still failed (HTTP <s>) after token refresh"` |
| 401 / 403 without refresh_token | `"Usage API failed (HTTP <s>), no refresh_token available"` |
| other non-2xx | error, retried by the outer loop |
| refresh rejected terminally | `TerminalAuthError{code,message}`; summary `re-login required (<code>)`; display `token refresh rejected, sign in again — <code>[: <message>]` |
| rotated tokens could not be written | summary `refreshed token not saved`; detail `[alias] token refresh succeeded but the rotated credentials could not be saved: <cause>. The auth server has already invalidated the previous refresh token, so this profile may need to sign in again once the write problem is fixed.` |

`UsageError { summary, detail }`; `extract_error_summary` pulls the first `HTTP <status>…` fragment for the list column.

### 4.4 Single fetch with refresh (`fetch_usage_capturing_refresh`, `src/usage/api.rs:645-799`)

```
if refresh_token && (access_token or id_token expiring within 1800 s):
    proactive refresh → on success: persist (CAS), GET usage with new bearer, return
                      → on TerminalAuthError: remember, fall through with old bearer (do NOT refresh again on 401)
                      → on other error: warn, fall through with old bearer
GET usage with current bearer
  2xx → parse; 429 → rate_limited; 
  401/403 && refresh_token && no earlier terminal rejection → refresh, persist, GET once more
```

Every rotation is stored in `refreshed: Option<RefreshedTokens>` **before** any further fallible step so `?` can never drop a rotated token (`UsageFetchOutcome { refreshed, result }`).

### 4.5 Response schema and `UsageInfo` mapping (`src/usage/parse.rs`, `src/usage/mod.rs:36-100`)

Structs:

```rust
pub struct WindowUsage { pub used_percent: Option<f64>, pub resets_at: Option<i64>, pub window_minutes: Option<i64> }
pub struct SpendControlLimit { source, limit, used, remaining: Option<String>, remaining_percent: Option<f64>, resets_at: Option<i64> }
pub struct AdditionalRateLimit {
    pub limit_name: Option<String>, pub metered_feature: Option<String>,
    pub allowed: Option<bool>, pub limit_reached: Option<bool>,
    pub primary: Option<WindowUsage>, pub secondary: Option<WindowUsage>,
}
pub struct ResetCredit { pub id: String, pub granted_at: Option<String>, pub expires_at: Option<String> }
pub struct UsageInfo {
    pub fetched_at: Option<i64>,           // unix secs when parsed
    pub primary: Option<WindowUsage>,      // 5h window
    pub secondary: Option<WindowUsage>,    // 7d window
    pub credits_balance: Option<f64>,
    pub unlimited_credits: Option<bool>,
    pub plan_type: Option<String>,         // authoritative over JWT when present
    pub reset_credits_available_count: Option<u64>,
    pub reset_credits: Vec<ResetCredit>,
    pub reset_credits_error: Option<String>,
    pub account_limited: bool,
    pub rate_limit_reached_type: Option<String>,
    pub individual_limit: Option<Box<SpendControlLimit>>,
    pub additional_limits: Vec<AdditionalRateLimit>,   // model-specific quota pools
}
```

Representative real-world response (composite of the fixtures in `src/usage/parse.rs` tests, "Pro 20x account, sanitized"):

```json
{
  "plan_type": "pro",
  "rate_limit": {
    "allowed": true,
    "limit_reached": false,
    "primary_window": {
      "used_percent": 42.0,
      "limit_window_seconds": 18000,
      "reset_after_seconds": 9876,
      "reset_at": 1783843614
    },
    "secondary_window": {
      "used_percent": 84.0,
      "limit_window_seconds": 604800,
      "reset_after_seconds": 400000,
      "reset_at": 1784430414
    }
  },
  "rate_limit_reached_type": null,
  "spend_control": {
    "reached": false,
    "individual_limit": {
      "source": "workspace_spend_controls",
      "limit": "25000",
      "used": "8000",
      "remaining": "17000",
      "remaining_percent": 68,
      "reset_at": 1784430414
    }
  },
  "credits": {
    "has_credits": false,
    "unlimited": false,
    "balance": "0"
  },
  "rate_limit_reset_credits": {
    "available_count": 2,
    "credits": [
      { "id": "cred_1", "reset_type": "codex_rate_limits", "status": "available",
        "granted_at": "2026-07-01T00:00:00Z", "expires_at": "2026-07-08T00:00:00Z" },
      { "id": "cred_2", "reset_type": "codex_rate_limits", "status": "consumed",
        "expires_at": "2026-07-08T00:00:00Z" }
    ]
  },
  "code_review_rate_limit": null,
  "additional_rate_limits": [
    {
      "limit_name": "GPT-5.3-Codex-Spark",
      "metered_feature": "codex_bengalfox",
      "rate_limit": {
        "allowed": true,
        "limit_reached": false,
        "primary_window":   { "used_percent": 0, "limit_window_seconds": 18000,  "reset_after_seconds": 18000,  "reset_at": 1783843614 },
        "secondary_window": { "used_percent": 0, "limit_window_seconds": 604800, "reset_after_seconds": 604800, "reset_at": 1784430414 }
      }
    }
  ]
}
```

Free-account variant (single 7-day window in the primary slot):

```json
{
  "plan_type": "free",
  "rate_limit": {
    "allowed": false,
    "limit_reached": true,
    "primary_window": { "used_percent": 100, "limit_window_seconds": 604800, "reset_after_seconds": 437896, "reset_at": 1778468889 },
    "secondary_window": null
  }
}
```

Older/alternate fields seen in fixtures and tolerated: `remaining_seconds`, `requests_remaining`, `requests_limit`, `reset_time` (RFC3339) inside windows (ignored), numeric `credits.balance`, camelCase `rateLimitResetCredits`, `availableCount`, `resetType`, `expiresAt`, `grantedAt`.

Field-by-field mapping (`parse_usage`):

| JSON | `UsageInfo` | Notes |
|---|---|---|
| `rate_limit.primary_window.used_percent` (f64, **required** for the window to count) | `primary.used_percent` | window without `used_percent` is treated as absent |
| `rate_limit.primary_window.reset_at` (unix secs i64) | `primary.resets_at` | `reset_after_seconds` is **not** used; `reset_at` is |
| `rate_limit.primary_window.limit_window_seconds` | `primary.window_minutes` = secs/60 | 18000 → 300; 604800 → 10080 |
| `rate_limit.secondary_window.*` | `secondary.*` | same |
| primary `limit_window_seconds >= 604800` and no secondary | remap: `primary = None`, `secondary = primary` | free accounts; makes scoring treat it as 7d data |
| `credits.has_credits` (bool, default **true** when absent for old API) | gate | when `false`, `credits_balance` is forced `None` (don't show $0.00) |
| `credits.balance` (number **or** numeric string) | `credits_balance` | |
| `credits.unlimited` | `unlimited_credits` | |
| `plan_type` | `plan_type` | |
| `rate_limit_reached_type` (`{"type": "..."}` or bare string) | `rate_limit_reached_type` | |
| `account_limited` = `rate_limit_reached_type ∈ {rate_limit_reached, workspace_owner_credits_depleted, workspace_member_credits_depleted, workspace_owner_usage_limit_reached, workspace_member_usage_limit_reached}` OR `spend_control.reached == true` OR `rate_limit.limit_reached == true` | `account_limited` | unknown reason strings are ignored; a limited account is scored as 100 % used in both windows |
| `spend_control.individual_limit.{source,limit,used,remaining,remaining_percent,reset_at}` | `individual_limit` | strings or numbers → strings |
| `rate_limit_reset_credits` / `rateLimitResetCredits` → `available_count`, `credits[]` | `reset_credits_available_count`, `reset_credits` | only `reset_type == "codex_rate_limits"` (or missing) and `status == "available"` (or missing) entries kept; `id` required |
| `additional_rate_limits[] {limit_name, metered_feature, rate_limit{allowed, limit_reached, primary_window, secondary_window}}` | `additional_limits[]` | malformed entries skipped; same 7d remap per pool |
| `code_review_rate_limit` (object with windows directly, or `{rate_limit:{…}}`) | appended to `additional_limits` with `limit_name="Code review"`, `metered_feature="code_review"` | observed `null` on most accounts |

"Model-specific quota pools": `additional_rate_limits[]` entries whose `metered_feature` starts with `codex_` (e.g. `codex_bengalfox` = "GPT-5.3-Codex-Spark", `codex_other`). `is_five_hour_warmup_pool` treats them as warmup targets when `allowed != false`, `limit_reached != true` and they still have (or may open) a 5h window. Display row helper: `additional_pool_rows` → `PoolRow { limit_name (default "pool"), unavailable = limit_reached==true || allowed==false, primary, secondary }`.

`is_available(u)`: false if `account_limited`, both windows absent, or either window `used_percent >= 100`.

---

## 5. Token refresh

### 5.1 Request (`src/usage/api.rs:913-926`)

```rust
/// Build the token refresh request. Codex 0.144.1 sends a JSON body
/// ({client_id, grant_type, refresh_token}) — keep the same shape so the
/// auth server sees requests identical to the real client's.
pub(crate) fn build_refresh_request(client: &reqwest::Client, token_url: &str, refresh_token: &str) -> reqwest::RequestBuilder {
    client.post(token_url).json(&serde_json::json!({
        "client_id": CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    }))
}
```

- `POST https://auth.openai.com/oauth/token`, `Content-Type: application/json`. **No `scope` field is sent** on refresh (scope is only used at authorize / device usercode time).
- Sent with the plain client (no `http_retry`), single attempt; transport errors surface as `token refresh request failed: …`.

### 5.2 Response (`src/usage/api.rs:99-142, 298-350`)

```rust
#[derive(Deserialize)] #[serde(untagged)]
enum RefreshError { Code(String), Detail { code: Option<String>, message: Option<String>, #[serde(rename="type")] kind: Option<String> } }
#[derive(Deserialize)]
struct RefreshResponse { id_token: Option<String>, access_token: Option<String>, refresh_token: Option<String>, error: Option<RefreshError>, error_description: Option<String> }
```

Two error shapes are accepted: OAuth `{"error":"invalid_grant","error_description":"…"}` and OpenAI `{"error":{"code":"refresh_token_reused","message":"This refresh token has already been used.","param":null,"type":"invalid_request_error"}}`.

`resolve_refreshed_tokens`:
- error body present → `TerminalAuthError` if `is_terminal_auth_failure(code, status)` else `"token refresh failed: <code>: <message>"`.
- non-2xx without error body → code `http_<status>`; terminal if 4xx (except 429/408).
- 2xx: `id_token` and `access_token` fall back to the current ones if omitted; `refresh_token` falls back to the presented one if omitted → `RefreshedTokens { id_token, access_token, refresh_token }`.
- The body is never logged (may contain credentials).

### 5.3 Terminal verdicts (`src/usage/api.rs:144-186`)

```rust
/// Auth-server verdicts no retry can change, independent of HTTP status.
const TERMINAL_AUTH_CODES: &[&str] = &[
    "refresh_token_reused", "refresh_token_invalidated", "invalid_grant",
    "invalid_client", "unauthorized_client", "access_denied",
];
/// The subset that may outlive the invocation (recorded in cache.json until the credential changes).
const MEMORABLE_AUTH_CODES: &[&str] = &["refresh_token_reused", "refresh_token_invalidated"];

/// A 4xx from the token endpoint means the credential itself was rejected; 429/408 stay retryable.
fn is_terminal_auth_failure(code: &str, status: reqwest::StatusCode) -> bool {
    if matches!(status, TOO_MANY_REQUESTS | REQUEST_TIMEOUT) { return false; }
    TERMINAL_AUTH_CODES.contains(&code) || status.is_client_error()
}
```

- Only `refresh_token_reused` / `refresh_token_invalidated` are persisted ("needs re-login"), because `invalid_grant`/`access_denied` are also emitted by proxies/gateways for transient reasons. A recorded verdict is stored against SHA-256(refresh_token); signing in again (new token) clears it automatically; `list --force` also bypasses it; `cache::invalidate(alias)` drops it.
- A verdict is only recorded if the profile **still holds the presented refresh_token** (`profile_still_holds_refresh_token`), otherwise a concurrent winner's token is in place and the verdict is about a superseded credential.

### 5.4 Single-use refresh-token hazard and the discipline used

1. `refresh_token` rotates on every use; the old one is dead the instant the server issues the new one. Replaying yields `refresh_token_reused`.
2. Persist immediately with CAS (`update_profile_tokens_if_refresh_matches`, 3.5) under launch+auth locks, both to the profile and, if that profile is current, to the live `auth.json` (only when the live copy still holds the presented token).
3. A persist failure is a reportable error, not a warning (`token_persist_failed`).
4. Never abort an in-flight refresh request (no `timeout` around the join set; `JoinSet::drop` would abort tasks and lose the credential) — `refresh_expiring_tokens_within` uses the budget only to decide whether to **start** the next refresh.
5. Do not run two copies of the same account (import dedupe, 3.6).
6. Never overwrite a profile's refresh_token with one that cannot be proven newer (freshness guard, 3.4); when `launch` restores the backup it first folds any token Codex rotated in place back into the profile (7.2).
7. Recovery round (4.3 step 6) distinguishes "another process rotated it" from "truly dead".

### 5.5 When refresh happens

- **Proactive**: before the usage GET when either JWT expires within `OPPORTUNISTIC_REFRESH_MARGIN = 1800 s`.
- **Reactive**: on 401/403 from the usage API (once).
- **Opportunistic batch** (`refresh_expiring_tokens`, called by `list`/`best`): scan all profiles, skip ones with a recorded verdict, pick those whose min(access exp, id exp) − now < 1800 s, sort soonest first, take 3, refresh with concurrency 2, start-budget 8 s, await all started ones; returns `Vec<TokenPersistFailure>` for the caller to print.
- codex-switch itself never refreshes on a plain `use`/switch.

---

## 6. Login flows (`src/login.rs`)

### 6.1 Browser PKCE flow (`run_device_auth`, `src/login.rs:126-181`)

1. `ensure_file_credentials_store()`.
2. PKCE: `code_verifier` = base64url-no-pad of 64 random bytes (86 chars); `code_challenge` = base64url-no-pad(SHA-256(code_verifier)); method `S256`. `state` = base64url-no-pad of 32 random bytes.
3. Bind `127.0.0.1:1455`, fallback `127.0.0.1:1457` (Windows error 10013 gets a `net stop winnat/hns` hint). `redirect_uri = http://localhost:<port>/auth/callback` (host must be `localhost`, not 127.0.0.1).
4. Authorize URL (`build_authorize_url`), parameters in this order, values url-encoded:

```
https://auth.openai.com/oauth/authorize
  ?response_type=code
  &client_id=app_EMoamEEZ73f0CkXaXp7hrann
  &redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback
  &scope=openid%20profile%20email%20offline_access%20api.connectors.read%20api.connectors.invoke
  &code_challenge=<S256 challenge>
  &code_challenge_method=S256
  &id_token_add_organizations=true
  &codex_cli_simplified_flow=true
  &state=<state>
  &originator=codex_cli_rs
  [&allowed_workspace_id=ws1%2Cws2]      # only when config.toml forced_chatgpt_workspace_id is set
```

5. Open browser (`webbrowser` crate; Windows tries `rundll32.exe url.dll,FileProtocolHandler <url>` first). Print the URL as fallback.
6. Wait up to 600 s for `GET /auth/callback?code=…&state=…` (Ctrl+C → `LoginCancelled`). Up to 16 concurrent connections, 5 s per-connection read timeout, 8 KiB buffer. Responses: wrong path → 404; state mismatch → 403 `Invalid callback state`; `error=` param → 400 and bail `Authorization failed: <error>`; missing code → 400; success → 200 HTML `✓ Login successful … You may close this tab`.
7. Exchange code (`exchange_code_with_redirect`): `POST https://auth.openai.com/oauth/token`, `Content-Type: application/x-www-form-urlencoded`, body

```
grant_type=authorization_code&code=<code>&redirect_uri=<redirect_uri>&client_id=app_EMoamEEZ73f0CkXaXp7hrann&code_verifier=<verifier>
```

   Retries: up to 3 attempts, backoff 150 ms × attempt, only on transport errors (`is_connect() || is_timeout() || is_request()`) or 5xx; any 4xx is final (one-shot code). Response `{id_token, access_token, refresh_token}` all required; error shapes as in 5.2.
8. `validate_managed_chatgpt_account(&id_token)`.
9. Best-effort API-key exchange (`obtain_api_key`): `POST /oauth/token`, form body

```
grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange
&client_id=app_EMoamEEZ73f0CkXaXp7hrann
&requested_token=openai-api-key
&subject_token=<id_token>
&subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aid_token
```

   Response `{ "access_token": "sk-…" }` → stored as `OPENAI_API_KEY`; any failure → `null`.
10. `build_auth_from_tokens` → auth.json value (1.2) + `AccountInfo`; then `profile::save_auth_value(auth, alias)` (writes live + profile + current), `workspace::refresh_for_auth` (best-effort), daemon restart report (section 8). `login <existing-alias>` instead goes through `replace_profile_auth_and_live_if_current` ("re-auth").

### 6.2 Device-code flow (`run_device_code_auth`, `src/login.rs:529-883`) — note it is NOT RFC 8628 wire-compatible

1. `POST https://auth.openai.com/api/accounts/deviceauth/usercode` JSON `{"client_id": CLIENT_ID, "scope": SCOPE, "originator": "codex_cli_rs"}` → `{ "device_auth_id": "...", "user_code": "XXXX-XXXX", "interval": "5" }` (`interval` is a string; default 5 s).
2. Show: `To sign in, visit: https://auth.openai.com/codex/device` and `Enter code: <user_code>`.
3. Poll `POST https://auth.openai.com/api/accounts/deviceauth/token` JSON `{"device_auth_id", "user_code", "client_id"}` every `interval` s, deadline 900 s, Ctrl+C safe.
   - HTTP 403 / 404 → still pending.
   - 429 → `rate_limit_decision` delay (header/body/local backoff).
   - error codes: `deviceauth_authorization_unknown` | `authorization_pending` → continue; `slow_down` → interval += 5; `expired_token` | `deviceauth_expired` → bail `Device code expired`; `access_denied` → bail `Authorization was denied by the user.`; unknown code on 2xx/403/404/408/429/5xx → retry, on other 4xx → bail.
   - Success body: `{ "authorization_code": "...", "code_challenge": "...", "code_verifier": "..." }` (server supplies the PKCE pair).
4. Exchange the returned `authorization_code` + `code_verifier` with the same form POST as 6.1 step 7 but `redirect_uri = https://auth.openai.com/deviceauth/callback`.
5. Managed-policy check; no API-key exchange in this flow.

---

## 7. `launch`: staging/restoring the live `auth.json` (`src/launch.rs:127-368, 1067-1280`)

Sequence for a ChatGPT profile:
1. Resolve alias (explicit, or best-account auto-select). Locate `codex` on PATH without executing it (`command_on_path`; `codex --version` would write into `$CODEX_HOME/tmp`).
2. `forwarded = embedded_codex_argv(codex_supports_no_daemon(&codex), chatgpt_codex_argv(model, reasoning, args))` (see 8.2).
3. Register SIGINT/SIGTERM listener **before** touching auth.json.
4. Take `launch.lock` (held until restore). Snapshot `had_original = live.exists()`.
5. Under `auth.lock`: `backup_launch_auth` (copy live → `auth.json.bak.<pid>.<unix_secs>` via atomic private write), then `stage_profile_auth(alias)` (write profile → live, no `current` update). Release `auth.lock`.
6. Spawn `codex` with inherited stdio (or piped for `--json`). No `CODEX_HOME` override for ChatGPT launches.
7. Sleep `restore_delay_secs` (default 3 s; 0 is normalized to 3 with a warning) — "Codex CLI reads auth.json only at startup". Interrupt during the window → kill child, restore, exit with signal code.
8. `restore_launch_auth` under `auth.lock`:
   - `preserve_refreshed_launch_auth`: if the live file's `last_refresh` is strictly newer than the profile's (or the profile has none and live has one) **and** identity matches → write live into the profile (Codex rotated the token in place while staged). Print `Codex refreshed the credentials of profile '<alias>'; saved them before restoring.` If live is newer but belongs to another account or the write fails → **refuse to roll back** and leave the live file (error text names the `.bak` path and suggests `codex-switch import`).
   - If the backup is the same account with the same refresh_token as the staged profile and a refresh was preserved → just delete the backup (live already holds the newest).
   - Else restore backup → live (atomic), delete backup; or if there was no original, delete the staged live file.
9. Release `launch.lock`, wait for the child, propagate its exit code (signal death → 128+n).

---

## 8. App-server daemon restart and `--no-daemon` (`src/app_server.rs`, `src/launch.rs:394-425`)

### 8.1 Why
Codex CLI 0.157+ attaches interactive sessions to a managed local app-server daemon which loads `auth.json` once and re-reads it only for the account it already holds. `codex exec` and `codex --no-daemon` run in process and read the file at startup.

### 8.2 Probe / restart

```rust
pub enum DaemonRestart { NotRunning, Unchanged, Restarted, Failed(String) }

pub struct LiveAuthSnapshot(Option<String>);                      // sha256 hex of live auth.json
pub fn snapshot_live_auth() -> LiveAuthSnapshot { … codex_auth_path().and_then(sha256_file) … }

pub fn restart_daemon_if_live_auth_changed(before: &LiveAuthSnapshot) -> DaemonRestart {
    if live_auth_unchanged(before, &snapshot_live_auth()) { return DaemonRestart::Unchanged; }
    restart_daemon_if_running()
}
/// A missing or unreadable file before the change gives no evidence → counts as changed.
fn live_auth_unchanged(before, after) -> bool { before.0.is_some() && before == after }

fn restart_with(run_codex) -> DaemonRestart {
    match run_codex(&["app-server", "daemon", "version"]) {
        Ok(output) if daemon_is_running(&output) => {}
        Ok(_) | Err(_) => return DaemonRestart::NotRunning,     // stopped daemon, old Codex, codex missing
    }
    match run_codex(&["app-server", "daemon", "restart"]) {
        Ok(output) if output.status.success() => DaemonRestart::Restarted,
        Ok(output) => DaemonRestart::Failed(first non-empty stderr line, or "codex exited with <status>"),
        Err(err) => DaemonRestart::Failed(err.to_string()),
    }
}
/// `codex app-server daemon version` prints {"status":"running","backend":"pid","cliVersion":"0.158.0","appServerVersion":"0.159.0"}
fn daemon_is_running(output: &Output) -> bool {
    output.status.success() && serde_json::from_slice::<Value>(&output.stdout).ok().is_some_and(|v| v["status"] == "running")
}
```

- Commands run with `stdin = null`; `codex` resolved via PATH lookup (`command_on_path("codex")`).
- A stopped daemon is never started ("restart would start a daemon nobody asked for"); a stopped daemon reports e.g. `Error: failed to connect to /home/u/.codex/app-server-control/app-server-control.sock`; an old Codex reports `error: unrecognized subcommand 'daemon'`.
- Messages (`DaemonRestart::message(alias)`):
  - Restarted: `Restarted the Codex app-server daemon; new and reconnecting Codex sessions use '<alias>'.`
  - Failed: `Warning: the Codex app-server daemon still holds the previous account (<detail>). Run `codex app-server daemon restart` so Codex sessions use '<alias>'.` (printed to stderr in error colour; the switch itself still succeeds)
  - NotRunning / Unchanged: nothing.
- Call sites: `use <alias>`, best-select switch, `login` (when live was replaced), TUI switch/login. Snapshot is taken **before** the write, restart decided **after**.

### 8.3 `--no-daemon` argv rule (`src/launch.rs:394-425`)

```rust
pub(crate) fn embedded_codex_argv(supports_no_daemon: bool, mut argv: Vec<String>) -> Vec<String> {
    if supports_no_daemon && !argv.iter().any(|arg| picks_app_server(arg)) {
        argv.insert(0, "--no-daemon".to_string());     // root option → before any subcommand
    }
    argv
}
fn picks_app_server(arg: &str) -> bool {
    arg == "--no-daemon" || arg == "--remote" || arg.starts_with("--remote=") || arg == "agents"
}
/// `--no-daemon` exists since Codex 0.156; older Codex rejects unknown options, so `codex --help` stdout decides.
fn codex_supports_no_daemon(command: &Path) -> bool {
    Command::new(command).arg("--help").stdin(null).stderr(null).output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("--no-daemon"))
}
```

Also: `--model <m>` and `-c model_reasoning_effort=<e>` are spliced **after** a Codex subcommand (`exec`, `resume`, …) because Codex 0.149 ignores flags placed before the subcommand; interactive launch (no subcommand) keeps them in front (`splice_after_subcommand`, `src/launch.rs:442-470`; subcommand list in `src/cli.rs:338-368`).

---

## 9. Isolated per-run `CODEX_HOME` (custom-provider launches; `src/provider.rs:1727-2615`)

Only used for API-provider launches, never for ChatGPT launches (those use the real `$CODEX_HOME`). Directory: `$CODEX_SWITCH_HOME/provider-runs/<identity_id>/<pid>-<unix_nanos>-<seq>/` (0700), passed to the child as env `CODEX_HOME=<dir>`.

```rust
/// Keys that provider `launch` supplies via `codex -c`. They stay in the user's config.toml and are omitted from the per-launch home.
const PROVIDER_SESSION_KEYS: [&str; 6] = ["model", "model_provider", "model_reasoning_effort", "model_catalog_json", "model_providers", "web_search"];
const USER_PROMPT_LINKS: [&str; 3] = ["AGENTS.md", "prompts", "skills"];
```

`ProviderCodexHome::begin`:
1. `base_config = load_toml_if_present($CODEX_HOME/config.toml)` (empty file → None).
2. For each of `AGENTS.md`, `prompts`, `skills`: if it exists in the user home and nothing exists at the run path → **symlink** (unix `symlink`; Windows `symlink_dir/file`, falling back to a recursive **copy** if symlinking fails).
3. `config.toml`: copy of the user's config with the six `PROVIDER_SESSION_KEYS` **removed** (so MCP servers, sandbox, etc. carry over; model/provider come from `-c`), written atomically.
4. `provider_run.json` metadata `{provider_identity_id, alias, model?, cwd?, created_at}`.
5. **Not** linked or copied: `auth.json` (provider key goes in env under `env_key`), `sessions/`, `history.jsonl`, `sqlite`, `log/`, `tmp/` — each run gets fresh session/history state inside its own home; sessions are later indexed from `<run>/sessions/**/rollout-*.jsonl`, `session_index.json[l]`, `session_meta.json` for `resume`.

`restore()` (on exit, spawn failure, shutdown, and `Drop`): `merge_isolated_config_into_user(user_config, base_config, run_config)` — a **three-way merge** (base = user config at start, ours = run config now, theirs = user config now) under `codex-config.lock`, skipping `PROVIDER_SESSION_KEYS`; tables merge recursively, scalars: `ours==theirs → either; ours==base → theirs; theirs==base → ours; else ours`. This carries MCP servers Codex added during the session (`codex mcp add`) back into the user's config without clobbering concurrent edits. If the run config vanished while base existed → error `provider launch config … disappeared before it could be merged`.

`open_existing` (resume): refreshes the run's `config.toml` from the current user config (session keys re-applied from the old run config), re-links prompt entries, and takes a per-run `resume.lock` (`ProviderRunLease`, zero wait).

Gotchas noted in comments: concurrent launches must not share sqlite; `codex --version` must not be run for preflight (writes into `$CODEX_HOME/tmp`); the run dir is retained after the process exits so history stays addressable (tombstone after provider removal).

---

## 10. Usage cache (`src/cache.rs`)

File `$CODEX_SWITCH_HOME/cache.json` (compact JSON, atomic private write, guarded by in-process mutex + `cache.lock` flock with 15 s wait / 50 ms poll):

```jsonc
{
  "entries": {
    "<alias>": {
      "ts": 1790000000,                    // fetched_at unix secs (TTL reference)
      "primary_used": 42.0, "primary_reset": 1783843614, "primary_window_minutes": 300,
      "secondary_used": 84.0, "secondary_reset": 1784430414, "secondary_window_minutes": 10080,
      "credits_balance": null, "unlimited_credits": false, "plan_type": "pro",
      "reset_credits_available_count": 2,
      "reset_credits": [{"id":"cred_1","granted_at":"…","expires_at":"…"}],
      "reset_credits_error": null,
      "account_limited": false, "rate_limit_reached_type": null,
      "individual_limit": null,
      "additional_limits": [ { "limit_name": "...", "metered_feature": "codex_bengalfox", "allowed": true, "limit_reached": false,
                               "primary": {"used_percent":0.0,"resets_at":…,"window_minutes":300}, "secondary": {…} } ]
    }
  },
  "last_used": { "<alias>": 1790000000 },             // set by `use`
  "workspace_names": { "<account_id>": "Platform Team" },
  "workspace_names_absent": { "<account_id>": 1790000000 },   // confirmed "no workspace name", TTL 24 h
  "auth_failures": {
    "<alias>": { "ts": 1790000000, "credential": "<sha256 hex of refresh_token>",
                 "summary": "re-login required (refresh_token_reused)", "detail": "…≤512 chars, control chars stripped…" }
  }
}
```

- TTL: `config.cache.ttl` (default 300 s); `get(alias)` returns None when `now - ts > ttl`. `Refresh::Unattended` (timers, warmup) and `Refresh::Forced` (`--force`) bypass the TTL; only `Forced` bypasses `auth_failures`.
- `auth_failures` has no TTL; it is invalidated by a changed refresh_token, `invalidate(alias)`, or `--force`. `rename(old,new)` migrates entries, last_used and auth_failures.
- A window is reconstructed only if `*_used` or `*_reset` is present. Batch writes (`put_many`) keep an existing entry if its `ts` is newer or equal.

---

## 11. "Best account" scoring inputs (`src/usage/scoring.rs`, `src/usage/mod.rs:128-212`, `src/commands/profile.rs:331-365, 466-557`)

Candidate built from `UsageInfo` + `AccountInfo` + `last_used` (cache) + shared `now`:

```rust
pub struct Candidate {
    pub alias: String,
    pub used_5h: f64, pub resets_at_5h: Option<i64>,      // 100 / None when account_limited
    pub used_7d: f64, pub resets_at_7d: Option<i64>,
    pub has_5h_data: bool, pub has_7d_data: bool,         // window present (or account_limited)
    pub is_team: bool, pub is_free: bool,                 // usage.plan_type first, else JWT AccountInfo
    pub last_used: i64, pub now: i64,
    pub pool_size: usize, pub pool_exhausted: usize, pub team_priority: bool,
}
// effective_used_5h/7d(): 0.0 once resets_at <= now
```

Eligibility (`is_candidate_eligible(c, safety_margin_7d)`): needs some window data; 5h not ≥100; 7d not ≥100; if 7d remaining < max(0.25·margin, 1) and reset > 48 h away → ineligible; Free plan with 5h remaining < 35 % → ineligible.

Score (`score_unified`) = `tier_bonus (Team && team_priority → 500)` + `headroom` (no 5h data → 50; exhausted → 0..500 by closeness of reset; else 1000 + min(projected minutes to exhaustion at current burn rate, 300)/3, or 1000 + remaining % when burn ≈ 0) + `sustain` (no 7d data → −50; 7d exhausted → −800 relieved linearly toward reset; below `safety_margin_7d` → −800 × budget-per-remaining-5h-window shortfall × (1 − relief within 48 h × 0.8)) + `drain_value` (0..300 when the 5h window resets within 60 min and quota would be wasted, weighted 0.5/0.3/1.0/1.5 by pool size and exhausted ratio) + `recency` (−60..0, decays 1 point per 30 s since last_used).

Ordering: eligible first, then score desc, then `last_used` asc, then alias. Pace helper `pace_percent(window, secs)` = elapsed fraction of the window in %, used for display ("pace"). If nothing is eligible, reset-card revival may be offered (`rate_limit_reset_credits`).

---

## 12. Other Codex endpoints used (brief)

- **Workspace name** (`src/workspace.rs`): `GET https://chatgpt.com/backend-api/wham/accounts/check`, bearer + `Accept: application/json` + `ChatGPT-Account-ID`, 5 s timeout, idempotent retry. Response either `{"accounts":[{"id","name","structure"}]}` or `{"accounts":{"<id>":{"account":{"account_id","name","structure"}}}, "account_ordering":[…]}`. Result `Unlisted | Unnamed | Named(name)`; `Unnamed` cached 24 h, `Unlisted` not cached.
- **Reset credits** (`src/usage/reset_credits.rs`): list/consume URLs in §0; headers `Accept: application/json`, `OpenAI-Beta: codex-1`, `Originator: Codex Desktop`, `Chatgpt-Account-Id`. Consume body `{"credit_id": "...", "redeem_request_id": "<uuid v4>"}`; success `{"code":"reset","windows_reset":N,"credit":{"redeemed_at":"…"}}`. Consume is never auto-replayed; 3 s spacing between list calls.
- **Warmup** (`src/warmup.rs`): `POST https://chatgpt.com/backend-api/codex/responses` with routing headers and body `{"model": m, "instructions": "You are a helpful assistant.", "input": [{"type":"message","role":"user","content":[{"type":"input_text","text":"ping"}]}], "tools": [], "tool_choice": "auto", "parallel_tool_calls": false, "stream": true, "store": false, "include": []}`; one request per 5h pool; models from `GET …/codex/models`. Skipped for 7d-only (free) accounts.

---

## 13. Gotchas checklist for ccsw

1. Always require `cli_auth_credentials_store = "file"` (or absent) and refuse `forced_login_method = "api"`; re-check managed `forced_chatgpt_workspace_id` at every credential write.
2. Treat `refresh_token` as single-use: persist rotations with CAS on the presented token, to profile **and** live copy; never drop a rotated token on an error path; never cancel an in-flight refresh; never keep two copies of one account.
3. `last_refresh` must be stamped (`%Y-%m-%dT%H:%M:%SZ`) on every rotation or Codex will re-refresh (8-day rule) and rotate underneath you.
4. `tokens.account_id` is `null` (not `""`) when unknown; identity comes from the JWT claim first.
5. Usage windows: `used_percent` is required; `reset_at` is the epoch; free accounts put the only (7d) window in `primary_window` → remap to secondary; `credits.has_credits=false` → hide balance; `balance` may be a string.
6. Usage API plan_type overrides the JWT plan.
7. Restart the app-server daemon only if it is running (`daemon version` → `"status":"running"`) and the live file hash actually changed; pass `--no-daemon` on launch when `codex --help` lists it and argv doesn't already pick a server.
8. Do not run `codex --version` as a preflight (writes into `$CODEX_HOME/tmp`).
9. Backups: `auth.json.bak.<nanos>`, keep 3, 0600; launch backups `auth.json.bak.<pid>.<secs>` are removed on restore.
10. `CODEX_HOME` with `..` is rejected; empty means default.
