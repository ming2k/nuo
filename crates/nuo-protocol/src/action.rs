//! The action manifest: one declaration per action, owned by the server
//! ([ADR-0252](../../../docs/adr/0252-one-action-manifest-declared-primitives-and-compositions.md)).
//!
//! # Why this type exists
//!
//! Every interaction used to be declared up to six times — key interpretation, command
//! identity, a hand-written client intercept table, the server's slash vocabulary, the
//! wire request enum, and the driver's dispatch match — and no two of them agreed about
//! *who performs the action* or *where its result lands*. "Create a thread" alone had
//! five client implementations and three mechanisms, two of which tore the whole TUI
//! down to re-attach (ADR-0252 §1, measured in
//! `shell/tests/verb_and_instance_baseline.rs`).
//!
//! A manifest answers three questions in one place, and nothing else may answer them:
//!
//! | question | field |
//! | :--- | :--- |
//! | who performs it? | [`Authority`] |
//! | what kind of change is it? | [`ActionClass`] |
//! | where does the result land? | [`Presentation`] |
//!
//! # Two layers, and both belong to the server
//!
//! A [`Primitive`] is one indivisible act. A [`Composition`] is an ordered sequence of
//! primitives plus the presentation it lands in. **Compositions are declared on the
//! server too**, and that is the load-bearing decision: a client handed only primitives
//! must re-derive "create → bind workspace → inherit posture → switch foreground", which
//! is the knowledge that produced the five divergent implementations in the first place.
//!
//! A client chooses *which declared action to invoke* and *how to render it*. It holds
//! no step list, no ordering, and no class of its own.
//!
//! # Evidence is part of the contract
//!
//! The manifest's tables are meant to carry, per row, the measurement that justifies that
//! row's class — because a classification with no producer is a guess
//! ([ADR-0232](../../../docs/adr/0232-a-classifier-is-only-closed-if-its-members-have-producers.md)).
//! A row whose class cannot be pointed at an observable is a row that should not exist.
//! The gate for that is the bidirectional manifest/handler agreement declared by
//! [ADR-0252](../../../docs/adr/0252-one-action-manifest-declared-primitives-and-compositions.md)
//! (`INV-VERB-04`): a handler that emits no event cannot justify an `Act`.

use serde::{Deserialize, Serialize};

/// Who performs an action.
///
/// This is the one field a trigger may never override: a palette item, a keybinding and
/// a typed spelling of the same action must resolve to the same authority, or the action
/// has two meanings ([INV-VERB-01]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// The server performs it. The client may hold an optimistic projection, but the
    /// server's event is the verdict.
    Server,
    /// The client owns the surface and the server only answers. The four
    /// surface-owned commands (`/models`, `/connections`, `/tools`, `/mcp`) are the
    /// existing population: the server performs nothing, so recording anything about
    /// them would be a lie in the audit.
    Surface,
    /// The client performs it and it never crosses the wire: navigation, scrolling,
    /// focus, editing. Declared so that "local" is a decision rather than a default
    /// `match` arm.
    ///
    /// A local action is not a degenerate wire action. The majority of the input space
    /// is local, and admitting it to the server vocabulary is the mistake ADR-0119 §1
    /// records — naming a vocabulary for a *layer* instead of a *scope*.
    Local,
}

impl Authority {
    /// Whether invoking this action puts a frame on the wire.
    pub fn crosses_the_wire(self) -> bool {
        !matches!(self, Self::Local)
    }
}

/// What kind of change an action makes.
///
/// Three members, because the two-member effect vocabulary (`nuo_tool::CommandEffect`, which
/// this replaces for the manifest's purposes) conflated two
/// different questions: *does this change durable state?* and *does it occupy a row in
/// this thread's story?* `/new` answers yes to the first and no to the second — it
/// creates a thread, so it cannot be a row in the thread it creates, and the thread it
/// leaves is over. Folding it into `Mutation` forced the completeness net to special-case
/// session resets; folding it into `Reading` would deny that it changed anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    /// Durable state this thread must remember. Earns a ledger record and a row in the
    /// thread's story. The reply is evidence attached to the act.
    Act,
    /// A measurement of the world, true only at the moment of asking. No ledger record,
    /// no row; the reply is a receipt.
    Reading,
    /// A change to durable *topology* rather than to this thread's content: it creates,
    /// binds, switches, or ends threads. An act for audit purposes, but never a row in
    /// the thread it creates.
    SessionControl,
}

