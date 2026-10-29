//! Content-capture eligibility: whether a path's bytes may be written into the
//! durable local store (ADR-0125 `INV-EFFECT-07`).
//!
//! ## The problem this closes
//!
//! The workspace journal captures pre-images so `/undo` can restore them. That is
//! only honest if the captured bytes are *allowed to persist*. A `.env` holding a
//! live API key, an `id_rsa`, a `.git-credentials` — copying any of these into a
//! content-addressed blob store creates a second, long-lived, unencrypted copy of a
//! credential that the user never asked to have copied, in a place with no
//! expiry of its own.
//!
//! [ADR-0032] settled this for nuo's *own* state (`[INV-SEC-01]`: zero plaintext keys
//! in the config directory). This is the same discipline applied to the journal: the
//! store may hold the user's source, and may not hold their credentials.
//!
//! [ADR-0032]: ../../docs/adr/0032-relocation-of-credentials-to-state-store-and-dotfiles-secret-leaks-prevention.md
//!
//! ## Why classification is by path and not by content
//!
//! Scanning bytes for key-shaped strings was considered and rejected. It would make
//! reversibility depend on a file's *contents at a particular moment*, which is
//! unpredictable in exactly the way a safety property must not be: the user would be
//! told "this file cannot be undone" and be unable to discover why, since a
//! three-character edit elsewhere could have removed the string that triggered it.
//! A path is stable, explainable, and something the user can act on. The asymmetry of
//! the failure modes settles the tie: over-classification costs an undo the user can
//! live without, under-classification copies a credential into durable storage.
//!
//! Content scanning still happens where it belongs — [`SecretScrubMiddleware`] over
//! tool *output*, so a secret echoed by a command is redacted before it reaches the
//! model or the transcript. That channel is about text in flight; this one is about
//! bytes at rest.
//!
//! [`SecretScrubMiddleware`]: ../../crates/nuo-harness/src/runtime/execution/middleware/scrub.rs

use std::path::Path;

/// Whether a path's content may be captured into the durable local store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureEligibility {
    /// The bytes are ordinary workspace content: capturing them is fine, and a
    /// reversal can restore them exactly.
    Eligible,
    /// The path conventionally holds credentials. Its bytes must not be copied into
    /// the local store, so the call is not reversible from a pre-image.
    Withheld,
}

impl CaptureEligibility {
    /// Whether the bytes may be written to the content-addressed store.
    pub fn permits_capture(self) -> bool {
        matches!(self, Self::Eligible)
    }

    /// The explanation carried into the journal row and shown by `/diff` and
    /// `/undo`. `None` when capture is permitted.
    ///
    /// A reason the user cannot understand is a reason they cannot act on, so it
    /// names the file's role rather than the rule that matched it.
    pub fn withheld_reason(self) -> Option<&'static str> {
        match self {
            Self::Eligible => None,
            Self::Withheld => Some(
                "this file conventionally holds credentials, so its content is not copied \
                 into the local store; the change is recorded but cannot be undone",
            ),
        }
    }
}

/// Filenames whose content is credentials wherever they appear.
///
/// Matched on the final path component, case-sensitively (these names are
/// conventional and case-sensitive on the platforms that define them).
const SECRET_FILENAMES: &[&str] = &[
    ".netrc",
    "_netrc",
    ".git-credentials",
    ".pypirc",
    ".npmrc",
    ".dockercfg",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "credentials",
    "credentials.toml",
    "secrets.toml",
    "auth.toml",
    "master.key",
];

/// Filename suffixes whose content is credentials.
const SECRET_SUFFIXES: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".jks",
    ".keystore",
    ".secret",
    "_rsa",
    "_dsa",
    "_ed25519",
];

/// Directory components that make **every** file beneath them credentials.
///
/// `.ssh` holds private keys under names that carry no suffix; `.aws` holds
/// `credentials` and `config` (the latter is not a secret by name, but sitting in
/// `.aws` it is). A path-component match is the only correct test here: a file named
/// `config` is ordinary content in `src/` and a credential in `.aws/`.
const SECRET_DIRECTORIES: &[&str] = &[".ssh", ".aws", ".gnupg", ".docker", ".kube"];

/// `.env`-family carve-out: these are committed templates, not secrets.
///
/// Checked before the `.env.*` rule, because `.env.example` *is* meant to be read,
/// committed, and edited — classifying it as a secret would make the one file in the
/// family a user legitimately rewrites the one file that cannot be undone.
const ENV_TEMPLATE_SUFFIXES: &[&str] = &["example", "sample", "template", "dist", "tpl"];

