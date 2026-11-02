//! Peer focus planes of the chat scenes ([ADR-0145]).
//!
//! The Thread scene hosts **two peer surfaces**: the composer (where a prompt is
//! drafted) and the transcript (where execution is inspected). Neither is a
//! mode of the other. Exactly one plane owns the keyboard at any instant, and
//! `Shift-Tab` is the sole chord that crosses between them — so the crossing is
//! symmetric by construction rather than by two unrelated chords that happen to
//! exist ([`FocusPlane`], `[INV-FOCUS-01]`).
//!
//! This type is the single owner of that state. The legacy shape — two
//! independent booleans (`focused_target: Option<InteractiveTarget>` and
//! `transcript_focused: bool`) plus a "browse focus" notion bolted onto the
//! composer's blur state — allowed states the model cannot express meaningfully
//! (`Some(target)` *and* `transcript_focused == false`, i.e. an entry selected
//! while the composer was supposedly live). Every such combination is now
//! unrepresentable: there is one plane, and the transcript plane remembers its
//! own sub-state.
//!
//! ## Why the sub-state is remembered
//!
//! The toggle must be an **involution**: crossing twice returns to exactly the
//! state the user left, including which entry was highlighted. A plane-only
//! toggle would drop the selection the moment the user glanced at the composer,
//! forcing a re-walk of `Ctrl+P` to find the same step. [`Focus`] therefore
//! carries the transcript's sub-state across a crossing, and
//! `Focus::toggle` flips only the plane.
//!
//! [ADR-0145]: ../../../docs/adr/0145-composer-and-transcript-are-peer-focus-planes.md

use nuo_view::model::layout::InteractiveTarget;

/// Which peer surface of the chat scene owns the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusPlane {
    /// The composer: typing, caret motion, readline chords, completion.
    #[default]
    Composer,
    /// The transcript: inspection — scrolling and entry selection. Holds no
    /// decision authority and never edits the draft.
    Transcript,
}

/// What the transcript plane is attending to.
///
/// Both variants are *inside* the transcript plane: moving between them is not
/// a plane crossing, and neither can be reached while the composer owns the
/// keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranscriptFocus {
    /// Viewport attention only: `↑`/`↓` scroll the transcript, page keys page
    /// it, `Home`/`End` jump to the edges. This is what a click into the
    /// transcript content produces.
    #[default]
    Browse,
    /// One activatable entry is highlighted and accepts the entry verbs
    /// (`Space` activate, `y` copy, `Shift-y` diagnostic, `o` view log).
    Entry(InteractiveTarget),
}

/// The keyboard ownership of a chat scene: one plane at a time, plus the
/// transcript's remembered sub-state so a crossing is lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Focus {
    plane: FocusPlane,
    /// Remembered while the composer owns the keyboard. Restored verbatim when
    /// the transcript plane is re-entered through [`Focus::toggle`].
    transcript: TranscriptFocus,
}

impl Focus {
    /// The composer owns the keyboard (the default for every chat scene).
    pub const fn composer() -> Self {
        Self {
            plane: FocusPlane::Composer,
            transcript: TranscriptFocus::Browse,
        }
    }

    /// The transcript owns the keyboard in the given sub-state.
    pub const fn transcript(focus: TranscriptFocus) -> Self {
        Self {
            plane: FocusPlane::Transcript,
            transcript: focus,
        }
    }

    /// The transcript owns the keyboard, browsing (no entry highlighted).
    pub const fn browsing() -> Self {
        Self::transcript(TranscriptFocus::Browse)
    }

    /// The transcript owns the keyboard with `target` highlighted.
    pub const fn at(target: InteractiveTarget) -> Self {
        Self::transcript(TranscriptFocus::Entry(target))
    }

    pub const fn plane(self) -> FocusPlane {
        self.plane
    }

    pub const fn is_composer(self) -> bool {
        matches!(self.plane, FocusPlane::Composer)
    }

    pub const fn is_transcript(self) -> bool {
        matches!(self.plane, FocusPlane::Transcript)
    }

    /// The remembered transcript sub-state, whether or not the transcript
    /// currently owns the keyboard.
    pub const fn transcript_focus(self) -> TranscriptFocus {
        self.transcript
    }

    /// The highlighted entry, or `None` when the composer owns the keyboard or
    /// the transcript is browsing. This is the *only* way a reader may ask
    /// "is an entry focused" — the plane and the sub-state cannot disagree.
    pub const fn entry(self) -> Option<InteractiveTarget> {
        match (self.plane, self.transcript) {
            (FocusPlane::Transcript, TranscriptFocus::Entry(target)) => Some(target),
            _ => None,
        }
    }

    /// Give the keyboard to the composer, remembering the transcript sub-state.
    pub fn enter_composer(&mut self) {
        self.plane = FocusPlane::Composer;
    }

    /// Give the keyboard to the transcript in `focus`.
    pub fn enter_transcript(&mut self, focus: TranscriptFocus) {
        self.plane = FocusPlane::Transcript;
        self.transcript = focus;
    }