impl ActionClass {
    /// Whether this class earns a durable ledger record.
    ///
    /// `SessionControl` deliberately does not: the ledger it would enter belongs to a
    /// session that was minted by the same action. The audit value is carried by the
    /// receipt and by the topology events (`ConversationCleared`, `ThreadSubscribed`),
    /// which are facts about the new thread rather than claims about the old one.
    pub fn earns_a_record(self) -> bool {
        matches!(self, Self::Act)
    }

    /// Whether this class may occupy a row in the thread's transcript.
    pub fn occupies_a_row(self) -> bool {
        matches!(self, Self::Act)
    }
}

impl From<nuo_tool::CommandEffect> for ActionClass {
    fn from(effect: nuo_tool::CommandEffect) -> Self {
        match effect {
            nuo_tool::CommandEffect::Mutation => Self::Act,
            nuo_tool::CommandEffect::Reading => Self::Reading,
            nuo_tool::CommandEffect::SessionControl => Self::SessionControl,
        }
    }
}

/// Where an action's result lands.
///
/// Declared here, in the manifest, and never inferred from a trigger, a name, or a text
/// shape. ADR-0233 `[INV-CMDMSG-02]` forbids deciding a projection's fate from a
/// message's *text*; [INV-VERB-05] extends the ban to the *trigger* — whether `/new`
/// reaches the transcript must not depend on whether the user pressed `+`, typed it, or
/// clicked the palette.
///
/// The client still chooses **how** to render: a receipt may be a toast, a notice-centre
/// line, or both. It does not choose **whether** the result is a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presentation {
    /// A row in the thread's transcript, settled in place.
    ThreadRow,
    /// A receipt in the client's notice log (ADR-0121), never a row.
    Receipt,
    /// Chrome only: no transcript, no receipt.
    Silent,
}

impl Presentation {
    /// Whether this presentation may write into the thread document.
    pub fn reaches_the_transcript(self) -> bool {
        matches!(self, Self::ThreadRow)
    }
}

/// A client-side projection an action can invalidate.
///
/// Declared so that "what must the client re-read after this?" is a property of the
/// action rather than something each call site remembers. A single-owner state model
/// (ADR-0253) is only sufficient *because* this list exists: with one owner there is
/// nothing to reconcile, and this says what to refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    /// The thread document itself (dialogue, anchors, and ledger-derived act rows).
    Transcript,
    /// The per-thread chrome: phase, responding flag, round/turn counters, cost.
    Chrome,
    /// The follow-up queue projection.
    Queue,
    /// Token accounting and the context-window report.
    ContextTokens,
    /// Which thread owns the live composer buffers (ADR-0247).
    DraftOwner,
    /// The tab strip: open tabs, their titles, and the foreground selection.
    Tabs,
    /// The thread's authentic title (ADR-0050).
    Title,
    /// Provider/model attribution.
    Attribution,
    /// Posture: unattended, confinement, retry point, and workspace trust.
    Posture,
}

/// A precondition an action states as data.
///
/// "The caller should have…" is the shape that let a hidden precondition live only in
/// client memory (the queue modal's "owe a release" bookkeeping, `queue_exit_session`).
/// A requirement is checkable by the client *before* dispatch and by the server *on*
/// dispatch, so the two cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Requirement {
    /// The acting thread must carry content.
    ThreadHasContent,
    /// The client must currently be viewing the acting thread.
    ThreadIsForeground,
    /// A round must be running on the acting thread.
    RoundIsRunning,
    /// No round may be running on the acting thread.
    RoundIsIdle,
    /// The server-attested readiness gate must be `Ready` (ADR-0133).
    ThreadIsReady,
}

/// Whether replaying an action without a request id is harmless.
///
/// Stated per primitive because "is this safe to retry?" is a property of the action,
/// not of the transport. A primitive that mints an identity is never idempotent, and
/// saying so is what makes the client's retry policy mechanical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    /// Replaying produces the same state. No request id needed.
    Idempotent,
    /// Replaying must be de-duplicated by a caller-supplied request id.
    IdempotentWithKey,
    /// Replaying has further effects. The client must not retry.
    NotIdempotent,
}

