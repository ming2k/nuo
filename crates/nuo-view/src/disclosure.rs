//! Step state machine: the three orthogonal axes that determine a step's
//! presentation, and the pure functions that reduce them to color.
use nuotc::{Color, Modifier, Style};

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
/// | [`Focused`](Interaction::Focused) | "does the keyboard own this?" | focus **band** |
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
    /// entry is its highlighted target). Resolves to a full-width focus band,
    /// the same background channel the composer uses to say the same thing
    /// ([`Theme::entry_focus_style`]).
    Focused,
}

impl Interaction {
    /// Build from the raw interaction flags produced by the call site.
    ///
    /// Priority: **focus** beats **hover** beats **idle**. Focus outranks hover
    /// because the two answer different questions — "the keyboard is here" is a
    /// stronger, more actionable fact than "the pointer is here", and the focus
    /// cue must not be diluted by a coincident pointer.
    pub fn from_hover_focused(hovered: bool, focused: bool) -> Self {
        if focused {
            Interaction::Focused
        } else if hovered {
            Interaction::Hovered
        } else {
            Interaction::Idle
        }
    }

    /// The transient affordance **hue** for the *hover* question. Keyboard
    /// focus deliberately contributes nothing here: it answers a different
    /// question and speaks through [`Self::focus_style`] instead, so a focused
    /// entry never has to compete with its own pointer cue.
    pub fn color(self, theme: &Theme) -> Option<Color> {
        match self {
            Interaction::Idle | Interaction::Focused => None,
            Interaction::Hovered => Some(theme.affordance()),
        }
    }

    /// Whether the keyboard owns this entry.
    pub const fn is_focused(self) -> bool {
        matches!(self, Interaction::Focused)
    }

    /// The **background** style of the entry's header row.
    ///
    /// This is the channel the composer has always used to answer "who owns the
    /// keyboard?" (`input_surface` vs `input_surface_inactive`). The transcript
    /// is its peer plane (ADR-0145), so it must answer in the same channel at
    /// the same weight — otherwise `Shift-Tab` only dims one side and lights
    /// nothing on the other. `None` means "paint the ordinary surface" and is
    /// what every non-focused state returns, so an unfocused transcript is
    /// pixel-identical to the pre-focus-band rendering.
    ///
    /// Scoped to *every* disclosure state, including expanded: keyboard
    /// ownership is not a disclosure fact, and suppression is precisely the bug
    /// this band exists to fix.
    pub fn focus_style(self, theme: &Theme) -> Option<Style> {
        self.is_focused().then(|| theme.entry_focus_style())
    }

    /// Resolve the entry header's chrome in one step.
    pub fn chrome(self, theme: &Theme) -> EntryChrome {
        EntryChrome::resolve(self, theme)
    }
}

/// The resolved background chrome of an entry's header row.
///
/// Bundles the two things a header row needs to answer "who owns the keyboard?"
/// without each renderer re-deriving them:
///
/// 1. the **band** to paint behind the row (`None` = leave the ambient surface),
/// 2. the **foreground adaptation** that band forces — the summary tone is
///    picked for the page, not for the band, so on a focused row it must be
///    re-seated against the band it now sits on or the text loses contrast at
///    exactly the moment the user is looking at it.
///
/// Producing both from one value is the point: a renderer can paint a band it
/// forgot to re-seat text on, but it cannot fabricate an [`EntryChrome`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryChrome {
    band: Option<Color>,
    /// The DEC VT100 cue: on the `Structured` archetype there is no background
    /// latitude, so ownership is reverse video (ADR-0053) and [`Self::band`]
    /// stays `None` — a `Reset` bg would otherwise read as "paint the ambient
    /// surface", which is the correct *paint* but the wrong *cue*.
    reverse: bool,
}

impl EntryChrome {
    /// No focus chrome: the row paints the ambient surface, exactly as it did
    /// before the focus band existed. The value unfocused rows resolve to, and
    /// the one every non-focus-aware caller can name explicitly.
    pub const fn plain() -> Self {
        Self {
            band: None,
            reverse: false,
        }
    }

