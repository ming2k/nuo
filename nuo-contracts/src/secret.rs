//! A small wrapper around a `String` that does not leak its contents
//! via [`Debug`] or [`std::fmt::Display`].
//!
//! Use this for API keys, bearer tokens, and any other credential
//! that may end up in a struct whose [`Debug`] output is logged.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A string whose [`Debug`] and [`std::fmt::Display`] implementations redact
/// the contents.
///
/// The underlying value is still reachable via [`Self::expose_secret`] — the
/// redaction only prevents accidental leaks through `{:?}` or `{}` formatting.
/// Serde is transparent: the value (de)serializes exactly like a plain
/// `String`, so on-disk shapes (`config.toml`, `credentials.toml`,
/// `auth.toml`) are unchanged and existing files need no migration.
#[derive(Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(transparent)]
// ts-rs cannot see through the redacting wrapper's custom Debug/Display, but
// the serde shape is exactly a plain JSON string — pin that explicitly.
#[ts(type = "string", export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct SecretString(String);

impl SecretString {
    /// Construct from any string-like input.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the underlying value as `&str`. This is the ONLY way to read the
    /// secret; callers are responsible for not logging the result or persisting
    /// it anywhere but the credential store it came from.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Returns `true` if the secret is the empty string.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Consume and return the underlying value.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Convenience for tests and assertions: compares against the underlying
/// value without an explicit [`SecretString::expose_secret`] call.
impl PartialEq<&str> for SecretString {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(***)")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_contents() {
        let s = SecretString::new("sk-secret-key");
        let dbg = format!("{s:?}");
        assert!(dbg.contains("SecretString"));
        assert!(!dbg.contains("sk-secret-key"));
    }

    #[test]
    fn display_redacts_contents() {
        let s = SecretString::new("sk-secret-key");
        assert_eq!(s.to_string(), "***");
    }

    #[test]
    fn expose_secret_returns_underlying() {
        let s = SecretString::new("sk-secret-key");
        assert_eq!(s.expose_secret(), "sk-secret-key");
        assert_eq!(s, "sk-secret-key");
    }

    #[test]
    fn serde_round_trips_through_json_as_plain_string() {
        let s = SecretString::new("sk-x");
        let json = serde_json::to_string(&s).expect("serialize");
        assert_eq!(json, r#""sk-x""#);
        let back: SecretString = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.expose_secret(), "sk-x");
    }

    #[test]
    fn serde_round_trips_through_toml_as_plain_string() {
        #[derive(Serialize, Deserialize)]
        struct Holder {
            api_key: Option<SecretString>,
        }
        let holder = Holder {
            api_key: Some(SecretString::new("sk-toml")),
        };
        let text = toml::to_string(&holder).expect("serialize");
        // Identical shape to a plain `Option<String>` field.
        assert_eq!(text, "api_key = \"sk-toml\"\n");
        let back: Holder = toml::from_str(&text).expect("deserialize");
        assert_eq!(
            back.api_key.as_ref().map(SecretString::expose_secret),
            Some("sk-toml")
        );
    }
}