/// Classify a path for content capture.
///
/// Deliberately total and side-effect free: no filesystem access, no configuration,
/// no ambient state. The same path yields the same answer everywhere, which is what
/// lets the harness, the journal, and a future inspection surface agree without a
/// shared mutable registry.
pub fn classify(path: &Path) -> CaptureEligibility {
    // A directory component that makes the whole subtree credentials wins first, so
    // `.ssh/config` is withheld even though `config` alone is ordinary content.
    for component in path.components() {
        let Some(name) = component.as_os_str().to_str() else {
            continue;
        };
        if SECRET_DIRECTORIES.contains(&name) {
            return CaptureEligibility::Withheld;
        }
    }

    let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
        return CaptureEligibility::Eligible;
    };

    // `.env` and `.env.<something>` — except the committed templates.
    if filename == ".env" {
        return CaptureEligibility::Withheld;
    }
    if let Some(rest) = filename.strip_prefix(".env.")
        && !ENV_TEMPLATE_SUFFIXES.contains(&rest)
    {
        return CaptureEligibility::Withheld;
    }

    if SECRET_FILENAMES.contains(&filename) {
        return CaptureEligibility::Withheld;
    }
    if SECRET_SUFFIXES
        .iter()
        .any(|suffix| filename.ends_with(suffix))
    {
        return CaptureEligibility::Withheld;
    }

    CaptureEligibility::Eligible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn withheld(path: &str) -> bool {
        classify(Path::new(path)) == CaptureEligibility::Withheld
    }

    #[test]
    fn ordinary_source_is_eligible() {
        for path in [
            "src/lib.rs",
            "README.md",
            "Cargo.toml",
            "nested/deeply/tree/file.py",
            "config/settings.json",
        ] {
            assert!(!withheld(path), "{path} must be capturable");
        }
    }

    #[test]
    fn env_files_are_withheld_but_their_templates_are_not() {
        for path in [".env", ".env.local", ".env.production", "app/.env"] {
            assert!(withheld(path), "{path} holds credentials");
        }
        // Templates are committed on purpose and contain no secrets; making them
        // un-undoable would be gratuitous.
        for path in [
            ".env.example",
            ".env.sample",
            ".env.template",
            ".env.dist",
            ".env.tpl",
        ] {
            assert!(!withheld(path), "{path} is a template, not a secret");
        }
    }

    #[test]
    fn key_material_is_withheld_by_suffix_and_name() {
        for path in [
            "certs/server.pem",
            "deploy/id_rsa",
            "deploy/id_ed25519",
            "signing.key",
            "bundle.p12",
            "app.pfx",
            "store.jks",
        ] {
            assert!(withheld(path), "{path} is key material");
        }
    }

    #[test]
    fn credential_files_are_withheld() {
        for path in [
            ".netrc",
            ".git-credentials",
            ".pypirc",
            ".npmrc",
            "credentials",
            "secrets.toml",
        ] {
            assert!(withheld(path), "{path} holds credentials");
        }
    }

    /// A bare `config` is ordinary content; the same name inside `.aws` is a
    /// credential. Only a whole-component directory test gets this right.
    #[test]
    fn directory_components_decide_by_context() {
        assert!(!withheld("src/config"));
        assert!(withheld(".aws/config"));
        assert!(withheld(".aws/credentials"));
        assert!(withheld(".ssh/config"));
        assert!(withheld(".ssh/anything_at_all"));
        assert!(withheld("home/user/.kube/config"));
        assert!(withheld(".gnupg/gpg.conf"));
    }

    #[test]
    fn reason_is_present_exactly_when_capture_is_withheld() {
        assert_eq!(CaptureEligibility::Eligible.withheld_reason(), None);
        let reason = CaptureEligibility::Withheld
            .withheld_reason()
            .expect("withholding must be explained");
        // The reason names the file's role, not the rule, so a user can act on it.
        assert!(reason.contains("credentials"), "got: {reason}");
        assert!(CaptureEligibility::Eligible.permits_capture());
        assert!(!CaptureEligibility::Withheld.permits_capture());
    }

    /// Paths with no filename (a filesystem root) are not credentials by name.
    #[test]
    fn root_path_is_eligible() {
        assert_eq!(classify(Path::new("/")), CaptureEligibility::Eligible);
    }
}
