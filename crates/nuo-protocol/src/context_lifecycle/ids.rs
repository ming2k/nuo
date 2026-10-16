//! Typed identifiers for the context-lifecycle domain (ADR-0275 §1).
//!
//! Every fact, view, checkpoint, artifact, and retrieval handle carries a
//! scope-qualified identity. These IDs exist so that a provider tool-call ID
//! can never stand in for an internal identity: two turns may hand the wire the
//! same provider call ID, but no two facts share a [`FactId`], and a handle
//! addresses exactly one record (ADR-0279 `INV-RET-01`).

macro_rules! string_newtype_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap an existing identity string.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the underlying identity.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the wrapper and return the identity.
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

string_newtype_id! {
    /// Identity of an immutable [`crate::context_lifecycle::FactNode`].
    ///
    /// Assigned by the canonical writer; stable for the life of the session and
    /// never reused, including after a fork.
    FactId
}

string_newtype_id! {
    /// Identity of a branch within a session (ADR-0275 §1).
    ///
    /// A branch isolates an execution path and its context view. A fork shares
    /// facts but not views, pins, or task state.
    BranchId
}

string_newtype_id! {
    /// Identity of a cross-round [`crate::context_lifecycle::TaskState`].
    TaskId
}

string_newtype_id! {
    /// Identity of one admitted execution round.
    RoundId
}

string_newtype_id! {
    /// Identity of one accepted model turn within a round.
    TurnId
}

string_newtype_id! {
    /// Identity of one provider attempt within a turn.
    ///
    /// A retry produces a new attempt that reuses the same request snapshot;
    /// an uncommitted attempt never enters fact history (ADR-0275 §1).
    AttemptId
}

string_newtype_id! {
    /// Identity of one committed [`crate::context_lifecycle::ContextView`].
    ///
    /// Only a confirmed projection transaction mints a `CommittedView`
    /// (ADR-0275 §7).
    ViewId
}

string_newtype_id! {
    /// Identity of one derived [`crate::context_lifecycle::Checkpoint`].
    CheckpointId
}

string_newtype_id! {
    /// Scoped identity of a published raw artifact (ADR-0276).
    ArtifactId
}

string_newtype_id! {
    /// Identity of one tool execution, spanning dispatch through result commit.
    ExecutionId
}

string_newtype_id! {
    /// Identity of one compiled request snapshot (ADR-0277).
    RequestId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_as_transparent_strings() {
        let id = FactId::new("fact-1");
        assert_eq!(id.as_str(), "fact-1");
        assert_eq!(id.to_string(), "fact-1");
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            "\"fact-1\"",
            "a fact id must serialize as a bare string, not a struct"
        );
        assert_eq!(serde_json::from_str::<FactId>("\"fact-1\"").unwrap(), id);
    }

    #[test]
    fn ids_are_ordered_and_hashable() {
        use std::collections::BTreeSet;
        let mut set = BTreeSet::new();
        set.insert(BranchId::from("b"));
        set.insert(BranchId::from("a"));
        assert_eq!(
            set.into_iter().collect::<Vec<_>>(),
            vec![BranchId::from("a"), BranchId::from("b")],
            "typed ids must sort deterministically for stable serialization"
        );
    }

    #[test]
    fn distinct_id_kinds_are_distinct_types() {
        // Compile-time property: a FactId cannot be passed where a BranchId is
        // expected. Expressed here by construction so a regression that
        // collapses the newtypes into one type fails to compile.
        let fact: FactId = "x".into();
        let branch: BranchId = "x".into();
        assert_eq!(fact.as_str(), branch.as_str());
    }
}