    fn resolve(interaction: Interaction, theme: &Theme) -> Self {
        if !interaction.is_focused() {
            return Self::plain();
        }
        let reverse = theme
            .entry_focus_style()
            .add
            .contains(nuotc::Modifier::REVERSE);
        Self {
            // On reverse video the band is carried by the modifier, so the
            // background stays the ambient surface — claiming a `Reset` band
            // would make [`Self::text`] re-seat the tone against nothing.
            band: (!reverse).then(|| theme.entry_focus_band()),
            reverse,
        }
    }

    /// The band to paint behind the row, or `None` for the ambient surface.
    pub fn band(self) -> Option<Color> {
        self.band
    }

    /// Whether the row is painted in reverse video (the VT100 cue).
    pub fn is_reverse(self) -> bool {
        self.reverse
    }

    /// The background style for the row's spans. `None` on an unfocused row, so
    /// callers keep whatever surface they already paint and an unfocused
    /// transcript stays byte-identical to the pre-focus-band rendering.
    pub fn span_style(self) -> Option<Style> {
        match (self.band, self.reverse) {
            (_, true) => Some(Style::default().add_modifier(Modifier::REVERSE)),
            (Some(band), false) => Some(Style::default().bg(band)),
            (None, false) => None,
        }
    }

    /// Re-seat a foreground tone so it stays legible on the resolved band.
    ///
    /// A no-op on an unfocused row (the tone was chosen for the page and the
    /// page has not changed) and on reverse video (the terminal swaps the pair
    /// itself).
    pub fn text(self, theme: &Theme, preferred: Color) -> Color {
        match self.band {
            Some(band) if !self.reverse => theme.band_text(band, preferred),
            _ => preferred,
        }
    }

