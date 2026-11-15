//! Effect taxonomy: what a tool *did* to the world, and whether it can be undone.
//!
//! [`RiskProfile`](crate::RiskProfile) answers a *pre-execution* question — "does
//! this call need a human's authority?" — and therefore drives the permission
//! chain. It is the wrong axis for the post-execution question a workspace
//! journal asks: "can this call's effect be reversed?" The two are genuinely
//! orthogonal and conflating them produces a lie in one direction or the other:
//!
//! - `write_file` declares [`RiskProfile::IdempotentMutation`](crate::risk::RiskProfile::IdempotentMutation) because
//!   re-writing the same bytes is harmless — yet it *truncates* whatever was
//!   there, so it is not idempotent with respect to history, only with respect
//!   to the final state.
//! - `execute_command` declares [`RiskProfile::ArbitraryExecution`](crate::risk::RiskProfile::ArbitraryExecution) and is
//!   irreversible, but so is `read_url`, which is not high-risk at all.
//!
//! [`EffectClass`] is the reversibility axis. It is a **closed** enum (not a
//! string, not a set of booleans) so every new tool must pick a class, and every
//! consumer that switches on it must handle every class. The journal's honesty
//! depends entirely on this type being impossible to leave unset.
//!
//! ## The ring: `None → FileBytes → …` and the join
//!
//! Classes form a lattice under "irreversibility severity":
//!
//! ```text
//! None(0) < FileBytes(1) < ExternalEffect(2) < ProcessEffect(3) < Opaque(4)
//! ```
//!
//! [`EffectClass::join`] takes the more severe of two classes. A tool whose
//! declared accesses claim a file write while its risk profile says "arbitrary
//! execution" joins to `Opaque` — the weaker claim can never launder the
//! stronger one. This is what makes [`Tool::effect_class_for`] safe to derive:
//! the derivation can only ever *promote* severity, never demote it.
//!
//! [`Tool::effect_class_for`]: crate::Tool::effect_class_for
//!
//! ## Fail-closed derivation
//!
//! A tool that mutates ([`RiskProfile::IdempotentMutation`](crate::risk::RiskProfile::IdempotentMutation)) but declares no
//! locatable mutating [`ToolAccesses`] is classified
//! `Opaque`, **not** `FileBytes`: the journal cannot capture a pre-image for a
//! path it was never told about, and silently recording "nothing happened" would
//! be a lie the user only discovers when `/undo` fails. Fail closed: an
//! unlocatable mutation is an irreversible one.

use serde::{Deserialize, Serialize};

use crate::access::ToolAccesses;

/// What a tool invocation did to state outside the conversation, and whether the
/// workspace journal can reverse it.
///
/// Ordered by irreversibility severity so [`EffectClass::join`] can compose two
/// classifications from one call (a tool's ceiling joined with what its declared
/// accesses actually implicate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    /// No state outside the conversation changed. Reads, queries, todo updates,
    /// and other conversation-scoped mutations. Reversal is a non-question.
    None,

    /// Mutated bytes under a declared, locatable filesystem path. Reversible
    /// **iff** the caller captured a pre-image and no third party has since
    /// rewritten the path: the journal records `before`/`after` content hashes
    /// and refuses to write over an observation that no longer holds.
    FileBytes,

    /// Changed state in a system outside this process and this filesystem:
    /// an HTTP endpoint, a search index, an MCP server, a third-party API. The
    /// journal can record *that it happened* and with what arguments, but the
    /// world moved and no local pre-image can move it back.
    ExternalEffect,

    /// Started, signalled, or reaped an operating-system process. Even a
    /// perfectly recorded command is unrepeatable in reverse: killed processes
    /// do not resume, and the effects of a completed process are whatever the
    /// process did — see [`EffectClass::Opaque`].
    ProcessEffect,

    /// Arbitrary execution or an unlocatable mutation: the journal cannot know
    /// what changed, so it cannot promise to undo it. A shell command may write
    /// files, mutate a database, publish a package, or all three, and the
    /// harness cannot tell which from the command text.
    ///
    /// This is the honest floor, not a gap to be closed by heuristics. The
    /// bash-policy module already documents why command-string analysis cannot
    /// be a capability boundary; guessing a shell command's file effects from
    /// its text would re-introduce exactly that failure mode in a place where
    /// the user is told their work is recoverable.
    Opaque,
}

