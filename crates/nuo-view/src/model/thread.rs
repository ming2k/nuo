//! The thread document: the thread's conversation, and nothing else
//! (ADR-0117, `[INV-TRANSCRIPT-01..04]`).
//!
//! # Why this type exists
//!
//! The rendered thread used to be a plain `Vec<TranscriptMessage>` that any
//! event arm could `push` into. Because the wire carries dialogue, operations,
//! telemetry and ephemera on one ordered stream (`RoundEvent`), "append whatever
//! arrived" made the transcript the default home for content that is not
//! dialogue: command results, background-job receipts, notices, projection
//! cards. That is not a rendering defect — it destroyed the one property a
//! transcript has: *everything in it was said, in this order*.
//!
//! [`ThreadDoc`] therefore has **one admission entry point**,
//! [`ThreadDoc::admit`], which takes the event's [`EventClass`] and refuses
//! anything that is not dialogue or turn structure. There is no `push`. A new
//! wire variant cannot reach the document without declaring a class
//! (`RoundEvent::class` is exhaustive) and passing the matching arm here.
//!
//! # What may be admitted
//!
//! | class                  | admitted as | why |
//! |------------------------|-------------|-----|
//! | `Dialogue`             | fact        | someone said something |
//! | `TurnOutcome`          | anchor      | it must hold a position in time (completion, interrupt, retry, compaction, job lifecycle) |
//! | `Operation`            | never       | it lives in the durable command ledger and is joined at render time |
//! | `Telemetry`            | never       | it drives chrome, not conversation |
//! | `Ephemera`             | never       | it belongs to a surface layer (see `nuo_view::model::layer`) |
//!
//! # Presentation edits are not admission
//!
//! Streaming deltas, disclosure toggles, cancellation and selection all mutate
//! *already admitted* facts. Those go through the targeted mutators below, which
//! can only reach an existing message by predicate or id — never append one.

use std::ops::Deref;

use nuo_wire::EventClass;

use crate::model::document::TranscriptMessage;

/// How an admission attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Admitted as a dialogue fact, at this message id.
    Dialogue(u64),
    /// Admitted as a structural turn marker, at this message id.
    Anchor(u64),
    /// Refused: this class has no home in the thread document.
    Refused(EventClass),
}

impl Admission {
    /// The admitted message id, when the attempt succeeded.
    pub fn id(self) -> Option<u64> {
        match self {
            Self::Dialogue(id) | Self::Anchor(id) => Some(id),
            Self::Refused(_) => None,
        }
    }

    /// Whether the attempt was refused.
    pub fn is_refused(self) -> bool {
        matches!(self, Self::Refused(_))
    }
}

/// The thread's conversation document.
///
/// Reads are a plain slice of facts ([`Deref`]); writes go through the named
/// methods, so "how did this get into the transcript?" always has an answer.
#[derive(Debug, Clone, Default)]
pub struct ThreadDoc {
    facts: Vec<TranscriptMessage>,
}

