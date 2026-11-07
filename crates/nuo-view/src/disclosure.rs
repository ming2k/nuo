//! Step state machine: the three orthogonal axes that determine a step's
//! presentation, and the pure functions that reduce them to color.
use nuotc::Color;

use nuo_ui_primitives::theme::Theme;

/// Whether a step's body is shown. User-controlled (click / `Enter` /
/// auto-expand on first stream chunk) and persisted on the message so it
/// survives redraws and history restore.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Disclosure {
    /// Only the one-line summary is visible.
    Collapsed,
    /// The summary plus its body are both visible.
    Expanded,
}

impl Disclosure {
    /// Build from the raw `expanded` bool carried on the message.
    pub fn from_expanded(expanded: bool) -> Self {
        if expanded {
            Disclosure::Expanded
        } else {
            Disclosure::Collapsed
        }
    }
}

/// Transient interaction with a step summary, recomputed every frame from
/// pointer / keyboard state. Never persisted.
///
/// The three variants are **not** three intensities of one cue. They are three
/// distinct questions the user asks of a row, and each resolves through a
/// different channel:
///
/// | Variant | Question | Channel |
/// | --- | --- | --- |
/// | [`Idle`](Interaction::Idle) | — | none |
/// | [`Hovered`](Interaction::Hovered) | "would a click land here?" | affordance **hue** |
/// | [`Focused`](Interaction::Focused) | "does the keyboard own this?" | entry **marker** |
///
/// Collapsing hover and focus into one tint (the previous model) is what made
/// the `Shift-Tab` crossing invisible: the keyboard could be on a step with no
/// more signal than a pointer resting on it, and on an *expanded* step not even
/// that — the tint was suppressed there entirely.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interaction {
    /// Not under the pointer and not keyboard-focused.
    Idle,
    /// Pointer rests on the summary line — a soft hover affordance.
    Hovered,
    /// The keyboard **owns** this entry (the transcript plane is live and this
    /// entry is its highlighted target). Resolves to the entry marker — the one
    /// cell the row carries in its reserved gutter
    /// ([`crate::focus_plane::draw_entry_marker`]) — on top of the pane wash that
    /// says the keyboard is in the transcript at all
    /// ([`crate::focus_plane::draw_pane_focus`]).
    Focused,
    /// Both at once: the keyboard owns the entry **and** the pointer is on it.
    ///
    /// This combination is not a curiosity — it is how a row is usually reached
    /// (click it to focus it, and the pointer is still there). The original
    /// three-variant shape had to discard one of the two facts, and it discarded
    /// hover, which made the row under the pointer go dead the moment the
    /// keyboard arrived on it. Keeping the pair representable costs one variant
    /// and makes "focus is not a tint" and "hover always answers the pointer"
    /// both true.
    FocusedHovered,
}

impl Interaction {
    /// Build from the raw interaction flags produced by the call site.
    ///
    /// **Focus and hover are independent, and both can be true.** The state is
    /// carried as [`Self::Focused`] because keyboard ownership is the stronger
    /// fact, but a focused row that is *also* under the pointer must still answer
    /// the pointer: `Focused` reports `is_hovered()` so the affordance hue can
    /// ride on top of it. Treating the two as mutually exclusive (the original
    /// shape) made hovering the keyboard's own row completely inert — the
    /// commonest way to reach a row you are inspecting.
    pub fn from_hover_focused(hovered: bool, focused: bool) -> Self {
        match (hovered, focused) {
            (false, false) => Interaction::Idle,
            (true, false) => Interaction::Hovered,
            (false, true) => Interaction::Focused,
            (true, true) => Interaction::FocusedHovered,
        }
    }

    /// Whether the pointer is answering this row.
    pub const fn is_hovered(self) -> bool {
        matches!(self, Interaction::Hovered | Interaction::FocusedHovered)
    }

    /// The transient affordance **hue** for the *hover* question. Keyboard focus
    /// on its own deliberately contributes nothing here: it answers a different
    /// question and speaks through the pane wash plus the entry marker instead, so
    /// a focused entry never has to compete with its own pointer cue. A row that is
    /// **both** keeps the pointer's answer, because the pointer being there is
    /// still a live click target.
    pub fn color(self, theme: &Theme) -> Option<Color> {
        if self.is_hovered() {
            Some(theme.affordance())
        } else {
            None
        }
    }