impl EffectClass {
    /// Whether an effect of this class can be reversed by writing recorded
    /// content back to disk. Only [`EffectClass::FileBytes`] can; everything
    /// else is recorded-but-irreversible.
    #[inline]
    pub fn is_reversible(self) -> bool {
        matches!(self, Self::FileBytes | Self::None)
    }

    /// Whether the journal must capture pre-image content for this class. True
    /// only for [`EffectClass::FileBytes`]: capturing bytes for a class the
    /// journal cannot restore would cost storage and buy nothing.
    #[inline]
    pub fn requires_capture(self) -> bool {
        matches!(self, Self::FileBytes)
    }

    /// Whether this class denotes *any* change outside the conversation. Used by
    /// the journal to skip pure-read calls instead of writing a row per read.
    #[inline]
    pub fn is_effectful(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Compose two classifications: the result is the more irreversible of the
    /// two. Never demotes, so a derivation that combines a tool's ceiling with
    /// its declared accesses cannot accidentally claim reversibility.
    #[inline]
    pub fn join(self, other: Self) -> Self {
        if self >= other { self } else { other }
    }

    /// Short, stable token for the durable journal column.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::FileBytes => "file_bytes",
            Self::ExternalEffect => "external_effect",
            Self::ProcessEffect => "process_effect",
            Self::Opaque => "opaque",
        }
    }

    /// Parse the durable token. Returns `None` for an unknown value, which the
    /// caller must treat as [`EffectClass::Opaque`] — an unreadable class is a
    /// class this build cannot promise to reverse.
    pub fn from_str_token(token: &str) -> Option<Self> {
        match token {
            "none" => Some(Self::None),
            "file_bytes" => Some(Self::FileBytes),
            "external_effect" => Some(Self::ExternalEffect),
            "process_effect" => Some(Self::ProcessEffect),
            "opaque" => Some(Self::Opaque),
            _ => None,
        }
    }

    /// User-facing label for the diff/undo surfaces.
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "no external effect",
            Self::FileBytes => "file changes",
            Self::ExternalEffect => "external effect",
            Self::ProcessEffect => "process effect",
            Self::Opaque => "opaque effect",
        }
    }

    /// Why this class cannot be reversed, for the `/undo` refusal body. `None`
    /// for the reversible classes.
    pub const fn irreversibility_reason(self) -> Option<&'static str> {
        match self {
            Self::None | Self::FileBytes => None,
            Self::ExternalEffect => {
                Some("it changed state in an external system; no local pre-image can move it back")
            }
            Self::ProcessEffect => {
                Some("it started or signalled an operating-system process; processes do not resume")
            }
            Self::Opaque => Some(
                "it ran with arbitrary execution authority, so what it changed cannot be determined",
            ),
        }
    }
}