    /// Paint `chrome` onto an already-built header `line` and pad it to
    /// `full_width`, so the band spans the whole row.
    ///
    /// This is the one place a renderer has to touch, so the band cannot be
    /// applied to some spans and forgotten on others — the failure mode that
    /// leaves the right-hand timestamp on the page background while the summary
    /// sits in a band. Every span inherits the band; a span that already carries
    /// an explicit background (a themed chip) keeps it.
    ///
    /// A no-op when [`Self::span_style`] is `None`: an unfocused entry renders
    /// exactly as it did before the focus band existed, padding included. A band
    /// that is shorter than the row reads as a ragged highlight rather than as
    /// ownership, so the tail is filled to `full_width`.
    pub fn apply(self, mut line: nuotc::Line<'static>, full_width: usize) -> nuotc::Line<'static> {
        let Some(style) = self.span_style() else {
            return line;
        };
        for span in &mut line.spans {
            // Only the *background* is imposed here: a span's foreground is a
            // deliberate semantic choice, re-seated through [`Self::text`] by the
            // caller when the band demands it.
            if span.style.bg == Color::Reset {
                span.style.bg = style.bg;
            }
            if self.reverse {
                span.style = span.style.add_modifier(Modifier::REVERSE);
            }
        }
        let used: usize = line
            .spans
            .iter()
            .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        let tail = full_width.saturating_sub(used);
        if tail > 0 {
            line.spans
                .push(nuotc::Span::styled(" ".repeat(tail), style));
        }
        line
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
    /// it answers a different question and speaks through the focus band
    /// instead (see `focus_is_a_band_not_a_tint`), so a focused row never has to
    /// share a channel with "the pointer is here".
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

    /// Keyboard focus is a **band**, not a tint.
    ///
    /// This is the core of the fix: the transcript answers "who owns the
    /// keyboard?" in the same channel the composer does (background), at the
    /// same weight, so `Shift-Tab` reads as a swap. A foreground tint cannot do
    /// that — it is invisible on an expanded step and easily out-shone by a
    /// lifecycle accent on a collapsed one.
    #[test]
    fn focus_is_a_band_not_a_tint() {
        let theme = Theme::default();
        let focused = Interaction::from_hover_focused(false, true);
        let hovered = Interaction::from_hover_focused(true, false);

        // The band exists for focus, and only for focus.
        let band = focused
            .chrome(&theme)
            .band()
            .expect("keyboard focus must produce a band");
        assert_eq!(band, theme.entry_focus_band());
        assert_eq!(
            hovered.chrome(&theme).band(),
            None,
            "a hovered entry keeps its ambient surface: nothing moved the keyboard"
        );
        assert_eq!(Interaction::Idle.chrome(&theme).band(), None);

        // The band is a *visible* step off the page — otherwise the cue is
        // nominal, which is the defect this test exists to prevent.
        assert_ne!(
            band,
            theme.surface(),
            "the band must not melt into the page"
        );

        // The tint channel is now hover-only, so focus never has to compete
        // with its own pointer cue for the same few characters.
        assert_eq!(focused.color(&theme), None);
        assert_eq!(hovered.color(&theme), Some(theme.affordance()));
    }

    /// The band is a keyboard-ownership cue, so it must not be the generic
    /// selection tone: a user who selected text must never mistake their own
    /// selection for "the keyboard is on this entry", and vice versa.
    #[test]
    fn focus_band_is_not_the_selection_tone() {
        let theme = Theme::default();
        assert_ne!(theme.entry_focus_band(), theme.selected());
        assert_ne!(theme.entry_focus_band(), theme.surface());
        assert_ne!(theme.entry_focus_band(), theme.input_surface_inactive());
    }

    /// The band survives on **every** disclosure state, including expanded —
    /// the exact case where the old affordance-hue cue was suppressed and the
    /// crossing vanished. Keyboard ownership is not a disclosure fact.
    #[test]
    fn focus_band_survives_an_expanded_entry() {
        let theme = Theme::default();
        for disclosure in [Disclosure::Collapsed, Disclosure::Expanded] {
            let chrome = Interaction::Focused.chrome(&theme);
            assert_eq!(
                chrome.band(),
                Some(theme.entry_focus_band()),
                "{disclosure:?}: keyboard ownership must not depend on disclosure"
            );
            // And the text is re-seated through the band's own contrast rule, so
            // a focused row cannot lose legibility at the moment it is looked at.
            let preferred = Color::Rgb(100, 100, 100);
            assert_eq!(
                chrome.text(&theme, preferred),
                theme.band_text(theme.entry_focus_band(), preferred),
                "{disclosure:?}: the focused row's tone must be resolved against its band"
            );
            assert_ne!(
                chrome.text(&theme, preferred),
                preferred,
                "a tone too close to the band must be flipped for contrast"
            );
        }
    }

    /// Unfocused entries must render exactly as they did before the band
    /// existed — the focus cue is additive state, not a restyling of the whole
    /// transcript.
    #[test]
    fn unfocused_chrome_is_a_no_op() {
        let theme = Theme::default();
        for interaction in [Interaction::Idle, Interaction::Hovered] {
            let chrome = interaction.chrome(&theme);
            assert!(chrome.span_style().is_none());
            assert!(!chrome.is_reverse());
            let tone = theme.muted();
            assert_eq!(
                chrome.text(&theme, tone),
                tone,
                "an unfocused row keeps the tone chosen for the page"
            );
        }
        assert_eq!(EntryChrome::plain(), Interaction::Idle.chrome(&theme));
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
    /// focus, and that focus now paints a *band* (a background cue) rather than
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
        // Focus contributes no luminance: the tone is unchanged, the band is
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
    ///
    /// Focus is a band on this archetype too (reverse video), so the *tint*
    /// assertions belong to hover; what focus must preserve is that it never
    /// fabricates an RGB value the 16-color terminal cannot render.
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
            "focus rides the band, so the text stays on its discrete rung"
        );
        // The ANSI-16 focus band is a named token, not an RGB mix.
        assert!(!matches!(theme.entry_focus_band(), Color::Rgb(..)));
    }
}