    /// Drop the entry highlight but stay on the transcript, browsing.
    ///
    /// Used when the highlighted entry is no longer visible: the user's
    /// attention is still on the transcript, so the plane never silently
    /// reverts to the composer ([`Self::retain_entry`] callers).
    pub fn browse(&mut self) {
        self.transcript = TranscriptFocus::Browse;
    }

    /// Cross to the other plane, remembering (and, when crossing back,
    /// restoring) the transcript sub-state.
    ///
    /// This is an involution: `focus.toggle(); focus.toggle();` leaves the
    /// state — plane *and* remembered entry — exactly as it was, which is what
    /// makes `Shift-Tab` symmetric rather than a one-way "escape"
    /// (`[INV-FOCUS-01]`).
    pub fn toggle(&mut self) {
        self.plane = match self.plane {
            FocusPlane::Composer => FocusPlane::Transcript,
            FocusPlane::Transcript => FocusPlane::Composer,
        };
    }

    /// Rewrite the remembered entry when it no longer names a visible target,
    /// keeping the plane untouched.
    ///
    /// A transcript that mutates under the user (a step collapsing, a round
    /// appending) can strand the selection. Stranding must not change *which
    /// surface owns the keyboard* — that is the asymmetry this model exists to
    /// remove — so the entry degrades to browse focus and the plane survives.
    pub fn retain_entry(&mut self, target_still_visible: impl FnOnce(InteractiveTarget) -> bool) {
        if let TranscriptFocus::Entry(target) = self.transcript
            && !target_still_visible(target)
        {
            self.browse();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step() -> InteractiveTarget {
        InteractiveTarget::tool_step(3)
    }

    #[test]
    fn default_is_the_composer_plane() {
        let focus = Focus::default();
        assert_eq!(focus.plane(), FocusPlane::Composer);
        assert!(focus.is_composer());
        assert!(!focus.is_transcript());
        assert_eq!(focus.entry(), None);
    }

    #[test]
    fn toggle_is_an_involution_from_either_plane() {
        for start in [
            Focus::composer(),
            Focus::browsing(),
            Focus::at(step()),
            Focus::at(InteractiveTarget::reasoning(7)),
        ] {
            let mut focus = start;
            focus.toggle();
            assert_ne!(
                focus.plane(),
                start.plane(),
                "one toggle crosses the plane boundary: {start:?}"
            );
            focus.toggle();
            assert_eq!(focus, start, "two toggles restore the origin state");
        }
    }

    #[test]
    fn crossing_keeps_the_highlighted_entry() {
        // The whole point of the memory: glance at the composer, come back to
        // the same step instead of re-walking Ctrl+P.
        let mut focus = Focus::at(step());
        focus.toggle();
        assert!(focus.is_composer());
        assert_eq!(
            focus.entry(),
            None,
            "the composer plane cannot report an entry"
        );
        assert_eq!(
            focus.transcript_focus(),
            TranscriptFocus::Entry(step()),
            "the sub-state is remembered while the composer owns the keyboard"
        );
        focus.toggle();
        assert_eq!(focus.entry(), Some(step()));
    }

    #[test]
    fn browsing_survives_a_crossing_as_browsing() {
        let mut focus = Focus::browsing();
        focus.toggle();
        focus.toggle();
        assert!(focus.is_transcript());
        assert_eq!(focus.entry(), None);
    }

    #[test]
    fn entry_is_only_reported_from_the_transcript_plane() {
        // The legacy model's unrepresentable state: an entry selected while the
        // composer was live. `Focus` cannot express it.
        let composer_with_memory = {
            let mut focus = Focus::at(step());
            focus.enter_composer();
            focus
        };
        assert_eq!(composer_with_memory.entry(), None);
        assert_eq!(
            composer_with_memory.transcript_focus(),
            TranscriptFocus::Entry(step())
        );
    }

    #[test]
    fn enter_transcript_overwrites_the_sub_state() {
        let mut focus = Focus::at(step());
        focus.enter_transcript(TranscriptFocus::Browse);
        assert!(focus.is_transcript());
        assert_eq!(focus.entry(), None);
        focus.enter_transcript(TranscriptFocus::Entry(InteractiveTarget::notice(1)));
        assert_eq!(focus.entry(), Some(InteractiveTarget::notice(1)));
    }

    #[test]
    fn retain_entry_degrades_to_browse_without_leaving_the_plane() {
        let mut focus = Focus::at(step());
        focus.retain_entry(|_| false);
        assert!(focus.is_transcript(), "the plane survives a stranded entry");
        assert_eq!(focus.entry(), None);

        let mut focus = Focus::at(step());
        focus.retain_entry(|target| target == step());
        assert_eq!(focus.entry(), Some(step()), "a live entry is untouched");
    }

    #[test]
    fn retain_entry_is_inert_on_the_composer_plane() {
        let mut focus = Focus::composer();
        focus.retain_entry(|_| false);
        assert_eq!(focus, Focus::composer());
    }
}