    /// Whether the keyboard owns this entry — and therefore whether it is the one
    /// the pane's entry marker names.
    ///
    /// The **painting** of the marker is deliberately not a per-row concern: it is
    /// one cell in the pane gutter, drawn once by
    /// [`crate::focus_plane::draw_entry_marker`] from the entry's registered
    /// header row. Keeping it out of the row renderers is what lets every entry
    /// kind — tool, reasoning, command, compaction card, notice — say the same
    /// thing in the same place without any of them knowing the cue exists, and it
    /// is why there is no second `is_marked` predicate to keep in sync.
    pub const fn is_focused(self) -> bool {
        matches!(self, Interaction::Focused | Interaction::FocusedHovered)
    }
}

/// Summary-line **weight** (luminance) — a pure function of disclosure. This
/// is the "is it open?" channel only; it never depends on lifecycle or
/// interaction, so it cannot leak run-state into the brightness.
pub fn summary_weight(disclosure: Disclosure, theme: &Theme) -> Color {
    match disclosure {
        Disclosure::Expanded => theme.fg(),
        Disclosure::Collapsed => theme.muted(),
    }
}

/// Resolve the final summary text color from the channels:
///
/// - **Disclosure luminance** via [`summary_weight`]: expanded → `fg`,
///   collapsed → `muted`.
/// - **Lifecycle accent** (hue): a non-completed lifecycle supplies an accent
///   so a running / failed / denied step stays visibly classified even when
///   collapsed and idle (per ADR 0008, a steady hue — never a breathing
///   sweep). It leans toward the disclosure luminance when the body is open
///   (`ACCENT_EXPANDED_BLEND`) and stays intact while collapsed.
/// - **Interaction affordance hue**: hover/focus tint the result
///   toward the theme's affordance token (`INTERACTION_HOVER_BLEND`) — a hue
///   shift on top of whatever luminance/hue the summary already has, so the
///   cue reads identically on plain, accented, and open summaries without
///   ever changing their brightness ordering.
///
/// This is the single entry point renderers use for the summary text color,
/// keeping the three-channel separation in one auditable place.
pub fn summary_text_color(
    accent: Option<Color>,
    disclosure: Disclosure,
    interaction: Interaction,
    theme: &Theme,
) -> Color {
    let weight = summary_weight(disclosure, theme);
    // Channel 1 + 2: lifecycle accent over disclosure luminance.
    let base = match accent {
        Some(accent) => {
            let t = match disclosure {
                Disclosure::Expanded => ACCENT_EXPANDED_BLEND,
                Disclosure::Collapsed => ACCENT_IDLE_BLEND,
            };
            accent.blend(weight, t)
        }
        None => weight,
    };
    // Channel 3: the interaction affordance hue, composed last so the cue
    // rides on top of idle/accent composition. Scoped to *collapsed* steps:
    // an open body already announces itself (the `-` marker, the visible
    // body, the sticky pin when scrolled) — the old model's lesson was that
    // re-decorating the active state only muddies the disclosure signal.
    match (disclosure, interaction.color(theme)) {
        (Disclosure::Collapsed, Some(hue)) => {
            // In 16-color ANSI (where hue and base are discrete named ANSI tokens, e.g. Color::Yellow vs Color::Gray),
            // RGB float blending produces intermediate values that snap back to Gray during Euclidean quantization.
            // When named discrete ANSI tokens are used, preserve the affordance token directly.
            let is_named_ansi = !matches!(hue, Color::Rgb(..)) && !matches!(base, Color::Rgb(..));
            if is_named_ansi {
                hue
            } else {
                base.blend(hue, INTERACTION_HOVER_BLEND)
            }
        }
        _ => base,
    }
}

/// Blend factors composing the channels. Exposed as module consts so the unit
/// tests assert the exact composed color rather than only "it changed".
/// A collapsed idle step leaves the lifecycle accent untouched; an open body
/// leans toward the disclosure luminance; and the hover/focus affordance is a
/// strong tint toward the affordance hue (it must clearly read as a hue
/// change, not a subtle drift).
const ACCENT_IDLE_BLEND: f32 = 0.0;
const ACCENT_EXPANDED_BLEND: f32 = 0.6;
const INTERACTION_HOVER_BLEND: f32 = 0.70;

