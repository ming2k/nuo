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
mod tests;