/// A stable identifier for one declared action.
///
/// Namespaced (`thread.create`, `queue.push`) because one primitive set cannot cover
/// every domain: provider/OAuth/MCP carry their own state machines, and forcing them
/// into a universal vocabulary would either invent meaningless members or flatten three
/// domains into one (ADR-0252 §6). A namespace is a domain, not a layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ActionId(pub &'static str);

impl ActionId {
    /// The namespace segment: everything before the first `.`.
    pub fn namespace(self) -> &'static str {
        match self.0.split_once('.') {
            Some((namespace, _)) => namespace,
            None => self.0,
        }
    }

    /// The member segment: everything after the first `.`, or the whole id when the
    /// declaration omitted the separator.
    pub fn member(self) -> &'static str {
        match self.0.split_once('.') {
            Some((_, member)) => member,
            None => self.0,
        }
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// One indivisible act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Primitive {
    pub id: ActionId,
    pub authority: Authority,
    pub class: ActionClass,
    /// What the client must re-read after this succeeded.
    pub invalidates: &'static [Projection],
    /// Preconditions, stated as data.
    pub requires: &'static [Requirement],
    pub idempotency: Idempotency,
}

/// An ordered sequence of primitives plus the presentation it lands in.
///
/// A composition may not contain another composition: that rule keeps every step
/// inspectable and stops a manifest from becoming a scripting language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Composition {
    pub id: ActionId,
    /// Ordered steps. Each must name a declared [`Primitive`].
    pub steps: &'static [ActionId],
    pub requires: &'static [Requirement],
    pub invalidates: &'static [Projection],
    /// Where the result lands. The client reads this; it never infers it.
    pub presentation: Presentation,
    /// The composer spelling, if this composition is reachable as a slash command.
    pub slash: Option<&'static str>,
}

impl Composition {
    /// The class of the whole composition.
    ///
    /// A composition inherits the strongest claim its steps make, because a sequence that
    /// changes durable topology cannot be filed as a reading just because one of its steps
    /// only read something. Precedence is `SessionControl` > `Act` > `Reading`:
    ///
    /// - `SessionControl` wins because a topology step may re-key the identity a row would
    ///   belong to, which makes the question "which thread is this row in?" unanswerable.
    /// - `Act` beats `Reading` because a missing record is unrecoverable while a spurious
    ///   one is merely noisy (ADR-0140's asymmetry).
    ///
    /// A composition with no steps has no class: an empty sequence declares nothing, and
    /// inventing a default would let a manifest row pass [INV-VERB-04] while doing
    /// nothing.
    pub fn class(&self, primitives: &'static [Primitive]) -> Option<ActionClass> {
        let mut strongest = None;
        for step in self.steps {
            let class = primitives.iter().find(|p| p.id == *step)?.class;
            strongest = Some(match (strongest, class) {
                (Some(ActionClass::SessionControl), _) | (_, ActionClass::SessionControl) => {
                    ActionClass::SessionControl
                }
                (Some(ActionClass::Act), _) | (_, ActionClass::Act) => ActionClass::Act,
                _ => ActionClass::Reading,
            });
        }
        strongest
    }
}

/// A namespace's declared primitives and compositions.
///
/// Grouped rather than flat so that ADR-0119 §2b's single-owner rule holds per domain:
/// a namespace is generated from its own handler table, so a hand-written second copy
/// cannot appear inside one domain either.
#[derive(Debug, Clone, Copy)]
pub struct Namespace {
    pub name: &'static str,
    pub primitives: &'static [Primitive],
    pub compositions: &'static [Composition],
}

impl Namespace {
    /// The primitive with this id, if this namespace declares it.
    pub fn primitive(&self, id: ActionId) -> Option<&'static Primitive> {
        self.primitives.iter().find(|p| p.id == id)
    }

    /// The composition with this id, if this namespace declares it.
    pub fn composition(&self, id: ActionId) -> Option<&'static Composition> {
        self.compositions.iter().find(|c| c.id == id)
    }

    /// The composition reachable by this slash spelling, if any.
    pub fn composition_for_slash(&self, slash: &str) -> Option<&'static Composition> {
        self.compositions.iter().find(|c| c.slash == Some(slash))
    }
}

#[cfg(test)]
mod tests;
