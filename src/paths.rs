//! Where everything lives: the ccsw backup store and the Codex home.

use std::path::{Component, Path, PathBuf};

use crate::errors::{CcswError, Result};

/// `CCSW_KEYCHAIN=off|0|false` disables the Keychain; it is never used off macOS.
pub fn keychain_enabled_from(value: Option<&str>) -> bool {
    let off = value.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false"
        )
    });
    cfg!(target_os = "macos") && !off
}

/// `/a/b` + `.lock` → `/a/b.lock` (no extension games with dotfiles).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `$CCSW_HOME`, default `~/.ccsw`.
    pub backup_root: PathBuf,
    /// `$CODEX_HOME`, default `~/.codex`.
    pub codex_home: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`, default `~/.claude`.
    pub claude_home: PathBuf,
    /// Source of shared customizations, independent of a pinned session.
    pub claude_default_home: PathBuf,
    /// Exact exported value; Keychain hashing must retain trailing slashes and Unicode.
    pub claude_config_dir_raw: Option<String>,
    /// Defined-but-empty selects the default secure store.
    pub claude_secure_storage_dir: Option<String>,
    /// Where `.claude.json` lives: `$CLAUDE_CONFIG_DIR` when set, else the user home.
    pub claude_config_base: PathBuf,
    /// macOS with `CCSW_KEYCHAIN` not `off`; the file backend otherwise.
    pub keychain_enabled: bool,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| CcswError::config("could not determine home directory"))?;
        let mut paths = Self::from_values(
            std::env::var_os("CCSW_HOME").map(PathBuf::from),
            std::env::var_os("CODEX_HOME").map(PathBuf::from),
            std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            &home,
        )?;
        paths.claude_secure_storage_dir = std::env::var("CLAUDE_SECURESTORAGE_CONFIG_DIR").ok();
        paths.keychain_enabled =
            keychain_enabled_from(std::env::var("CCSW_KEYCHAIN").ok().as_deref());
        Ok(paths)
    }

    /// Empty overrides are ignored; a `CODEX_HOME` or `CLAUDE_CONFIG_DIR` with a `..` component is refused.
    pub fn from_values(
        ccsw_home: Option<PathBuf>,
        codex_home: Option<PathBuf>,
        claude_config_dir: Option<PathBuf>,
        user_home: &Path,
    ) -> Result<Self> {
        let backup_root = match ccsw_home.filter(|p| !p.as_os_str().is_empty()) {
            Some(path) => path,
            None => user_home.join(".ccsw"),
        };
        let codex_home = match codex_home.filter(|p| !p.as_os_str().is_empty()) {
            Some(path) => {
                if path.components().any(|c| matches!(c, Component::ParentDir)) {
                    return Err(CcswError::config(format!(
                        "CODEX_HOME contains '..' component which is not allowed: {}",
                        path.display()
                    )));
                }
                path
            }
            None => user_home.join(".codex"),
        };
        let claude_dir = claude_config_dir.filter(|p| !p.as_os_str().is_empty());
        if let Some(path) = &claude_dir
            && path.components().any(|c| matches!(c, Component::ParentDir))
        {
            return Err(CcswError::config(format!(
                "CLAUDE_CONFIG_DIR contains '..' component which is not allowed: {}",
                path.display()
            )));
        }
        let claude_config_dir_raw = claude_dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        let (claude_home, claude_config_base) = match claude_dir {
            Some(path) => {
                // `/x/cc/` must derive `/x/cc.lock`, the path Claude Code locks.
                let path: PathBuf = path.components().collect();
                (path.clone(), path)
            }
            None => (user_home.join(".claude"), user_home.to_path_buf()),
        };
        Ok(Self {
            backup_root,
            codex_home,
            claude_home,
            claude_default_home: user_home.join(".claude"),
            claude_config_dir_raw,
            claude_secure_storage_dir: None,
            claude_config_base,
            keychain_enabled: false,
        })
    }

    pub fn sequence_file(&self) -> PathBuf {
        self.backup_root.join("sequence.json")
    }
    pub fn settings_file(&self) -> PathBuf {
        self.backup_root.join("settings.json")
    }
    pub fn mappings_file(&self) -> PathBuf {
        self.backup_root.join("mappings.json")
    }
    pub fn state_file(&self) -> PathBuf {
        self.backup_root.join("autoswitch_state.json")
    }
    pub fn state_lock_file(&self) -> PathBuf {
        self.backup_root.join(".autoswitch_state.lock")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.backup_root.join(".lock")
    }
    pub fn credentials_dir(&self) -> PathBuf {
        self.backup_root.join("credentials")
    }
    pub fn credential_file(&self, slot: u32) -> PathBuf {
        self.credentials_dir().join(format!("{slot}.json"))
    }
    pub fn credential_prev_file(&self, slot: u32) -> PathBuf {
        self.credentials_dir().join(format!("{slot}.json.prev"))
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.backup_root.join("cache")
    }
    pub fn usage_file(&self) -> PathBuf {
        self.cache_dir().join("usage.json")
    }
    pub fn usage_lock_file(&self) -> PathBuf {
        self.cache_dir().join(".usage.lock")
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.backup_root.join("sessions")
    }
    pub fn session_dir(&self, slot: u32, email: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("{slot}-{}", slugify_email(email)))
    }
    pub fn log_file(&self) -> PathBuf {
        self.backup_root.join("ccsw.log")
    }

    /// `$CODEX_HOME/auth.json`.
    pub fn live_auth_file(&self) -> PathBuf {
        self.codex_home.join("auth.json")
    }
    pub fn codex_config_file(&self) -> PathBuf {
        self.codex_home.join("config.toml")
    }

    /// `<claude home>/.credentials.json`.
    pub fn claude_credentials_file(&self) -> PathBuf {
        self.claude_home.join(".credentials.json")
    }

    /// `<claude home>/.config.json` when it exists (legacy), else
    /// `<base>/.claude.json` — the rule Claude Code itself applies.
    pub fn claude_global_config_file(&self) -> PathBuf {
        let legacy = self.claude_home.join(".config.json");
        if legacy.exists() {
            legacy
        } else {
            self.claude_config_base.join(".claude.json")
        }
    }

    /// Claude Code's primary credential-refresh lock directory.
    pub fn claude_refresh_lock_dir(&self) -> PathBuf {
        self.claude_home.join(".oauth_refresh.lock")
    }

    /// `~/.claude.lock`: the legacy credential lock, a sibling of the config home.
    pub fn claude_legacy_lock_dir(&self) -> PathBuf {
        with_suffix(&self.claude_home, ".lock")
    }

    /// `~/.claude.json.lock`: the global-config lock.
    pub fn claude_config_lock_dir(&self) -> PathBuf {
        with_suffix(&self.claude_global_config_file(), ".lock")
    }

    /// Where the outgoing Claude login is backed up before a switch.
    pub fn claude_backups_dir(&self) -> PathBuf {
        self.backup_root.join("backups").join("claude")
    }

    /// Codex must keep its credentials in `auth.json`: `cli_auth_credentials_store`
    /// absent or `"file"`. The keyring, `auto` and `ephemeral` stores bypass the file
    /// ccsw switches. A missing `config.toml` means the file store.
    pub fn validate_credential_store(&self) -> Result<()> {
        let config_path = self.codex_config_file();
        let Ok(raw) = std::fs::read_to_string(&config_path) else {
            return Ok(());
        };
        let config: toml::Value = toml::from_str(&raw).map_err(|err| {
            CcswError::config(format!(
                "{} could not be parsed ({err}); fix it before continuing",
                config_path.display()
            ))
        })?;
        match config.get("cli_auth_credentials_store") {
            None => {}
            Some(toml::Value::String(mode)) if mode == "file" => {}
            Some(_) => {
                return Err(CcswError::config(format!(
                    "ccsw requires file-based Codex credentials; set cli_auth_credentials_store = \"file\" in {}",
                    config_path.display()
                )));
            }
        }
        Ok(())
    }
}

/// Session-profile directory suffix: NFC-normalized email keeping ASCII letters,
/// digits, `.`, `_`, `-`; everything else becomes `_`.
pub fn slugify_email(email: &str) -> String {
    email
        .chars()
        .map(|ch| {
            if ch.is_ascii() && (ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let home = Path::new("/home/u");
        let paths = Paths::from_values(None, None, None, home).unwrap();
        assert_eq!(paths.backup_root, home.join(".ccsw"));
        assert_eq!(paths.codex_home, home.join(".codex"));
        assert_eq!(paths.live_auth_file(), home.join(".codex/auth.json"));
        assert_eq!(paths.claude_home, home.join(".claude"));
        assert_eq!(
            paths.claude_credentials_file(),
            home.join(".claude/.credentials.json")
        );
        assert_eq!(paths.claude_global_config_file(), home.join(".claude.json"));
        assert_eq!(
            paths.claude_refresh_lock_dir(),
            home.join(".claude/.oauth_refresh.lock")
        );
        assert_eq!(paths.claude_legacy_lock_dir(), home.join(".claude.lock"));
        assert_eq!(
            paths.claude_config_lock_dir(),
            home.join(".claude.json.lock")
        );
        assert_eq!(
            paths.claude_backups_dir(),
            home.join(".ccsw/backups/claude")
        );
        assert!(
            !paths.keychain_enabled,
            "from_values never touches the Keychain"
        );

        let paths = Paths::from_values(
            Some(PathBuf::from("")),
            Some(PathBuf::from("/tmp/codex")),
            Some(PathBuf::from("/tmp/cc")),
            home,
        )
        .unwrap();
        assert_eq!(paths.backup_root, home.join(".ccsw"));
        assert_eq!(paths.codex_home, PathBuf::from("/tmp/codex"));
        assert_eq!(paths.claude_home, PathBuf::from("/tmp/cc"));
        assert_eq!(
            paths.claude_global_config_file(),
            PathBuf::from("/tmp/cc/.claude.json"),
            "CLAUDE_CONFIG_DIR moves .claude.json inside it"
        );
        assert_eq!(
            paths.claude_config_lock_dir(),
            PathBuf::from("/tmp/cc/.claude.json.lock")
        );

        let err =
            Paths::from_values(None, Some(PathBuf::from("/tmp/../x")), None, home).unwrap_err();
        assert!(err.to_string().contains(".."));
        let err =
            Paths::from_values(None, None, Some(PathBuf::from("/tmp/../c")), home).unwrap_err();
        assert!(err.to_string().contains("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn a_trailing_slash_in_claude_config_dir_keeps_claude_codes_lock_path() {
        let home = Path::new("/home/u");
        for raw in ["/x/cc/", "/x/cc/.", "/x/./cc//"] {
            let paths = Paths::from_values(None, None, Some(PathBuf::from(raw)), home).unwrap();
            assert_eq!(paths.claude_home, PathBuf::from("/x/cc"), "{raw}");
            assert_eq!(paths.claude_config_base, PathBuf::from("/x/cc"), "{raw}");
            assert_eq!(
                paths.claude_legacy_lock_dir(),
                PathBuf::from("/x/cc.lock"),
                "{raw}"
            );
        }
    }

    #[test]
    fn legacy_claude_config_wins_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            Paths::from_values(None, None, Some(dir.path().to_path_buf()), dir.path()).unwrap();
        assert_eq!(
            paths.claude_global_config_file(),
            dir.path().join(".claude.json")
        );
        std::fs::write(dir.path().join(".config.json"), "{}").unwrap();
        assert_eq!(
            paths.claude_global_config_file(),
            dir.path().join(".config.json")
        );
    }

    #[test]
    fn keychain_switch_reads_the_environment() {
        assert!(!keychain_enabled_from(Some("off")));
        assert!(!keychain_enabled_from(Some("0")));
        assert_eq!(keychain_enabled_from(None), cfg!(target_os = "macos"));
        assert_eq!(keychain_enabled_from(Some("on")), cfg!(target_os = "macos"));
    }

    #[test]
    fn credential_store_gate() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            Paths::from_values(None, Some(dir.path().to_path_buf()), None, dir.path()).unwrap();
        assert!(paths.validate_credential_store().is_ok(), "no config.toml");
        std::fs::write(paths.codex_config_file(), "model = \"x\"\n").unwrap();
        assert!(paths.validate_credential_store().is_ok(), "key absent");
        std::fs::write(
            paths.codex_config_file(),
            "cli_auth_credentials_store = \"file\"\n",
        )
        .unwrap();
        assert!(paths.validate_credential_store().is_ok());
        std::fs::write(
            paths.codex_config_file(),
            "cli_auth_credentials_store = \"keyring\"\n",
        )
        .unwrap();
        let err = paths.validate_credential_store().unwrap_err();
        assert_eq!(err.type_name(), "ConfigError");
        assert!(err.to_string().contains("cli_auth_credentials_store"));
    }

    #[test]
    fn slug_and_session_dir() {
        assert_eq!(
            slugify_email("user+tag@example.com"),
            "user_tag_example.com"
        );
        assert_eq!(slugify_email("bø@x.com"), "b__x.com");
        let paths =
            Paths::from_values(Some(PathBuf::from("/s")), None, None, Path::new("/h")).unwrap();
        assert_eq!(
            paths.session_dir(2, "a@b.c"),
            PathBuf::from("/s/sessions/2-a_b.c")
        );
    }
}