/// Reconcile a tool's declared ceiling with its declared resource accesses.
///
/// Only two transitions are possible, and both promote severity:
///
/// 1. A [`FileBytes`](EffectClass::FileBytes) ceiling with **no** locatable
///    mutating access becomes [`Opaque`](EffectClass::Opaque). The journal
///    records pre-images per path; a mutating tool that names no path cannot
///    have one captured, so claiming reversibility would produce an `/undo`
///    that silently does nothing.
/// 2. Any ceiling below [`Opaque`](EffectClass::Opaque) that declares a
///    mutating access is *at least* `FileBytes`. A tool that writes files while
///    describing itself as read-only or network-only is under-declaring; the
///    observation wins over the self-description.
///
/// Everything else is returned unchanged, so [`EffectClass::Opaque`] and
/// [`EffectClass::ProcessEffect`] can never be demoted by an access list.
pub fn reconciling(ceiling: EffectClass, accesses: &ToolAccesses) -> EffectClass {
    let writes = accesses.declares_write();
    match ceiling {
        EffectClass::FileBytes if !writes => EffectClass::Opaque,
        EffectClass::None if writes => EffectClass::FileBytes,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_never_demotes_severity() {
        // A file-write claim can never launder an arbitrary-execution fact.
        assert_eq!(
            EffectClass::FileBytes.join(EffectClass::Opaque),
            EffectClass::Opaque
        );
        assert_eq!(
            EffectClass::Opaque.join(EffectClass::FileBytes),
            EffectClass::Opaque
        );
        assert_eq!(
            EffectClass::None.join(EffectClass::FileBytes),
            EffectClass::FileBytes
        );
    }

    #[test]
    fn reversibility_is_exactly_file_bytes_and_none() {
        assert!(EffectClass::None.is_reversible());
        assert!(EffectClass::FileBytes.is_reversible());
        for class in [
            EffectClass::ExternalEffect,
            EffectClass::ProcessEffect,
            EffectClass::Opaque,
        ] {
            assert!(!class.is_reversible(), "{class:?} must not claim reversal");
            assert!(
                class.irreversibility_reason().is_some(),
                "{class:?} must explain its refusal"
            );
        }
    }

    #[test]
    fn only_file_bytes_requires_capture() {
        assert!(EffectClass::FileBytes.requires_capture());
        for class in [
            EffectClass::None,
            EffectClass::ExternalEffect,
            EffectClass::ProcessEffect,
            EffectClass::Opaque,
        ] {
            assert!(!class.requires_capture(), "{class:?}");
        }
    }

    #[test]
    fn durable_token_round_trips() {
        for class in [
            EffectClass::None,
            EffectClass::FileBytes,
            EffectClass::ExternalEffect,
            EffectClass::ProcessEffect,
            EffectClass::Opaque,
        ] {
            assert_eq!(EffectClass::from_str_token(class.as_str()), Some(class));
        }
        // An unknown token must not silently become reversible.
        assert_eq!(EffectClass::from_str_token("something_new"), None);
    }

    #[test]
    fn severity_order_is_irreversibility_order() {
        assert!(EffectClass::None < EffectClass::FileBytes);
        assert!(EffectClass::FileBytes < EffectClass::ExternalEffect);
        assert!(EffectClass::ExternalEffect < EffectClass::ProcessEffect);
        assert!(EffectClass::ProcessEffect < EffectClass::Opaque);
    }

    #[test]
    fn unlocatable_mutation_fails_closed_to_opaque() {
        // A file-mutating ceiling with no declared write path cannot capture a
        // pre-image, so it must not claim file reversibility.
        assert_eq!(
            reconciling(EffectClass::FileBytes, &ToolAccesses::none()),
            EffectClass::Opaque
        );
    }

    #[test]
    fn declared_write_promotes_readonly_ceiling() {
        // Under-declaring risk cannot hide an observed write.
        assert_eq!(
            reconciling(
                EffectClass::None,
                &ToolAccesses::write_file("src/lib.rs".to_string())
            ),
            EffectClass::FileBytes
        );
    }

    #[test]
    fn reconciliation_never_demotes_strong_classes() {
        let writes = ToolAccesses::write_file("src/lib.rs".to_string());
        for ceiling in [
            EffectClass::ExternalEffect,
            EffectClass::ProcessEffect,
            EffectClass::Opaque,
        ] {
            assert_eq!(
                reconciling(ceiling, &writes),
                ceiling,
                "{ceiling:?} must not be demoted by an access declaration"
            );
        }
    }

    #[test]
    fn declared_write_keeps_file_bytes() {
        assert_eq!(
            reconciling(
                EffectClass::FileBytes,
                &ToolAccesses::read_write_file("src/lib.rs".to_string())
            ),
            EffectClass::FileBytes
        );
    }
}
