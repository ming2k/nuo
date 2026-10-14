//! Step state machine: the three orthogonal axes that determine a step's
//! presentation, and the pure functions that reduce them to color.
//!
//! See [`super`] for the full architectural overview; this module owns the
//! state types and the accent/weight/affordance resolution so they can be
//! unit-tested in isolation from rendering.

use nuotc::Color;

use super::Theme;

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
/// pointer / keyboard state. Never persisted. Resolves to the affordance
/// **hue** channel (ADR-0174), not a luminance rung.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interaction {
    /// Not under the pointer and not keyboard-focused.
    Idle,
    /// Pointer rests on the summary line — a soft hover affordance.
    Hovered,
    /// Keyboard focus ring is on this step. Resolves identically to
    /// [`Hovered`](Interaction::Hovered): both tint the summary toward the
    /// theme's affordance hue, so a focused step never silently changes
    /// color when the pointer leaves.
    Focused,
}

impl Interaction {
    /// Build from the raw interaction flags produced by the call site.
    ///
    /// Priority: **focus** beats **hover** beats **idle**. Focus wins over
    /// hover purely to keep the enum deterministic (both resolve to the same
    /// color in [`summary_weight`], so a focused step stays highlighted even
    /// after the pointer moves away).
    pub fn from_hover_focused(hovered: bool, focused: bool) -> Self {
        if focused {
            Interaction::Focused
        } else if hovered {
            Interaction::Hovered
        } else {
            Interaction::Idle
        }
    }
}

/// Summary-line **weight** (luminance) — a pure function of disclosure. This
/// is the "is it open?" channel only; it never depends on lifecycle or
/// interaction, so it cannot leak run-state into the brightness.
///
/// **Disclosure-first luminance, hue-separated interaction (ADR-0174).**
/// Disclosure picks the base tone; interaction no longer competes on
/// brightness. The old `muted < hover < fg` ladder made "hover" and "open"
/// two rungs of the *same* channel — the active (expanded) state was
/// structurally brighter than the affordance, so the hover cue always lost.
/// Now the affordance is its own hue channel ([`Interaction::color`], the
/// theme's affordance token) composed in [`summary_text_color`], and the
/// luminance ladder is just:
///
/// 1. **Expanded** → `theme.fg()`. An open body is the active state, carried
///    by the `+`/`-` marker and the body itself — no extra brightness.
/// 2. **Collapsed** → `theme.muted()`. A closed summary rests at muted.
pub fn summary_weight(disclosure: Disclosure, theme: &Theme) -> Color {
    match disclosure {
        Disclosure::Expanded => theme.fg(),
        Disclosure::Collapsed => theme.muted(),
    }
}

impl Interaction {
    /// The transient affordance **hue** (ADR-0174): the second presentation
    /// channel. Hover/focus resolve to the theme's affordance token — a
    /// *tint*, not a luminance step — so "this is interactive" is visually
    /// orthogonal to "this is open" and can never be out-shone by it. Idle
    /// contributes nothing.
    pub fn color(self, theme: &Theme) -> Option<Color> {
        match self {
            Interaction::Idle => None,
            Interaction::Hovered | Interaction::Focused => Some(theme.affordance()),
        }
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
/// - **Interaction affordance hue** (ADR-0174): hover/focus tint the result
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
const INTERACTION_HOVER_BLEND: f32 = 0.65;

#[cfg(test)]
mod tests {
    use super::*;

    /// The disclosure luminance rungs stay distinct, and the affordance hue
    /// is distinct from both of them — so "interactive" (hue) can never be
    /// confused with "open" or "idle" (luminance). This is the core invariant
    /// of the ADR-0174 channel separation.
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

    /// On a *collapsed* summary hover/focus is a hue cue: it tints the muted
    /// resting tone toward the affordance hue. Focus shares hover's result,
    /// so a focused step never changes color when the pointer leaves.
    #[test]
    fn collapsed_hover_focus_tints_to_affordance_hue() {
        let theme = Theme::default();
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Hovered, &theme),
            theme
                .muted()
                .blend(theme.affordance(), INTERACTION_HOVER_BLEND)
        );
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Focused, &theme),
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
    /// focus, but that focus is now a hue tint over the muted rung — still
    /// dimmer in luminance than the expanded fg — and once the pointer/focus
    /// leaves it reads as plain muted. An expanded step is therefore never
    /// dimmer than a closed one in any state.
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

    /// `from_hover_focused` priority: focus > hover > idle. The enum keeps
    /// focus distinct from hover for determinism, even though both resolve to
    /// the same affordance hue — so a focused step stays highlighted after
    /// the pointer leaves.
    #[test]
    fn focus_beats_hover_beats_idle() {
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
            Interaction::Focused
        );
    }

    /// Discrete ANSI 16 themes must not blend discrete tokens into RGB floats that collapse to Gray.
    #[test]
    fn ansi16_interaction_preserves_discrete_yellow() {
        let theme = Theme::ansi16();
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Idle, &theme),
            Color::Gray
        );
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Focused, &theme),
            Color::Yellow
        );
        assert_eq!(
            summary_text_color(None, Disclosure::Collapsed, Interaction::Hovered, &theme),
            Color::Yellow
        );
    }
}
