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
