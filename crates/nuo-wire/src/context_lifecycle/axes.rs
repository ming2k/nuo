//! The orthogonal lifecycle axes (ADR-0275 §3, `INV-FACT-04`).
//!
//! Context state is not one enum. Three axes are primary and independent
//! (validity, representation, retention); deletion and capture are two further
//! orthogonal states. They must never convert into one another implicitly:
//! `Purged` is not "out of the window", an interrupted capture is not "empty",
//! and reducing representation fidelity does not shorten retention.
//!
//! Representation is *not* modeled here as a fact attribute, because it belongs
//! to a [`crate::context_lifecycle::ContextView`]; this module models the
//! per-representation entry type that a view carries.

/// Whether a fact still proves the behavior of the current resource version
/// (ADR-0275 §6).
///
/// Validity is evaluated relative to a branch, resource versions, and the
/// purpose of the query. An older fact remains true; it simply stops proving
/// the current version. A validity change never rewrites the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Validity {
    /// The fact proves the referenced version as of the last evaluation.
    Current,
    /// A dependency version changed; the fact must be re-verified before use.
    NeedsRevalidation,
    /// A newer observation of the same scope and version supersedes this one.
    Superseded,
    /// Validity could not be established (e.g. an opaque command identity with
    /// no resolvable dependency).
    Unknown,
}

impl Validity {
    /// Whether this state permits treating the fact as proof of the current
    /// version without re-verification.
    pub const fn is_proof_of_current(self) -> bool {
        matches!(self, Validity::Current)
    }
}

/// How much of a fact a view exposes to the model (ADR-0275 §3).
///
/// A representation lives on a [`crate::context_lifecycle::ContextView`], not
/// on the fact. Lowering fidelity changes neither the raw material nor its
/// retention.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Representation {
    /// The complete payload is present.
    Full,
    /// A bounded excerpt plus exact omission information and a raw handle.
    Excerpt,
    /// A structured summary derived from the payload.
    Summary,
    /// Only a handle/identifier, resolvable on demand.
    Reference,
    /// Deliberately absent from this view.
    Omitted,
}

/// How long a fact or artifact is retained (ADR-0275 §3, ADR-0279 §4).
///
/// Retention is orthogonal to validity and representation; an expired
/// authorization attestation is not the same as expired content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    /// Discardable once the round ends; reconstructible, never user-visible
    /// evidence (the only class eligible for `DropEphemeral`).
    Ephemeral,
    /// Retained with the session (the default for raw observations).
    SessionBound,
    /// Retained until an explicit deadline.
    RetainedUntil {
        /// Epoch milliseconds after which the record may be collected.
        deadline_ms: u64,
    },
    /// Explicitly pinned; not collected until unpinned.
    Pinned,
}

impl Retention {
    /// Whether a record at this retention level is collectible at `now_ms`.
    ///
    /// `Ephemeral` is collected only through `DropEphemeral`; `Pinned` is never
    /// collected by time; `SessionBound` lives with the session.
    pub const fn is_time_collectible(self, now_ms: u64) -> bool {
        match self {
            Retention::Ephemeral | Retention::SessionBound | Retention::Pinned => false,
            Retention::RetainedUntil { deadline_ms } => now_ms >= deadline_ms,
        }
    }
}

/// Deletion state, independent of window presence (ADR-0275 §3, ADR-0279 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Deletion {
    /// Present and readable.
    Present,
    /// Deletion committed; content reclamation is pending or in progress.
    DeletePending,
    /// Payload and derived copies reclaimed; only a tombstone remains.
    Purged,
}

impl Deletion {
    /// Whether the content is readable. `DeletePending` and `Purged` are both
    /// reported as unreadable, and neither is ever presented as "outside the
    /// model window".
    pub const fn is_readable(self) -> bool {
        matches!(self, Deletion::Present)
    }
}

/// Completeness of a captured raw stream (ADR-0276 §1–§2).
///
/// Separate from execution and availability status; a handle existing does not
/// imply a complete capture.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capture {
    /// The full stream was captured and durably published.
    Complete,
    /// Capture stopped before the stream ended (process killed, cancelled).
    Interrupted {
        /// Why capture stopped.
        reason: String,
    },
    /// A quota or byte ceiling was reached; a prefix was retained.
    Truncated {
        /// Bytes retained before the stop.
        retained_bytes: u64,
        /// Why the stop happened.
        reason: String,
    },
    /// Nothing could be captured for this execution.
    Unavailable {
        /// Why the capture is unavailable.
        reason: String,
    },
}

impl Capture {
    /// Whether the capture is a faithful, complete record.
    pub const fn is_complete(&self) -> bool {
        matches!(self, Capture::Complete)
    }
}

