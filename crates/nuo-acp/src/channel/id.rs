use crate::error::{ProtocolError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Maximum length of a channel identifier.
pub const MAX_CHANNEL_ID_LEN: usize = 64;

/// A validated channel identifier, e.g. `ops`, `dev/frontend`, `incidents-p0`.
///
/// The naming rules are deliberately strict so that channel identity is
/// unambiguous across hosts, log files, config, and URLs:
///
/// - 1 to 64 characters.
/// - Lowercase ASCII letters, digits, `-`, `_`, and `/` as a hierarchy separator.
/// - Must begin with a letter or digit.
/// - No empty segments, so `a//b`, `/a`, and `a/` are all rejected.
///
/// Rejecting these at construction is the point: a channel name that cannot be
/// mistyped cannot silently create a second, empty channel.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ChannelId(String);

impl ChannelId {
    /// Validates and constructs a channel identifier.
    pub fn new(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();

        if raw.is_empty() {
            return Err(ProtocolError::InvalidChannelId(
                "channel id is empty".into(),
            ));
        }
        if raw.len() > MAX_CHANNEL_ID_LEN {
            return Err(ProtocolError::InvalidChannelId(format!(
                "channel id is {} characters, exceeding the {MAX_CHANNEL_ID_LEN} limit",
                raw.len()
            )));
        }
        if raw.starts_with('/') || raw.ends_with('/') {
            return Err(ProtocolError::InvalidChannelId(format!(
                "channel id `{raw}` must not begin or end with '/'"
            )));
        }
        if raw.contains("//") {
            return Err(ProtocolError::InvalidChannelId(format!(
                "channel id `{raw}` contains an empty segment"
            )));
        }

        for ch in raw.chars() {
            let allowed = ch.is_ascii_lowercase()
                || ch.is_ascii_digit()
                || ch == '-'
                || ch == '_'
                || ch == '/';
            if !allowed {
                return Err(ProtocolError::InvalidChannelId(format!(
                    "channel id `{raw}` contains `{ch}`; only lowercase letters, digits, \
                     '-', '_', and '/' are permitted"
                )));
            }
        }

        // Enforced separately from the loop above so the error names the rule
        // that actually failed, rather than reporting the character.
        let first = raw.chars().next().unwrap_or('/');
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err(ProtocolError::InvalidChannelId(format!(
                "channel id `{raw}` must begin with a letter or digit"
            )));
        }

        Ok(Self(raw))
    }

    /// The full identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parent channel path, e.g. `dev` for `dev/frontend`.
    pub fn parent(&self) -> Option<ChannelId> {
        self.0
            .rsplit_once('/')
            .map(|(parent, _)| Self(parent.to_string()))
    }

    /// Final segment, e.g. `frontend` for `dev/frontend`.
    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// Whether this id is nested inside `other`.
    pub fn is_within(&self, other: &ChannelId) -> bool {
        self.0
            .strip_prefix(other.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for ChannelId {
    type Err = ProtocolError;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl TryFrom<String> for ChannelId {
    type Error = ProtocolError;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<ChannelId> for String {
    fn from(id: ChannelId) -> Self {
        id.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_well_formed_ids() {
        for raw in [
            "ops",
            "dev/frontend",
            "incidents-p0",
            "team_a",
            "a1",
            "x/y/z",
        ] {
            assert!(ChannelId::new(raw).is_ok(), "`{raw}` should be valid");
        }
    }

    #[test]
    fn rejects_malformed_ids() {
        let cases = [
            ("", "empty"),
            ("/ops", "leading slash"),
            ("ops/", "trailing slash"),
            ("dev//frontend", "empty segment"),
            ("Ops", "uppercase"),
            ("dev/frontend ", "whitespace"),
            ("-ops", "leading dash"),
            ("_ops", "leading underscore"),
            ("café", "non-ascii"),
            ("a b", "space"),
        ];

        for (raw, why) in cases {
            assert!(
                ChannelId::new(raw).is_err(),
                "`{raw}` should be rejected ({why})"
            );
        }
    }

    #[test]
    fn rejects_overlong_ids() {
        let long = "a".repeat(MAX_CHANNEL_ID_LEN + 1);
        assert!(ChannelId::new(long).is_err());

        let at_limit = "a".repeat(MAX_CHANNEL_ID_LEN);
        assert!(ChannelId::new(at_limit).is_ok());
    }

    #[test]
    fn exposes_hierarchy() {
        let id = ChannelId::new("dev/frontend").unwrap();
        assert_eq!(id.name(), "frontend");
        assert_eq!(id.parent().unwrap().as_str(), "dev");

        let nested = ChannelId::new("a/b/c").unwrap();
        assert!(nested.is_within(&ChannelId::new("a").unwrap()));
        assert!(nested.is_within(&ChannelId::new("a/b").unwrap()));
        assert!(!nested.is_within(&ChannelId::new("b").unwrap()));
        assert!(!nested.is_within(&nested));
    }

    #[test]
    fn round_trips_through_serde() {
        let id = ChannelId::new("dev/frontend").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"dev/frontend\"");
        let back: ChannelId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);

        // Deserializing an invalid id must fail, not construct a bad value.
        assert!(serde_json::from_str::<ChannelId>("\"Bad/Id\"").is_err());
    }
}
