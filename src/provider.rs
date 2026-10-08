//! The two account providers cswitch manages and the selector grammar.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Codex,
    Claude,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Codex, Provider::Claude];

    /// The roster / JSON / selector spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    /// Capitalized, for headings such as `Codex accounts:`.
    pub fn title(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
        }
    }

    /// The product whose login this provider manages.
    pub fn tool_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }

    /// `codex` / `claude` (any case) as a command-line selector; anything
    /// else is an account identifier.
    pub fn parse_selector(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            _ => None,
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_and_strings() {
        assert_eq!(serde_json::to_value(Provider::Codex).unwrap(), "codex");
        assert_eq!(serde_json::to_value(Provider::Claude).unwrap(), "claude");
        let back: Provider = serde_json::from_str("\"claude\"").unwrap();
        assert_eq!(back, Provider::Claude);
        assert_eq!(Provider::default(), Provider::Codex);
        assert_eq!(Provider::Claude.as_str(), "claude");
        assert_eq!(Provider::Codex.title(), "Codex");
        assert_eq!(Provider::Claude.tool_name(), "Claude Code");
        assert_eq!(Provider::Codex.to_string(), "codex");
        assert_eq!(Provider::ALL, [Provider::Codex, Provider::Claude]);
    }

    #[test]
    fn selector_parsing_is_case_insensitive_and_strict() {
        assert_eq!(Provider::parse_selector("codex"), Some(Provider::Codex));
        assert_eq!(Provider::parse_selector("Claude"), Some(Provider::Claude));
        assert_eq!(Provider::parse_selector("claude "), None);
        assert_eq!(Provider::parse_selector("2"), None);
        assert_eq!(Provider::parse_selector("a@b.co"), None);
    }
}