/// Provenance authority attached to a fact payload (ADR-0275 §2).
///
/// A derived summary never gains higher authority than its sources; provider
/// lowering preserves these boundaries so an untrusted tool observation cannot
/// be laundered into a user instruction.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SourceAuthority {
    /// A user-authored requirement or message.
    User,
    /// A project/workspace instruction file.
    ProjectInstruction,
    /// Raw output observed by executing a tool.
    ToolObservation,
    /// A model-produced inference (never a fact by itself).
    AssistantInference,
    /// A summary or checkpoint derived from other facts.
    Derived,
}

impl SourceAuthority {
    /// The trust rank used to reject authority escalation. A derived record may
    /// never outrank its strongest source (ADR-0275 `INV-FACT-05`).
    pub const fn rank(self) -> u8 {
        match self {
            SourceAuthority::User => 4,
            SourceAuthority::ProjectInstruction => 3,
            SourceAuthority::ToolObservation => 2,
            SourceAuthority::AssistantInference => 1,
            SourceAuthority::Derived => 0,
        }
    }
}

/// Sensitivity class of a fact payload or derived record (ADR-0275 §8).
///
/// Drives scrubbing and authorization: a raw artifact always carries at least
/// the sensitivity of any preview derived from it, and a purge must reach every
/// derived copy.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// Safe to display and log.
    Public,
    /// Internal but not secret.
    Internal,
    /// Secret-bearing; scrubbed from model-visible previews and telemetry.
    Secret,
}

impl Sensitivity {
    /// The stricter of two classes (used when deriving a record from sources).
    pub const fn max(self, other: Sensitivity) -> Sensitivity {
        if (self as u8) >= (other as u8) {
            self
        } else {
            other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleted_content_is_never_reported_as_window_absence() {
        // INV-FACT-04: `Purged` is unreadable; it is never conflated with a
        // representation omission.
        assert!(!Deletion::Purged.is_readable());
        assert!(!Deletion::DeletePending.is_readable());
        assert!(Deletion::Present.is_readable());
    }

    #[test]
    fn retention_time_collection_is_explicit() {
        let expired = Retention::RetainedUntil { deadline_ms: 100 };
        let live = Retention::RetainedUntil { deadline_ms: 100 };
        assert!(expired.is_time_collectible(101), "past the deadline");
        assert!(live.is_time_collectible(100), "collectible at the deadline");
        assert!(!live.is_time_collectible(99), "not before the deadline");
        assert!(!Retention::Pinned.is_time_collectible(u64::MAX));
        assert!(!Retention::SessionBound.is_time_collectible(u64::MAX));
        // An ephemeral record is only removed by DropEphemeral, not by time.
        assert!(!Retention::Ephemeral.is_time_collectible(u64::MAX));
    }

    #[test]
    fn capture_completeness_is_explicit_for_every_noncomplete_case() {
        assert!(Capture::Complete.is_complete());
        assert!(
            !Capture::Interrupted {
                reason: "kill".into()
            }
            .is_complete()
        );
        assert!(
            !Capture::Truncated {
                retained_bytes: 10,
                reason: "quota".into()
            }
            .is_complete()
        );
        assert!(
            !Capture::Unavailable {
                reason: "spawn".into()
            }
            .is_complete()
        );
    }

    #[test]
    fn authority_rank_forbids_escalation() {
        // A derived record's rank is below a tool observation, so it cannot be
        // promoted above it.
        assert!(SourceAuthority::Derived.rank() < SourceAuthority::ToolObservation.rank());
        assert!(
            SourceAuthority::ToolObservation.rank() < SourceAuthority::ProjectInstruction.rank()
        );
        assert!(SourceAuthority::ProjectInstruction.rank() < SourceAuthority::User.rank());
    }

    #[test]
    fn sensitivity_derivation_takes_the_stricter_class() {
        assert_eq!(
            Sensitivity::Public.max(Sensitivity::Secret),
            Sensitivity::Secret
        );
        assert_eq!(
            Sensitivity::Secret.max(Sensitivity::Internal),
            Sensitivity::Secret
        );
        assert_eq!(
            Sensitivity::Internal.max(Sensitivity::Public),
            Sensitivity::Internal
        );
    }

    #[test]
    fn validity_states_are_not_retention_states() {
        // Orthogonality smoke test: the two enums share no variant names and
        // each answers a different question.
        assert!(Validity::Current.is_proof_of_current());
        assert!(!Validity::NeedsRevalidation.is_proof_of_current());
        // A fact can be Current while RetainedUntil expires — the axes must be
        // queried independently.
        assert!(Validity::Current.is_proof_of_current());
        assert!(Retention::RetainedUntil { deadline_ms: 0 }.is_time_collectible(0));
    }
}
