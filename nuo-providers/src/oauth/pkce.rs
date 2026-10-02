//! PKCE (RFC 7636) S256 code challenge generation and OAuth state/nonce values.
//!
//! Pure (no I/O): given a random verifier it derives the S256 challenge, and
//! it generates the random strings PKCE/state/nonce need. The randomness is
//! `rand::rngs::OsRng`-grade via [`uuid`]'s `Uuid::new_v4` plus a hex encoder,
//! which pulls from the OS CSPRNG.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nuo_contracts::SecretString;
use sha2::{Digest, Sha256};

/// The PKCE code verifier + matching S256 code challenge.
#[derive(Debug, Clone)]
pub struct PkceCodes {
    /// The verifier sent to the token endpoint in the exchange step. Kept
    /// secret on the client; never sent in the authorize URL.
    pub verifier: SecretString,
    /// `BASE64URL(SHA256(verifier))`, sent as `code_challenge` with
    /// `code_challenge_method=S256` in the authorize URL.
    pub challenge: String,
}

impl PkceCodes {
    /// Generate a fresh PKCE pair from OS randomness.
    pub fn generate() -> Self {
        let verifier = SecretString::from(random_string(64));
        let challenge = s256_challenge(verifier.expose_secret());
        Self {
            verifier,
            challenge,
        }
    }
}

/// The SHA-256 code challenge for a verifier, base64url-encoded (no padding).
/// Public so the device/browser flows and tests can reuse it.
pub fn s256_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// A high-entropy random string drawn from the unreserved URI characters
/// (`A-Za-z0-9-._~`), suitable for a PKCE verifier, `state`, or `nonce`.
/// Length is caller-chosen; 64 chars gives ~384 bits of entropy.
pub fn random_string(len: usize) -> String {
    // The legal PKCE verifier chars (RFC 7636 §4.1): unreserved from RFC 3986.
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    // Pull 16 random bytes per Uuid and index into CHARS modulo its length.
    // 43 (min verifier) .. 128 (max verifier) is the legal range; 64 is safe.
    let mut out = String::with_capacity(len);
    let mut remaining = len;
    while remaining > 0 {
        let id = uuid::Uuid::new_v4();
        let bytes = id.as_bytes();
        let take = remaining.min(bytes.len());
        for &b in bytes.iter().take(take) {
            // CHARS.len() is 66; b is 0..256 so `b % 66` is a uniform-enough
            // index over the legal set for a CSPRNG-sourced byte.
            let idx = usize::from(b) % CHARS.len();
            out.push(CHARS[idx] as char);
        }
        remaining = remaining.saturating_sub(take);
    }
    out
}

/// A fresh opaque `state` value for CSRF protection in the authorize step.
pub fn new_state() -> String {
    random_string(43)
}

/// A fresh `nonce` value (OpenID Connect). xAI's authorize endpoint accepts it.
pub fn new_nonce() -> String {
    random_string(32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_base64url_sha256_of_verifier() {
        // The challenge must be BASE64URL(SHA256(verifier)) with no padding,
        // per RFC 7636 §4.2. Verify against an independent computation.
        let verifier = "test-verifier-1234567890";
        let challenge = s256_challenge(verifier);

        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected = URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge, expected);
        // No padding chars.
        assert!(!challenge.contains('='));
    }

    #[test]
    fn generated_pair_is_consistent() {
        let pkce = PkceCodes::generate();
        assert_eq!(pkce.verifier.expose_secret().len(), 64);
        // Verifier chars are all legal.
        for c in pkce.verifier.expose_secret().chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' || c == '~',
                "illegal verifier char: {c}"
            );
        }
        // The challenge regenerates from the verifier.
        assert_eq!(
            pkce.challenge,
            s256_challenge(pkce.verifier.expose_secret())
        );
    }

    #[test]
    fn random_strings_are_unique() {
        // OS randomness: two draws must (practically) never collide.
        assert_ne!(random_string(64), random_string(64));
        assert_ne!(new_state(), new_state());
        assert_ne!(new_nonce(), new_nonce());
    }
}
