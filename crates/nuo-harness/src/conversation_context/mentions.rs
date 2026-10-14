//! Canonical address rewriting for the **request projection** (ADR-0288).
//!
//! The extractors under this module resolve `@`-references on the durable side
//! and emit canonical envelopes carrying each address verbatim. This file owns
//! the other half of that contract: the visible prompt the provider receives
//! must not spell one asset two ways, and a backslash escape that suppressed
//! resolution must be consumed.
//!
//! It is a *rewriting* policy over the shared grammar kernel
//! ([`nuo_wire::mention`]): the kernel recognizes and segments
//! references; this module decides what an accepted reference becomes in the
//! request view. No scanning logic lives here.
//!
//! Scope discipline: only **qualified** namespaces are rewritten. A bare
//! `@name` (kernel [`Form::Bare`]) is deliberately left untouched — without a
//! registry lookup it is indistinguishable from a username or an email
//! fragment, and rewriting it would corrupt ordinary prose (ADR-0288 rejected
//! alternatives).

use nuo_wire::mention::{Form, Namespace, scan_references};

/// Whether a message's content is a place where `@`-references are written by
/// the user and therefore subject to canonicalization (`[INV-REF-03]`).
///
/// Only **genuine user input** qualifies. Every harness-injected message
/// (`origin.is_some()`, which also covers the hidden `<file>` / `<skill>`
/// envelopes) is *resolved asset content or a harness note*, not a reference
/// site: rewriting it would corrupt the asset (a referenced file whose text
/// legitimately contains `@files:` or `\@file:` must reach the model verbatim).
/// Assistant output and tool results are likewise not reference sites.
///
/// The check is provenance-based, never content-sniffing: a genuine user
/// message carries no `origin`.
pub(crate) fn is_reference_site(message: &crate::Message) -> bool {
    message.role == crate::Role::User && !message.hidden && message.origin.is_none()
}

