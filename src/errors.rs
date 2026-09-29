//! The error taxonomy shared by every command.
//!
//! The variant names double as the `type` string in the `--json` error
//! envelope, so they mirror cswap's exception class names. Exit status is 1 for
//! all of them; usage errors are reported by the CLI layer with status 2.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CswitchError {
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Switch(String),
    #[error("{0}")]
    Session(String),
    #[error("{0}")]
    Lock(String),
    #[error("{0}")]
    AccountNotFound(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Transfer(String),
    #[error("{0}")]
    CredentialRead(String),
    #[error("{0}")]
    CredentialWrite(String),
}

impl CswitchError {
    /// The JSON `error.type` string.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Config(_) => "ConfigError",
            Self::Switch(_) => "SwitchError",
            Self::Session(_) => "SessionError",
            Self::Lock(_) => "LockError",
            Self::AccountNotFound(_) => "AccountNotFoundError",
            Self::Validation(_) => "ValidationError",
            Self::Transfer(_) => "TransferError",
            Self::CredentialRead(_) => "CredentialReadError",
            Self::CredentialWrite(_) => "CredentialWriteError",
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }
    pub fn switch(message: impl Into<String>) -> Self {
        Self::Switch(message.into())
    }
    pub fn session(message: impl Into<String>) -> Self {
        Self::Session(message.into())
    }
    pub fn lock(message: impl Into<String>) -> Self {
        Self::Lock(message.into())
    }
    pub fn not_found(identifier: &str) -> Self {
        Self::AccountNotFound(format!("No account found with identifier: {identifier}"))
    }
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }
    pub fn transfer(message: impl Into<String>) -> Self {
        Self::Transfer(message.into())
    }
    pub fn credential_read(message: impl Into<String>) -> Self {
        Self::CredentialRead(message.into())
    }
    pub fn credential_write(message: impl Into<String>) -> Self {
        Self::CredentialWrite(message.into())
    }
}

pub type Result<T> = std::result::Result<T, CswitchError>;

#[cfg(test)]
mod tests {
    use super::CswitchError;

    #[test]
    fn type_names_match_the_json_contract() {
        assert_eq!(CswitchError::config("x").type_name(), "ConfigError");
        assert_eq!(
            CswitchError::not_found("2").type_name(),
            "AccountNotFoundError"
        );
        assert_eq!(
            CswitchError::not_found("dev").to_string(),
            "No account found with identifier: dev"
        );
    }
}