#[cfg(test)]
mod tests {
    use super::*;

    /// The disclosure luminance rungs stay distinct, and the affordance hue
    /// is distinct from both of them — so "interactive" (hue) can never be
    /// confused with "open" or "idle" (luminance). This is the core invariant
    /// of the channel separation.
    #[test]
    fn three_tones_are_distinct() {
        let theme = Theme::default();
        assert_ne!(theme.affordance(), theme.fg());
        assert_ne!(theme.affordance(), theme.muted());
        assert_ne!(theme.fg(), theme.muted());
    }

    /// Monotonic invariant (retained): interaction may never change a
    /// summary's disclosure luminance. An expanded summary stays pinned at
    /// `fg` with no transient cue at all; a collapsed one rests on the muted
    /// rung — hover/focus only add the affordance tint on top, never a
    /// brighter rung.
    #[test]
    fn hover_focus_never_changes_luminance() {
        let theme = Theme::default();
        // Expanded: interaction is a complete no-op.
        for interaction in [
            Interaction::Idle,
            Interaction::Hovered,
            Interaction::Focused,
        ] {
            assert_eq!(
                summary_text_color(None, Disclosure::Expanded, interaction, &theme),
                theme.fg(),
                "an expanded summary stays at fg regardless of interaction",
            );
        }
        // Collapsed: the base rung stays muted; hover/focus differentiate
        // only through the affordance hue tint (asserted in
        // `collapsed_hover_focus_tints_to_affordance_hue`).
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Idle, &theme),
            theme.muted()
        );
    }

    /// On a *collapsed* summary **hover** is a hue cue: it tints the muted
    /// resting tone toward the affordance hue. Focus deliberately does *not* —
    /// it answers a different question and speaks through the entry marker
    /// instead (see `focus_is_a_marker_not_a_tint`), so a focused row never has
    /// to share a channel with "the pointer is here".
    #[test]
    fn collapsed_hover_tints_to_affordance_hue() {
        let theme = Theme::default();
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Hovered, &theme),
            theme
                .muted()
                .blend(theme.affordance(), INTERACTION_HOVER_BLEND)
        );
        // A hue shift, not a luminance step: the tinted result must differ
        // from both plain rungs.
        assert_ne!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Hovered, &theme),
            theme.muted()
        );
        assert_ne!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Hovered, &theme),
            theme.fg()
        );
    }

    /// Keyboard focus is a **marker**, not a tint.
    ///
    /// The transcript answers "who owns the keyboard?" through the pane wash
    /// (painted once over the whole viewport) plus this entry marker, so
    /// `Shift-Tab` reads as a surface swap. A foreground tint cannot do that — it
    /// is invisible on an expanded step and easily out-shone by a lifecycle
    /// accent on a collapsed one.
    #[test]
    fn focus_is_a_marker_not_a_tint() {
        let theme = Theme::default();
        let focused = Interaction::from_hover_focused(false, true);
        let hovered = Interaction::from_hover_focused(true, false);

        // The marker exists for focus, and only for focus.
        assert!(focused.is_focused());
        assert!(!hovered.is_focused(), "nothing moved the keyboard");
        assert!(!Interaction::Idle.is_focused());

        // The tint channel is now hover-only, so focus never has to compete
        // with its own pointer cue for the same few characters.
        assert_eq!(focused.color(&theme), None);
        assert_eq!(hovered.color(&theme), Some(theme.affordance()));
    }

    /// The marker is a keyboard-ownership cue, so it must not be the generic
    /// selection tone, and it is a *marker*, not a band: the row's own surface is
    /// never repainted.
    #[test]
    fn focus_marker_is_not_the_selection_tone() {
        let theme = Theme::default();
        let marker = theme.pane_marker_style();
        assert_ne!(marker.fg, theme.selected());
        assert_ne!(marker.fg, theme.surface());
        assert_ne!(marker.fg, theme.input_surface_inactive());
    }

    /// The marker exists on **every** disclosure state, including expanded — the
    /// exact case where the old affordance-hue cue was suppressed and the
    /// crossing vanished. Keyboard ownership is not a disclosure fact.
    ///
    /// It also leaves the row's own tone alone: the marker is a separate cell, so
    /// there is no band for the text to be re-seated against. That is the whole
    /// reason the earlier band was replaced — it had to invert the disclosure and
    /// lifecycle tones the renderers resolved on purpose.
    #[test]
    fn focus_marker_survives_an_expanded_entry() {
        let theme = Theme::default();
        for disclosure in [Disclosure::Collapsed, Disclosure::Expanded] {
            assert!(
                Interaction::Focused.is_focused(),
                "{disclosure:?}: keyboard ownership must not depend on disclosure"
            );
            // The composition is untouched: focus adds a cell, never a tone.
            for interaction in [Interaction::Idle, Interaction::Focused] {
                assert_eq!(
                    summary_text_color(None, disclosure, interaction, &theme),
                    summary_text_color(None, disclosure, Interaction::Idle, &theme),
                    "{disclosure:?}: focus must not recolour the summary"
                );
            }
        }
    }

    /// Unfocused entries must render exactly as they did before the marker
    /// existed — the focus cue is additive state, not a restyling of the whole
    /// transcript.
    #[test]
    fn unfocused_entries_carry_no_marker() {
        for interaction in [Interaction::Idle, Interaction::Hovered] {
            assert!(!interaction.is_focused());
        }
    }

    /// Expanded and collapsed are mutually exclusive peers, decided only by
    /// disclosure: an open idle step is the primary foreground, a closed idle
    /// step is muted. Regression for the original bug — closing a step must
    /// *immediately* darken it instead of staying bright.
    #[test]
    fn idle_disclosure_decides_fg_vs_muted() {
        let theme = Theme::default();
        assert_eq!(
            summary_text_color(None, Disclosure::Expanded, Interaction::Idle, &theme),
            theme.fg()
        );
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Idle, &theme),
            theme.muted()
        );
    }

    /// Regression for the reported bug: after clicking a summary to collapse
    /// it, the step must darken to muted. The close click also sets keyboard
    /// focus, and that focus now paints a *marker* (a separate cell) rather than
    /// lifting the text's luminance — so the closed summary still reads on the
    /// muted rung, and an expanded step is never dimmer than a closed one in any
    /// state.
    #[test]
    fn closing_a_step_darkens_it() {
        let theme = Theme::default();
        let open = summary_text_color(None, Disclosure::Expanded, Interaction::Idle, &theme);
        let closed = summary_text_color(None, Disclosure::Collapsed, Interaction::Idle, &theme);
        assert_ne!(
            open, closed,
            "an open step must not read the same color as a closed idle one"
        );
        assert_ne!(open, theme.muted());
        assert_eq!(closed, theme.muted());
        // Focus contributes no luminance: the tone is unchanged, the marker is
        // what moves.
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Focused, &theme),
            theme.muted()
        );
    }

    /// A lifecycle accent is *not* discarded: idle + accent returns the accent
    /// untouched (the running / failed step stays vivid), while an open body
    /// leans toward the disclosure luminance. This is the composition
    /// contract — the hue dominates, the luminance shifts on disclosure.
    #[test]
    fn accent_idle_is_intact_expanded_blends() {
        let theme = Theme::default();
        let accent = Color::Rgb(128, 153, 156); // an arbitrary accent (e.g. info hue)
        // Idle collapsed: the accent is returned unchanged.
        assert_eq!(
            summary_text_color(
                Some(accent),
                Disclosure::Collapsed,
                Interaction::Idle,
                &theme
            ),
            accent
        );
        // Expanded leans toward the primary foreground (its own rung) and is
        // pinned: idle, hover and focus all produce the same composed color,
        // so pointing at an open accent step is a no-op just like the plain case.
        let expanded = summary_text_color(
            Some(accent),
            Disclosure::Expanded,
            Interaction::Idle,
            &theme,
        );
        assert_ne!(expanded, accent);
        assert_eq!(expanded, accent.blend(theme.fg(), ACCENT_EXPANDED_BLEND));
        for interaction in [Interaction::Hovered, Interaction::Focused] {
            assert_eq!(
                summary_text_color(Some(accent), Disclosure::Expanded, interaction, &theme),
                expanded,
                "an open accent step carries no transient cue"
            );
        }
    }

    /// Regression: an accent step must shift on hover. The composed result
    /// must differ between idle and hover — the affordance tint rides on top
    /// of the accent instead of being swallowed by it.
    #[test]
    fn accent_step_hover_is_visible() {
        let theme = Theme::default();
        let accent = theme.info();
        let idle = summary_text_color(
            Some(accent),
            Disclosure::Collapsed,
            Interaction::Idle,
            &theme,
        );
        let hover = summary_text_color(
            Some(accent),
            Disclosure::Collapsed,
            Interaction::Hovered,
            &theme,
        );
        assert_ne!(
            idle, hover,
            "hovering an accented step must change its color"
        );
        // The tint preserves the accent's luminance: the hover result is a
        // blend of the idle result with the affordance hue.
        assert_eq!(
            hover,
            idle.blend(theme.affordance(), INTERACTION_HOVER_BLEND)
        );
    }

    /// No accent falls through to the disclosure rungs.
    #[test]
    fn no_accent_uses_weight() {
        let theme = Theme::default();
        // Idle peers: expanded → fg, collapsed → muted.
        assert_eq!(
            summary_text_color(None, Disclosure::Expanded, Interaction::Idle, &theme),
            theme.fg()
        );
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Idle, &theme),
            theme.muted()
        );
        // Collapsed hover/focus tint toward the affordance hue; expanded
        // stays pinned at fg (an open body needs no transient cue).
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Hovered, &theme),
            theme
                .muted()
                .blend(theme.affordance(), INTERACTION_HOVER_BLEND)
        );
        assert_eq!(
            summary_text_color(None, Disclosure::Expanded, Interaction::Hovered, &theme),
            theme.fg()
        );
    }

    /// Focus and hover are **independent facts**, and the type can hold both.
    ///
    /// The regression this replaces: the first three-variant shape collapsed
    /// "focused" and "focused + hovered" into one variant, so hovering the row the
    /// keyboard was already on produced no cue at all — and that is the commonest
    /// way a row gets reached (click it, pointer still there).
    #[test]
    fn focus_and_hover_are_independent() {
        assert_eq!(
            Interaction::from_hover_focused(false, false),
            Interaction::Idle
        );
        assert_eq!(
            Interaction::from_hover_focused(true, false),
            Interaction::Hovered
        );
        assert_eq!(
            Interaction::from_hover_focused(false, true),
            Interaction::Focused
        );
        assert_eq!(
            Interaction::from_hover_focused(true, true),
            Interaction::FocusedHovered
        );

        let theme = Theme::default();
        // Focus always wins the *channel* question: it marks the entry.
        for focused in [Interaction::Focused, Interaction::FocusedHovered] {
            assert!(focused.is_focused());
        }
        // Hover always answers the *pointer* question, focus or not.
        assert!(Interaction::FocusedHovered.is_hovered());
        assert!(Interaction::Hovered.is_hovered());
        assert!(!Interaction::Focused.is_hovered());
        assert!(!Interaction::Idle.is_hovered());

        // …which is exactly the pair of answers the composed tone needs: the
        // keyboard tells nobody anything about the pointer, and vice versa.
        assert_eq!(Interaction::Focused.color(&theme), None);
        assert_eq!(
            Interaction::FocusedHovered.color(&theme),
            Some(theme.affordance())
        );
        assert_eq!(Interaction::Hovered.color(&theme), Some(theme.affordance()));
        assert_eq!(Interaction::Idle.color(&theme), None);
    }

    /// Discrete ANSI 16 themes must not blend discrete tokens into RGB floats that collapse to Gray.
    ///
    /// Focus speaks through the marker cell on this archetype too (a named
    /// slot), so the *tint* assertions belong to hover; what focus must preserve
    /// is that it never fabricates an RGB value the 16-color terminal cannot
    /// render.
    #[test]
    fn ansi16_interaction_preserves_discrete_yellow() {
        let theme = Theme::ansi16();
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Idle, &theme),
            Color::Gray
        );
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Hovered, &theme),
            Color::Yellow
        );
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Focused, &theme),
            Color::Gray,
            "focus rides the marker, so the text stays on its discrete rung"
        );
        // The ANSI-16 marker is a named token, not an RGB mix.
        assert!(!matches!(theme.pane_marker_style().fg, Color::Rgb(..)));
    }
}
