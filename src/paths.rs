//! Where everything lives: the cswitch backup store and the Codex home.

use std::path::{Component, Path, PathBuf};

use crate::errors::{CswitchError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `$CSWITCH_HOME`, default `~/.cswitch`.
    pub backup_root: PathBuf,
    /// `$CODEX_HOME`, default `~/.codex`.
    pub codex_home: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| CswitchError::config("could not determine home directory"))?;
        Self::from_values(
            std::env::var_os("CSWITCH_HOME").map(PathBuf::from),
            std::env::var_os("CODEX_HOME").map(PathBuf::from),
            &home,
        )
    }

    /// Empty overrides are ignored; a `CODEX_HOME` with a `..` component is refused,
    /// as Codex itself refuses it.
    pub fn from_values(
        cswitch_home: Option<PathBuf>,
        codex_home: Option<PathBuf>,
        user_home: &Path,
    ) -> Result<Self> {
        let backup_root = match cswitch_home.filter(|p| !p.as_os_str().is_empty()) {
            Some(path) => path,
            None => user_home.join(".cswitch"),
        };
        let codex_home = match codex_home.filter(|p| !p.as_os_str().is_empty()) {
            Some(path) => {
                if path.components().any(|c| matches!(c, Component::ParentDir)) {
                    return Err(CswitchError::config(format!(
                        "CODEX_HOME contains '..' component which is not allowed: {}",
                        path.display()
                    )));
                }
                path
            }
            None => user_home.join(".codex"),
        };
        Ok(Self {
            backup_root,
            codex_home,
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
        self.backup_root.join("cswitch.log")
    }

    /// `$CODEX_HOME/auth.json`.
    pub fn live_auth_file(&self) -> PathBuf {
        self.codex_home.join("auth.json")
    }
    pub fn codex_config_file(&self) -> PathBuf {
        self.codex_home.join("config.toml")
    }

    /// Codex must keep its credentials in `auth.json`: `cli_auth_credentials_store`
    /// absent or `"file"`. The keyring, `auto` and `ephemeral` stores bypass the file
    /// cswitch switches. A missing `config.toml` means the file store.
    pub fn validate_credential_store(&self) -> Result<()> {
        let config_path = self.codex_config_file();
        let Ok(raw) = std::fs::read_to_string(&config_path) else {
            return Ok(());
        };
        let config: toml::Value = toml::from_str(&raw).map_err(|err| {
            CswitchError::config(format!(
                "{} could not be parsed ({err}); fix it before continuing",
                config_path.display()
            ))
        })?;
        match config.get("cli_auth_credentials_store") {
            None => {}
            Some(toml::Value::String(mode)) if mode == "file" => {}
            Some(_) => {
                return Err(CswitchError::config(format!(
                    "cswitch requires file-based Codex credentials; set cli_auth_credentials_store = \"file\" in {}",
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
        let paths = Paths::from_values(None, None, home).unwrap();
        assert_eq!(paths.backup_root, home.join(".cswitch"));
        assert_eq!(paths.codex_home, home.join(".codex"));
        assert_eq!(paths.live_auth_file(), home.join(".codex/auth.json"));

        let paths = Paths::from_values(
            Some(PathBuf::from("")),
            Some(PathBuf::from("/tmp/codex")),
            home,
        )
        .unwrap();
        assert_eq!(paths.backup_root, home.join(".cswitch"));
        assert_eq!(paths.codex_home, PathBuf::from("/tmp/codex"));

        let err = Paths::from_values(None, Some(PathBuf::from("/tmp/../x")), home).unwrap_err();
        assert!(err.to_string().contains(".."));
    }

    #[test]
    fn credential_store_gate() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_values(None, Some(dir.path().to_path_buf()), dir.path()).unwrap();
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
        let paths = Paths::from_values(Some(PathBuf::from("/s")), None, Path::new("/h")).unwrap();
        assert_eq!(
            paths.session_dir(2, "a@b.c"),
            PathBuf::from("/s/sessions/2-a_b.c")
        );
    }
}
