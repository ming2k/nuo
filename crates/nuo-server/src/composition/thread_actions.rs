//! The `thread.*` namespace: the manifest's first declared domain
//! ([ADR-0252](../../../docs/adr/0252-one-action-manifest-declared-primitives-and-compositions.md)).
//!
//! # Why a namespace and not the whole vocabulary
//!
//! ADR-0252 §6 splits the manifest by domain because one primitive set cannot cover every
//! domain: provider registration, OAuth, and MCP carry their own state machines, and
//! forcing them into a universal vocabulary would either invent meaningless members or
//! flatten three domains into one. A namespace is a *domain*, not a layer — which is the
//! distinction ADR-0119 §1 records the repository getting wrong once already.
//!
//! # What is declared here, and what is deliberately not
//!
//! Only the thread lifecycle's primitives and the compositions the client actually
//! offers. Prompting, steering, and queueing are a separate namespace (they are thread
//! *content*, not thread *topology*), and the provider/OAuth/MCP state machines are
//! theirs. Declaring a small honest namespace is what makes the bidirectional gate
//! (`INV-VERB-04`, ADR-0252) meaningful: every row below names a handler that exists, and
//! every handler in this domain must have a row.
//!
//! # The measurement that shaped `start_fresh`
//!
//! `StartFreshThread` is the composition the baseline test measures
//! (`shell/tests/verb_and_instance_baseline.rs`): three client triggers, two mechanisms,
//! and a residue row. Declaring it here is what lets step 5 of ADR-0252 collapse the
//! five implementations onto one call — the client reads `presentation: Receipt` and
//! stops writing a `/new` transcript row, because the manifest says that action does not
//! reach the transcript.

use nuo_protocol::action::{
    ActionClass, ActionId, Authority, Composition, Idempotency, Namespace, Presentation, Primitive,
    Projection, Requirement,
};

/// Thread-lifecycle primitives.
///
/// Each row's class is the answer to "does this leave a row in the thread's story?":
/// - `thread.create` / `thread.bind_workspace` / `thread.end` change *topology*, so they
///   are `SessionControl` — an act for audit, never a row in the thread they mint.
/// - `thread.interrupt` stops a round inside an existing thread: it *is* part of that
///   thread's story, so it is an `Act` and settles a row.
/// - `thread.read_status` reports state at the moment of asking: a `Reading`, receipt
///   only (ADR-0140).
pub const PRIMITIVES: &[Primitive] = &[
    Primitive {
        id: ActionId("thread.create"),
        authority: Authority::Server,
        class: ActionClass::SessionControl,
        invalidates: &[
            Projection::Transcript,
            Projection::Tabs,
            Projection::DraftOwner,
        ],
        requires: &[],
        // Mints an identity, so replay would mint a second one.
        idempotency: Idempotency::NotIdempotent,
    },
    Primitive {
        id: ActionId("thread.bind_workspace"),
        authority: Authority::Server,
        class: ActionClass::SessionControl,
        invalidates: &[],
        requires: &[],
        // Binding the same workspace twice is the same state; the key makes a retried
        // frame a no-op rather than a second bind.
        idempotency: Idempotency::IdempotentWithKey,
    },
    Primitive {
        id: ActionId("thread.inherit_posture"),
        authority: Authority::Server,
        class: ActionClass::SessionControl,
        invalidates: &[Projection::Posture, Projection::Attribution],
        requires: &[],
        idempotency: Idempotency::IdempotentWithKey,
    },
    Primitive {
        id: ActionId("thread.switch_foreground"),
        authority: Authority::Server,
        class: ActionClass::SessionControl,
        invalidates: &[
            Projection::Tabs,
            Projection::Transcript,
            Projection::DraftOwner,
        ],
        requires: &[],
        idempotency: Idempotency::Idempotent,
    },
    Primitive {
        id: ActionId("thread.interrupt"),
        authority: Authority::Server,
        class: ActionClass::Act,
        invalidates: &[Projection::Chrome],
        requires: &[Requirement::RoundIsRunning],
        idempotency: Idempotency::Idempotent,
    },
    Primitive {
        id: ActionId("thread.interrupt_subagent"),
        authority: Authority::Server,
        class: ActionClass::Act,
        invalidates: &[Projection::Transcript],
        requires: &[Requirement::RoundIsRunning],
        idempotency: Idempotency::Idempotent,
    },
    Primitive {
        id: ActionId("thread.end"),
        authority: Authority::Server,
        class: ActionClass::SessionControl,
        invalidates: &[Projection::Tabs, Projection::Transcript],
        requires: &[],
        idempotency: Idempotency::Idempotent,
    },
    Primitive {
        id: ActionId("thread.read_status"),
        authority: Authority::Server,
        class: ActionClass::Reading,
        invalidates: &[],
        requires: &[],
        idempotency: Idempotency::Idempotent,
    },
];

/// Compositions the client may invoke.
///
/// `start_fresh` is the one ADR-0252 §1b measures, and it is the whole reason the third
/// `ActionClass` exists: it is a topology change, so its `Presentation` is a receipt and
/// the `/new` transcript row that the TabBar's `+` currently produces becomes
/// unrepresentable rather than merely discouraged.
pub const COMPOSITIONS: &[Composition] = &[
    Composition {
        id: ActionId("thread.start_fresh"),
        steps: &[
            ActionId("thread.create"),
            ActionId("thread.bind_workspace"),
            ActionId("thread.inherit_posture"),
            ActionId("thread.switch_foreground"),
        ],
        // Always valid: recycles in place when empty (ADR-0253 §5) or mounts a new resident
        // tab when non-empty. Branching policy is not a failure precondition.
        requires: &[],
        invalidates: &[
            Projection::Transcript,
            Projection::Chrome,
            Projection::Queue,
            Projection::ContextTokens,
            Projection::DraftOwner,
            Projection::Tabs,
            Projection::Posture,
        ],
        presentation: Presentation::Receipt,
        slash: Some("/new"),
    },
    Composition {
        id: ActionId("thread.halt"),
        steps: &[ActionId("thread.interrupt")],
        requires: &[Requirement::RoundIsRunning],
        invalidates: &[Projection::Chrome],
        presentation: Presentation::ThreadRow,
        slash: None,
    },
    Composition {
        id: ActionId("thread.end_foreground"),
        steps: &[ActionId("thread.end")],
        requires: &[],
        invalidates: &[
            Projection::Tabs,
            Projection::Transcript,
            Projection::DraftOwner,
        ],
        presentation: Presentation::Receipt,
        slash: Some("/exit"),
    },
];

/// The namespace the manifest publishes for thread lifecycle.
pub const NAMESPACE: Namespace = Namespace {
    name: "thread",
    primitives: PRIMITIVES,
    compositions: COMPOSITIONS,
};

#[cfg(test)]
mod tests;