impl Deref for ThreadDoc {
    type Target = [TranscriptMessage];

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl ThreadDoc {
    /// An empty document.
    pub fn new() -> Self {
        Self::default()
    }

    /// The single admission gate.
    ///
    /// `class` is the event's declared [`EventClass`] — for content that did not
    /// come from an event (an optimistic prompt, a restored row) the caller
    /// states the class explicitly. A `Dialogue` message is appended as a fact;
    /// a `TurnOutcome` message as an anchor; every other class is refused and
    /// the message is dropped.
    pub fn admit(&mut self, class: EventClass, message: TranscriptMessage) -> Admission {
        match class {
            EventClass::Dialogue => {
                let id = message.id;
                self.facts.push(message);
                Admission::Dialogue(id)
            }
            EventClass::TurnOutcome => {
                let id = message.id;
                self.facts.push(message);
                Admission::Anchor(id)
            }
            refused => Admission::Refused(refused),
        }
    }

    /// Replace the whole document from a persisted projection (resume, session
    /// switch, fork). This is not admission: the rows were admitted when they
    /// were produced, and the store is the authority for them.
    pub fn reset_from(&mut self, facts: Vec<TranscriptMessage>) {
        self.facts = facts;
    }

    /// Move the admitted facts out (for a caller that must rebuild the document
    /// while holding another borrow). Pair with [`Self::restore`].
    pub fn take(&mut self) -> Vec<TranscriptMessage> {
        std::mem::take(&mut self.facts)
    }

    /// Put taken facts back.
    pub fn restore(&mut self, facts: Vec<TranscriptMessage>) {
        self.facts = facts;
    }

    /// Presentation-only mutable access to the admitted facts, for the streaming
    /// edit pipeline. A slice, deliberately: it cannot grow the document.
    pub fn facts_mut(&mut self) -> &mut [TranscriptMessage] {
        &mut self.facts
    }

    /// The last fact matching `pred`, mutably.
    pub fn find_last_mut(
        &mut self,
        mut pred: impl FnMut(&TranscriptMessage) -> bool,
    ) -> Option<&mut TranscriptMessage> {
        self.facts.iter_mut().rev().find(|m| pred(m))
    }

    /// The last fact matching `pred`.
    pub fn find_last(
        &self,
        pred: impl Fn(&TranscriptMessage) -> bool,
    ) -> Option<&TranscriptMessage> {
        self.facts.iter().rev().find(|fact| pred(fact))
    }

    /// Remove every fact matching `pred`.
    pub fn retain(&mut self, mut keep: impl FnMut(&TranscriptMessage) -> bool) {
        self.facts.retain(|m| keep(m));
    }

    /// Remove the last fact matching `pred`, returning it.
    pub fn remove_last(
        &mut self,
        pred: impl Fn(&TranscriptMessage) -> bool,
    ) -> Option<TranscriptMessage> {
        let idx = self.facts.iter().rposition(pred)?;
        Some(self.facts.remove(idx))
    }

    /// Drop every fact (session clear).
    pub fn clear(&mut self) {
        self.facts.clear();
    }

    /// The fact count.
    pub fn len(&self) -> usize {
        self.facts.len()
    }

    /// Whether the document has no facts.
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::Role;

    fn msg(text: &str) -> TranscriptMessage {
        TranscriptMessage::new(Role::Assistant, text)
    }

    #[test]
    fn only_dialogue_and_turn_outcomes_are_admitted() {
        let mut doc = ThreadDoc::new();
        assert!(matches!(
            doc.admit(EventClass::Dialogue, msg("hello")),
            Admission::Dialogue(_)
        ));
        assert!(matches!(
            doc.admit(EventClass::TurnOutcome, msg("interrupted")),
            Admission::Anchor(_)
        ));
        for refused in [
            EventClass::Operation,
            EventClass::Telemetry,
            EventClass::Ephemera,
        ] {
            let outcome = doc.admit(refused, msg("should not be here"));
            assert_eq!(outcome, Admission::Refused(refused));
        }
        let texts: Vec<&str> = doc.iter().map(|m| m.raw.as_str()).collect();
        assert_eq!(texts, vec!["hello", "interrupted"]);
        assert_eq!(doc.len(), 2);
    }

    #[test]
    fn a_refused_admission_does_not_grow_the_document() {
        let mut doc = ThreadDoc::new();
        doc.admit(EventClass::Dialogue, msg("kept"));
        let before = doc.len();
        doc.admit(EventClass::Operation, msg("dropped"));
        assert_eq!(doc.len(), before);
    }

    #[test]
    fn presentation_edits_reach_existing_facts_only() {
        let mut doc = ThreadDoc::new();
        doc.admit(EventClass::Dialogue, msg("first"));
        doc.admit(EventClass::Dialogue, msg("second"));
        let last = doc
            .find_last_mut(|m| m.raw == "second")
            .expect("second fact");
        last.raw.push('!');
        assert_eq!(doc.last().map(|m| m.raw.as_str()), Some("second!"));
        let removed = doc.remove_last(|m| m.raw.starts_with("first"));
        assert_eq!(removed.map(|m| m.raw), Some("first".to_string()));
        assert_eq!(doc.len(), 1);
    }

    #[test]
    fn reset_from_replaces_the_projection() {
        let mut doc = ThreadDoc::new();
        doc.admit(EventClass::Dialogue, msg("live"));
        doc.reset_from(vec![msg("restored-a"), msg("restored-b")]);
        let texts: Vec<&str> = doc.iter().map(|m| m.raw.as_str()).collect();
        assert_eq!(texts, vec!["restored-a", "restored-b"]);
    }
}