/// Rewrite every *qualified* `@`-reference in `text` to its canonical spelling
/// and consume any backslash escape that guarded it.
///
/// Canonicalizations (`[INV-REF-03]`):
/// - `@file:{path}` / `@files:{path}` → `@file:{path}`
/// - `@skill:{name}` / `@skills:{name}` → `@skill:{name}`
/// - `skill://{name}` (bare name, no `/`) → `@skill:{name}`
///
/// Escapes (`[INV-REF-04]`): `\@file:x` / `\@skill:x` lose the backslash and
/// become the canonical literal text. Masked code spans are copied verbatim —
/// a mention quoted in backticks is discussion, not a reference.
///
/// Trailing sentence punctuation (`.`), which the path alphabet admits into the
/// run, is preserved outside the canonical address.
pub(crate) fn canonicalize_addresses(text: &str) -> String {
    let references = scan_references(text);
    if references.is_empty() {
        return text.to_string();
    }
    // Nothing in this text is a rewrite target (all bare, or all source-path
    // URIs) — avoid an allocation and a full copy.
    if !references.iter().any(is_rewrite_target) {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for reference in &references {
        // A bare `@name` is left in place; skip it without touching the cursor
        // so the surrounding text is copied whole.
        let replacement = match canonical_of(reference) {
            Some(replacement) => replacement,
            None => continue,
        };
        // The escape backslash (one byte before the `@`) is consumed with it.
        let replace_from = if reference.escaped {
            reference.start - 1
        } else {
            reference.start
        };
        if replace_from < cursor {
            continue;
        }
        out.push_str(&text[cursor..replace_from]);
        out.push_str(&replacement);
        cursor = reference.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Whether a reference is a rewrite target (qualified, or a bare-name
/// `skill://`). Bare `@name` mentions are not.
fn is_rewrite_target(reference: &nuo_wire::mention::Reference<'_>) -> bool {
    match reference.form {
        Form::Qualified => true,
        Form::Uri => !reference.target.contains('/'),
        Form::Bare => false,
    }
}

/// The canonical replacement for a rewrite target, or `None` if the reference
/// stays as written.
fn canonical_of(reference: &nuo_wire::mention::Reference<'_>) -> Option<String> {
    match reference.namespace {
        Namespace::File => Some(format!("@file:{}", reference.target)),
        Namespace::Skill => match reference.form {
            Form::Qualified => Some(format!("@skill:{}", reference.target)),
            Form::Uri => {
                (!reference.target.contains('/')).then(|| format!("@skill:{}", reference.target))
            }
            Form::Bare => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_file_namespace_plural_and_singular() {
        assert_eq!(
            canonicalize_addresses("see @files:a.rs and @file:b.rs"),
            "see @file:a.rs and @file:b.rs"
        );
    }

    #[test]
    fn collapses_skill_namespace_and_uri() {
        assert_eq!(
            canonicalize_addresses("use @skills:pdf and @skill:rust and skill://lint"),
            "use @skill:pdf and @skill:rust and @skill:lint"
        );
    }

    #[test]
    fn preserves_trailing_sentence_period() {
        assert_eq!(
            canonicalize_addresses("see @file:src/lib.rs."),
            "see @file:src/lib.rs."
        );
    }

    #[test]
    fn consumes_backslash_escape() {
        // The escape suppressed resolution at L1; at L2 the address is inert
        // literal text, so the backslash has no remaining meaning.
        assert_eq!(
            canonicalize_addresses(r"talk about \@file:esc.rs here"),
            "talk about @file:esc.rs here"
        );
        assert_eq!(
            canonicalize_addresses(r"talk about \@skill:rust here"),
            "talk about @skill:rust here"
        );
    }

    #[test]
    fn leaves_masked_code_spans_verbatim() {
        assert_eq!(
            canonicalize_addresses("quoting `@files:x.rs` in backticks"),
            "quoting `@files:x.rs` in backticks"
        );
        let fenced = "```\n@files:x.rs\n```";
        assert_eq!(canonicalize_addresses(fenced), fenced);
    }

    #[test]
    fn leaves_non_boundary_and_foreign_at_tokens_alone() {
        // Email-style `user@file:x` is not a reference.
        assert_eq!(canonicalize_addresses("user@files:x.rs"), "user@files:x.rs");
        // A bare `@name` has no qualified namespace; it must survive untouched
        // (it could be a username, not a skill).
        assert_eq!(
            canonicalize_addresses("ping @some_user about @file:a.rs"),
            "ping @some_user about @file:a.rs"
        );
    }

    /// ADR-0288 `[INV-REF-03]` — the bug this guards: canonicalization must run
    /// only on genuine user input, never on harness-injected messages. A
    /// referenced file whose body legitimately contains `@files:` / `\@file:`
    /// (e.g. a doc about this feature) must reach the model verbatim; rewriting
    /// the envelope body would corrupt the asset.
    #[test]
    fn only_genuine_user_input_is_a_reference_site() {
        use crate::{InjectionKind, Message, Role};

        let genuine = Message::new(Role::User, "@files:a.rs");
        assert!(is_reference_site(&genuine));

        // Hidden file/skill envelope: harness-injected, must NOT be rewritten.
        let injected = crate::conversation_context::hidden_user_with_reason(
            InjectionKind::ImplicitFile,
            "a.rs",
            "<file path=\"a.rs\" ref=\"@file:a.rs\">\nsee @files:x and \\@file:y\n</file>",
        );
        assert!(!is_reference_site(&injected));

        // Assistant output and tool results are not reference sites either.
        assert!(!is_reference_site(&Message::new(Role::Assistant, "@files:a.rs")));
        assert!(!is_reference_site(&Message::new(
            Role::Tool,
            "@files:a.rs"
        )));

        // The correctness consequence: the gate is load-bearing. Called
        // directly, canonicalization WOULD rewrite the envelope body (proving
        // the risk is real); `is_reference_site` is what prevents the caller
        // from ever doing so to an injected message.
        assert_eq!(canonicalize_addresses("@files:x"), "@file:x");
        assert!(!is_reference_site(&injected));
        // A genuine user message with the same text IS rewritten.
        assert_eq!(canonicalize_addresses("@files:a.rs"), "@file:a.rs");
        assert!(is_reference_site(&Message::new(Role::User, "@files:a.rs")));
    }

    #[test]
    fn leaves_source_path_skill_uri_alone() {
        let input = "load skill://skills/rust/SKILL.md";
        assert_eq!(canonicalize_addresses(input), input);
    }

    #[test]
    fn is_idempotent() {
        let once = canonicalize_addresses("@files:a.rs @skills:b skill://c");
        assert_eq!(canonicalize_addresses(&once), once);
    }

    #[test]
    fn fast_path_leaves_plain_prose_untouched() {
        assert_eq!(canonicalize_addresses("just prose"), "just prose");
    }

    #[test]
    fn rewrites_multiple_and_preserves_prose_between() {
        assert_eq!(
            canonicalize_addresses("a @files:x.rs b @skills:y c"),
            "a @file:x.rs b @skill:y c"
        );
    }
}
