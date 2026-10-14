use crate::error::{ProtocolError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Strongly typed agent URI address (`agent://{authority}/{path}`).
///
/// Examples:
/// - `agent://local/dev-agent`
/// - `agent://system/desktop/issue-tracker`
/// - `agent://cluster-1/analytics/market-research`
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentAddress {
    raw: String,
    authority: String,
    path: String,
}

impl AgentAddress {
    pub const SCHEME: &'static str = "agent";
    pub const SCHEME_ACP: &'static str = "acp";

    /// Creates a new `AgentAddress` from authority and path segments.
    pub fn new(authority: impl Into<String>, path: impl Into<String>) -> Result<Self> {
        let auth = authority.into().trim().to_lowercase();
        let p = path.into().trim().trim_start_matches('/').to_string();

        if auth.is_empty() {
            return Err(ProtocolError::InvalidAddress(
                "agent authority cannot be empty".into(),
            ));
        }
        if p.is_empty() {
            return Err(ProtocolError::InvalidAddress(
                "agent path cannot be empty".into(),
            ));
        }

        let raw = format!("{}://{}/{}", Self::SCHEME, auth, p);
        Ok(Self {
            raw,
            authority: auth,
            path: p,
        })
    }

    /// Parses an `agent://...` or `acp://...` URI string.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let remainder = if let Some(rest) = s.strip_prefix("agent://") {
            rest
        } else if let Some(rest) = s.strip_prefix("acp://") {
            rest
        } else {
            return Err(ProtocolError::InvalidAddress(format!(
                "address must start with `agent://` or `acp://`, got `{s}`"
            )));
        };

        let (auth, path) = match remainder.split_once('/') {
            Some((a, p)) => (a, p),
            None => {
                return Err(ProtocolError::InvalidAddress(format!(
                    "missing path component in `{s}`"
                )));
            }
        };

        Self::new(auth, path)
    }

    pub fn authority(&self) -> &str {
        &self.authority
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Matches address against an address selector pattern.
    pub fn matches(&self, selector: &AddressSelector) -> bool {
        selector.matches(self)
    }
}

impl fmt::Display for AgentAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

impl FromStr for AgentAddress {
    type Err = ProtocolError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl TryFrom<String> for AgentAddress {
    type Error = ProtocolError;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl From<AgentAddress> for String {
    fn from(addr: AgentAddress) -> Self {
        addr.raw
    }
}

/// Selector for pattern-based recipient matching.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AddressSelector {
    /// Exact match on specific address
    Exact(AgentAddress),
    /// Authority match: matches any agent on authority (e.g. `agent://local/*`)
    Authority(String),
    /// Wildcard prefix match: e.g. `agent://local/dev/*`
    Prefix(String),
    /// Match everything
    Any,
}

impl AddressSelector {
    pub fn matches(&self, address: &AgentAddress) -> bool {
        match self {
            Self::Exact(target) => target == address,
            Self::Authority(auth) => address.authority().eq_ignore_ascii_case(auth),
            Self::Prefix(prefix) => address.as_str().starts_with(prefix),
            Self::Any => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_address_parsing() {
        let addr = AgentAddress::parse("agent://local/dev-agent").unwrap();
        assert_eq!(addr.authority(), "local");
        assert_eq!(addr.path(), "dev-agent");
        assert_eq!(addr.as_str(), "agent://local/dev-agent");

        let addr2: AgentAddress = "agent://system/desktop/issue-tracker".parse().unwrap();
        assert_eq!(addr2.authority(), "system");
        assert_eq!(addr2.path(), "desktop/issue-tracker");
    }

    #[test]
    fn test_invalid_address() {
        assert!(AgentAddress::parse("http://local/agent").is_err());
        assert!(AgentAddress::parse("agent://local").is_err());
        assert!(AgentAddress::parse("agent:///path").is_err());
    }

    #[test]
    fn test_selector_matching() {
        let addr = AgentAddress::parse("agent://local/dev/frontend").unwrap();

        assert!(AddressSelector::Exact(addr.clone()).matches(&addr));
        assert!(AddressSelector::Authority("local".into()).matches(&addr));
        assert!(!AddressSelector::Authority("remote".into()).matches(&addr));
        assert!(AddressSelector::Prefix("agent://local/dev/".into()).matches(&addr));
        assert!(AddressSelector::Any.matches(&addr));
    }
}
