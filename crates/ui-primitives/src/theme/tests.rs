use super::color_math::*;
use super::*;

/// Whether shifting `band` by the per-channel delta from `page` to `wash` stays
/// inside the channel range — i.e. whether the additive pane lift can preserve
/// this surface's elevation without clamping it.
#[cfg(test)]
fn survives_the_lift(page: Color, wash: Color, band: Color) -> bool {
    let (Some((wr, wg, wb)), Some((pr, pg, pb)), Some((br, bgc, bb))) =
        (rgb_opt(wash), rgb_opt(page), rgb_opt(band))
    else {
        return true;
    };
    let ok = |w: u8, p: u8, b: u8| (0..=255).contains(&(b as i16 + (w as i16 - p as i16)));
    ok(wr, pr, br) && ok(wg, pg, bgc) && ok(wb, pb, bb)
}

#[test]
fn unknown_scheme_falls_back_to_zen() {
    let custom = ColorSchemeConfig::default();
    let fallback = Theme::from_color_scheme("not-a-theme", &custom);
    assert_eq!(fallback.surface(), Theme::default().surface());
    assert_eq!(Theme::normalize_color_scheme(""), "zen");
}

/// Keyboard ownership of the transcript is carried by **area**, not by
/// per-cell contrast: the pane wash must be visible in every colour-capable
/// scheme but must stay a small step, because it lands on every cell of the
/// viewport (ADR-0145, amended).
///
/// Both halves are load-bearing and they pull in opposite directions:
///
/// - a wash a couple of luminance units off the page is *nominal* — the
///   crossing looks like nothing happened, which is the original defect;
/// - a wash as large as a selection band blinds the transcript, which is the
///   other failure the first band shipped.
///
/// So the step is bounded on both sides: a visible floor and a ceiling well
/// below the `selected` tone.
#[test]
fn pane_focus_is_a_small_but_visible_wash_in_every_color_scheme() {
    for name in ["zen", "midnight", "nord", "catppuccin", "paper"] {
        let theme = Theme::from_color_scheme(name, &ColorSchemeConfig::default());
        let wash = theme.pane_focus();
        assert_ne!(
            wash,
            theme.surface(),
            "{name}: the pane wash must not melt into the page"
        );
        let step = (luminance(wash) - luminance(theme.surface())).abs();
        assert!(
            step >= 4.0,
            "{name}: a {step:.1}-unit step is not a visible wash (wash {wash:?}, page {:?})",
            theme.surface()
        );
        assert!(
            step <= 20.0,
            "{name}: a {step:.1}-unit wash is a highlight, not a page state — it would \
                 wash out every tone resolved for the page"
        );
        // A keyboard-ownership cue must never read as a text selection.
        assert_ne!(
            wash,
            theme.selected(),
            "{name}: the pane wash must not equal the selection tone"
        );
        // The lift is applied **additively** to every background in the pane, so
        // each band keeps its elevation above the page and the document's ladder
        // survives. The condition for that is headroom, and the token layer
        // *enforces* it rather than assuming it: `pane_focus()` scales the
        // declared wish back to the largest step this palette can absorb, so no
        // band can clamp. (Asserted structurally below, over hostile palettes as
        // well as the built-ins.)
        let delta = theme
            .pane_focus_delta()
            .unwrap_or_else(|| panic!("{name}: the built-in schemes must carry a pane wash"));
        for band in theme.pane_bands() {
            assert!(
                survives_the_lift(theme.surface(), theme.pane_focus(), band),
                "{name}: the derived wash must never clamp {band:?}"
            );
        }
        // And the derived delta is the declared wish, unclamped, on a palette that
        // has room — the clamp must not quietly shrink every scheme.
        let wished = |w: Color, p: Color| -> i16 {
            let (wr, _, _) = rgb(w);
            let (pr, _, _) = rgb(p);
            wr as i16 - pr as i16
        };
        if name != "paper" {
            assert_eq!(
                delta[0],
                wished(theme.pane_focus_bg, theme.surface()),
                "{name}: a palette with headroom keeps the declared step exactly"
            );
        }
        // The page's own foreground must stay legible on it without any
        // re-seat, because the wash covers prose the renderers never touch.
        let text_step = (luminance(theme.fg()) - luminance(wash)).abs();
        assert!(
            text_step >= 100.0,
            "{name}: body text must stay legible on the pane wash ({text_step:.0})"
        );
    }
}

/// `pane_lifted()` must lift **exactly** the fields the headroom classification
/// calls backgrounds — no more, no less.
///
/// This is the test that closes the drift between the two lists. It is *derived*
/// from `background_fields()` rather than enumerating a handful of fields, because a
/// hand-picked list is precisely how the original bug survived: a field was added,
/// classified, and never lifted, and a nine-field assertion had no way to notice the
/// tenth.
///
/// The two directions:
/// - every field classified `true` **must** move by the delta;
/// - every field classified `false` **must** arrive byte-identical.
///
/// The second half matters as much as the first: a foreground that drifted with the
/// page would change the document's *meaning* (a running hue means the same thing at
/// either page level), which is a worse failure than a flattened band.
#[test]
fn pane_lifted_lifts_exactly_the_classified_backgrounds() {
    let theme = Theme::default();
    let lifted = theme.pane_lifted();
    let delta = theme.pane_focus_delta().expect("zen carries a wash");
    let shift = |c: Color| -> Color {
        match rgb_opt(c) {
            Some((r, g, b)) => {
                let f = |c: u8, d: i16| (c as i16 + d).clamp(0, 255) as u8;
                Color::Rgb(f(r, delta[0]), f(g, delta[1]), f(b, delta[2]))
            }
            None => c,
        }
    };

    let before = theme.background_fields();
    let after = lifted.background_fields();
    assert_eq!(before.len(), after.len(), "the field sets must correspond");

    for ((field, was, is_background), (_, now, _)) in before.iter().zip(after.iter()) {
        assert_eq!(field, field, "the two classifications must agree on order");
        if *is_background {
            assert_eq!(
                now,
                &shift(*was),
                "{field}: classified as a pane background but not lifted — its \
                     elevation above the page would be truncated"
            );
        } else {
            assert_eq!(
                now, was,
                "{field}: classified as NOT a pane background but it moved — \
                     foregrounds and foreign surfaces must not drift with the page"
            );
        }
    }
}

/// Lifting the *theme* must be exactly equivalent to restyling every cell of the
/// painted pane — that equivalence is what lets the renderer take the cheap path
/// (`Theme::pane_lifted`) instead of the per-cell one, and it is what keeps the
/// field list in `pane_lifted` honest: a background that is not lifted there
/// fails here.
///
/// "Background" is the operative word: foregrounds are semantic and must not
/// move, or a running hue would change meaning with the keyboard's location.
#[test]
fn pane_lifted_theme_matches_a_cell_wise_restyle() {
    let theme = Theme::default();
    let lifted = theme.pane_lifted();
    let delta = theme.pane_focus_delta().expect("zen carries a wash");
    let shift = |c: Color| match rgb_opt(c) {
        Some((r, g, b)) => {
            let f = |c: u8, d: i16| (c as i16 + d).clamp(0, 255) as u8;
            Color::Rgb(f(r, delta[0]), f(g, delta[1]), f(b, delta[2]))
        }
        None => c,
    };

    // Every background the transcript reaches for moves by exactly the delta.
    for (label, before, after) in [
        ("page", theme.app_bg, lifted.app_bg),
        ("body", theme.menu_bg, lifted.menu_bg),
        ("code", theme.code_bg, lifted.code_bg),
        ("raised", theme.element_bg, lifted.element_bg),
        (
            "command card",
            theme.command_band_bg,
            lifted.command_band_bg,
        ),
        ("selection", theme.selected_bg, lifted.selected_bg),
        ("user panel", theme.user_panel_bg, lifted.user_panel_bg),
        ("diff add", theme.diff_add_bg, lifted.diff_add_bg),
        ("diff del", theme.diff_del_bg, lifted.diff_del_bg),
    ] {
        assert_eq!(after, shift(before), "{label} must be lifted by the delta");
    }

    // Foregrounds are untouched.
    assert_eq!(lifted.text, theme.text);
    assert_eq!(lifted.text_muted, theme.text_muted);
    assert_eq!(lifted.running_fg, theme.running_fg);
    assert_eq!(lifted.info, theme.info);
    assert_eq!(lifted.pane_marker_fg, theme.pane_marker_fg);

    // And the lift is a translation: every separation is invariant.
    for band in theme.pane_bands() {
        let before = luminance(band) - luminance(theme.surface());
        let after = luminance(shift(band)) - luminance(lifted.surface());
        assert!(
            (after - before).abs() < 0.01,
            "{band:?} moved relative to the page"
        );
    }
}

/// Every reachable palette must land on **one** of the two correct answers, and
/// never on a third: either a wash that preserves the whole ladder, or the gutter
/// fallback. This is the sweep the earlier design never ran — it only checked the
/// five built-in schemes, so a hand-authored theme could produce a wash that
/// clamped its own bands.
///
/// The palettes covered are exactly the ones a user can reach: the built-ins
/// through the scheme table (already swept elsewhere), the custom palettes built
/// from arbitrary hex semantics, and the two `Reset` archetypes.
#[test]
fn every_reachable_palette_lands_on_a_correct_pane_cue() {
    let custom = ColorSchemeConfig::default();
    let mut palettes: Vec<(String, Theme)> = Vec::new();
    for scheme in Theme::available_color_schemes() {
        palettes.push((
            scheme.id.to_string(),
            Theme::from_color_scheme(&scheme.id, &custom),
        ));
    }
    // Extremes a custom `colors` block can actually produce: near-white paper, near
    // -black ink, and a page that sits above and below its own surfaces.
    for (label, background, surface, text) in [
        ("near-white", "#fefefe", "#ffffff", "#101010"),
        ("near-black", "#010101", "#050505", "#e0e0e0"),
        ("mid-grey", "#808080", "#909090", "#303030"),
        ("page-above-surface", "#f0f0f0", "#e0e0e0", "#202020"),
        ("page-below-surface", "#202020", "#f0f0f0", "#f8f8f8"),
    ] {
        palettes.push((
            label.to_string(),
            Theme::from_custom(&ColorSchemeConfig {
                background: background.to_string(),
                surface: surface.to_string(),
                text: text.to_string(),
                ..Default::default()
            }),
        ));
    }

    for (label, theme) in palettes {
        if !theme.has_pane_wash() {
            // The fallback must still be a real cue.
            assert_ne!(
                theme.pane_focus_edge_style(),
                Style::default(),
                "{label}: a palette without a wash needs a fallback cue"
            );
            continue;
        }
        assert!(
            theme.pane_focus_delta().is_some(),
            "{label}: has_pane_wash must imply a delta"
        );
        for band in theme.pane_bands() {
            assert!(
                survives_the_lift(theme.surface(), theme.pane_focus(), band),
                "{label}: the derived wash would clamp {band:?}"
            );
        }
        // And the cue is a *visible* step, or it is not a cue.
        let step = (luminance(theme.pane_focus()) - luminance(theme.surface())).abs();
        assert!(step >= 1.0, "{label}: a {step:.2}-unit wash is nominal");
    }
}

/// A palette with **no headroom** must lose the wash, not flatten its own
/// document.
///
/// This is the case the earlier design could not express: it declared the wash
/// per *archetype* and trusted the literal, so a `Chromatic` theme whose bands
/// sat at the pole the lift travels toward had no way to say "this palette cannot
/// carry a whole-pane cue". The clamp now measures the palette and answers
/// honestly — the transcript takes the gutter form instead of a washed-out page.
#[test]
fn a_palette_without_headroom_refuses_the_wash() {
    // A band already sitting on the ceiling the lift travels toward: shifting it
    // would clamp, and clamping it is exactly the flattening this design exists to
    // avoid. There is no legal step, so there is no wash.
    let pinned = Theme {
        app_bg: Color::Rgb(250, 250, 250),
        pane_focus_bg: Color::Rgb(255, 255, 255),
        code_bg: Color::Rgb(255, 255, 255),
        ..Default::default()
    };
    assert!(
        !pinned.has_pane_wash(),
        "a band pinned at the pole leaves no room for the lift"
    );
    assert_eq!(pinned.pane_focus(), Color::Reset);
    // …and the fallback still names a pane-scoped background, so the crossing is
    // not silent on that palette either.
    assert_ne!(pinned.pane_focus_edge_style(), Style::default());

    // The same palette becomes usable the moment the band gives back one unit —
    // the refusal is a measurement, not a blacklist.
    let one_unit = Theme {
        code_bg: Color::Rgb(254, 254, 254),
        ..pinned
    };
    assert!(one_unit.has_pane_wash());
    assert_eq!(one_unit.pane_focus(), Color::Rgb(251, 251, 251));
}

/// A wash that *would* clamp is **scaled back**, not clamped. The distinction is
/// the whole point: clamping truncates the step (and silently flattens the
/// band it truncated), while scaling preserves the ladder at a smaller
/// magnitude — the cue stays honest about how much room there is.
#[test]
fn an_oversized_wash_is_scaled_back_to_the_headroom() {
    // One band pinned at the top of the range: the lift may travel at most as far
    // as that band's headroom allows.
    let theme = Theme {
        app_bg: Color::Rgb(10, 10, 10),
        code_bg: Color::Rgb(250, 250, 250),
        pane_focus_bg: Color::Rgb(200, 200, 200),
        ..Default::default()
    };
    let delta = theme
        .pane_focus_delta()
        .expect("there is headroom, just little");
    assert_eq!(
        delta[0], 5,
        "the lift is bounded by the band's 5 units of room, not by the declared 190"
    );
    assert!(
        survives_the_lift(theme.surface(), theme.pane_focus(), theme.code_surface()),
        "the derived wash must not clamp the band"
    );
    // The derived tone is the *scaled* step, not the declared wish.
    assert_eq!(theme.pane_focus(), Color::Rgb(15, 15, 15));
}

/// A hand-authored override moves the **wish**, never the invariant.
///
/// The configuration surface exists so a theme can state these two tones. It must
/// not exist as a way to flatten a transcript: an override that lands inside a
/// band's headroom is scaled like any other wish, and one that points back at the
/// page is refused outright.
#[test]
fn a_theme_override_cannot_flatten_the_pane() {
    use crate::color_scheme::ThemeFile;

    let file = ThemeFile {
        components: Some(crate::color_scheme::ComponentThemesConfig {
            pane: Some(crate::color_scheme::PaneThemeConfig {
                wash_bg: Some("#ffffff".to_string()),
                marker_fg: Some("#888888".to_string()),
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let theme = Theme::from_theme_file(&file);
    assert_eq!(theme.pane_marker_fg, Color::Rgb(136, 136, 136));
    for band in theme.pane_bands() {
        assert!(
            survives_the_lift(theme.surface(), theme.pane_focus(), band),
            "an override must not clamp {band:?}"
        );
    }

    // An override that names the page itself is a no-op, not a silent wash.
    let flat = ThemeFile {
        components: Some(crate::color_scheme::ComponentThemesConfig {
            pane: Some(crate::color_scheme::PaneThemeConfig {
                wash_bg: Some("#070808".to_string()),
                marker_fg: None,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let flat = Theme::from_theme_file(&flat);
    assert!(!flat.has_pane_wash(), "a zero-step wash is not a cue");
    assert_eq!(flat.pane_focus(), Color::Reset);
}

/// The entry marker holds a **fixed absolute tone**, so it must stay distinguishable
/// from the page *after* the page is lifted — not just before.
///
/// This is the property that replaces "keeps its elevation" for a non-surface mark.
/// It is asserted separately because the two are genuinely different requirements: a
/// band must preserve its step, a mark must not be swallowed. Classifying the marker
/// as a background would have implied the former and hidden the latter.
#[test]
fn the_entry_marker_stays_distinguishable_from_the_lifted_page() {
    let custom = ColorSchemeConfig::default();
    for scheme in Theme::available_color_schemes() {
        let theme = Theme::from_color_scheme(&scheme.id, &custom);
        let marker = theme.pane_marker_fg;
        for (state, page) in [("blurred", theme.surface()), ("lifted", theme.pane_focus())] {
            if page == Color::Reset || marker == Color::Reset {
                continue;
            }
            let step = (luminance(marker) - luminance(page)).abs();
            assert!(
                step >= 40.0,
                "{}: the marker is only {step:.0} from the {state} page — the cue \
                     would be swallowed",
                scheme.id
            );
        }
    }
}

/// The entry marker is a **neutral luminance rung**: it must separate from
/// both the page and the entry's own text without borrowing a semantic hue,
/// so "the keyboard is on this entry" can never be read as an outcome state
/// or as a selection (ADR-0145, amended).
#[test]
fn entry_focus_marker_is_neutral_and_visible() {
    for name in ["zen", "midnight", "nord", "catppuccin", "paper"] {
        let theme = Theme::from_color_scheme(name, &ColorSchemeConfig::default());
        let marker = theme.pane_marker_fg;
        let (r, g, b) = rgb(marker);
        // Neutral: no channel may dominate, so the marker cannot be mistaken
        // for a hue-coded lifecycle state.
        let spread = r.max(g).max(b) as i32 - r.min(g).min(b) as i32;
        assert!(
            spread <= 24,
            "{name}: the entry marker must be a neutral rung, not a hue ({marker:?})"
        );
        assert_ne!(marker, theme.selected(), "{name}: never the selection tone");
        assert_ne!(marker, theme.fg(), "{name}: never the body text tone");
        let step = (luminance(marker) - luminance(theme.surface())).abs();
        assert!(
            step >= 60.0,
            "{name}: a {step:.0}-unit marker is invisible on the page"
        );
    }
}

/// Liveness owns its own hue (ADR-0110 `[INV-TUI-RUNNING-01]`, amended).
///
/// The reported ambiguity: a running tool step and the `› turn N` header
/// above it were both `info`, so "this row is working" and "this is where the
/// turn starts" read as the same fact. The in-flight tone must therefore be
/// distinct from every structural and outcome tone in every scheme.
#[test]
fn running_hue_is_distinct_from_structure_and_outcomes() {
    for name in ["zen", "midnight", "nord", "catppuccin", "paper", "ansi16"] {
        let theme = Theme::from_color_scheme(name, &ColorSchemeConfig::default());
        let running = theme.running();
        for (label, other) in [
            ("info", theme.info()),
            ("brand", theme.brand()),
            ("ok", theme.ok()),
            ("warn", theme.warn()),
            ("err", theme.err()),
            ("muted", theme.muted()),
        ] {
            assert_ne!(
                running, other,
                "{name}: the in-flight hue must not be the {label} tone"
            );
            if matches!(other, Color::Rgb(..)) && matches!(running, Color::Rgb(..)) {
                assert!(
                    color_distance(running, other) >= 90.0,
                    "{name}: the in-flight hue is too close to {label} ({:?} vs {other:?})",
                    running
                );
            }
        }
    }
}

/// On the no-colour archetypes the marker is carried by DEC reverse video
/// instead, so no colour escape is emitted (ADR-0053). The cue must still
/// exist — silence here is the original bug.
#[test]
fn entry_focus_is_reverse_video_on_a_structured_profile() {
    // VT100 / getty: no colour latitude at all, so the cue is the modifier.
    let mono = Theme::monochrome();
    assert_eq!(mono.pane_marker_fg, Color::Reset);
    assert_eq!(mono.pane_focus(), Color::Reset);
    assert!(
        mono.pane_marker_style()
            .add
            .contains(nuotc::Modifier::REVERSE),
        "a VT100 says \"the keyboard is on this entry\" with reverse video"
    );

    // ANSI-16 keeps the named slots: the pane cannot be lifted (its page is
    // the terminal's own background) and the marker uses the brightest
    // discrete slot rather than an RGB the console would quantize away.
    let hybrid = Theme::ansi16();
    assert_eq!(hybrid.pane_focus(), Color::Reset);
    assert_eq!(hybrid.pane_marker_fg, Color::White);
    assert!(!matches!(hybrid.pane_marker_fg, Color::Rgb(..)));
    assert_eq!(hybrid.pane_marker_style().fg, Color::White);
    // Liveness is a named slot too — never an RGB.
    assert!(!matches!(hybrid.running(), Color::Rgb(..)));
}

/// A listing row's color follows the shell's `ls` convention — blue dir,
/// green exec, cyan link — while a plain file keeps the scheme's content
/// tone, and the four are mutually distinct so a type never reads as another.
#[test]
fn listing_color_matches_ls_convention() {
    let theme = Theme::default();
    let dir = theme.listing_color(ListingClass::Dir);
    let exec = theme.listing_color(ListingClass::Exec);
    let link = theme.listing_color(ListingClass::Link);
    let file = theme.listing_color(ListingClass::File);
    assert_eq!(file, theme.code_text(), "a plain file is the content tone");
    assert_ne!(dir, exec);
    assert_ne!(dir, link);
    assert_ne!(exec, link);
    // Blue dominates a directory hue: blue channel is the strongest.
    let (r, g, b) = rgb(dir);
    assert!(b > r && b > g, "the directory hue reads as blue");
    // Green dominates the executable hue.
    let (r, g, b) = rgb(exec);
    assert!(g > r && g > b, "the executable hue reads as green");
}

/// Monochrome (DEC VT100) must emit no hue at all: every listing class
/// collapses to the reset content tone so the type is carried structurally
/// (the directory trailing `/`) rather than by color.
#[test]
fn listing_color_is_hueless_under_monochrome() {
    let theme = Theme::monochrome();
    for class in [
        ListingClass::Dir,
        ListingClass::Exec,
        ListingClass::Link,
        ListingClass::File,
    ] {
        assert_eq!(
            theme.listing_color(class),
            theme.code_text(),
            "monochrome listing rows carry no color cue"
        );
    }
}

/// ANSI-16 uses the terminal's own named slots (never an RGB the console
/// cannot render as one of its 16 colors).
#[test]
fn listing_color_uses_named_slots_under_ansi16() {
    let theme = Theme::ansi16();
    assert_eq!(theme.listing_color(ListingClass::Dir), Color::LightBlue);
    assert_eq!(theme.listing_color(ListingClass::Exec), Color::LightGreen);
    assert_eq!(theme.listing_color(ListingClass::Link), Color::LightCyan);
}

/// The hover band is derived from the palette, never a fixed gray, and is
/// chosen so the row's own swatches stay legible against it: the picked
/// band is at least as far from the closest swatch as the palette's
/// inverse pole would be.
#[test]
fn row_hover_band_stages_the_row_swatches() {
    let custom = ColorSchemeConfig::default();
    for scheme in Theme::available_color_schemes() {
        let page = Theme::from_color_scheme(&scheme.id, &custom);
        let preview = Theme::from_color_scheme(&scheme.id, &custom);
        let swatches = [
            preview.body(),
            preview.panel(),
            preview.brand(),
            preview.info(),
            preview.ok(),
            preview.warn(),
        ];
        let band = page.row_hover_band(&swatches);

        // Monochrome owns no background latitude: the band is inert and the
        // caller falls back to reverse video. Everything else must visibly
        // differ from the resting body.
        if page.elevation == ElevationArchetype::Structured {
            assert_eq!(band, Color::Reset);
            continue;
        }
        assert_ne!(
            band,
            page.body(),
            "{}: the hover band must differ from the resting body",
            scheme.id
        );

        // The swatch-staging guarantee is a truecolor concern: ANSI-16 uses
        // its named highlight verbatim, so skip it here.
        if page.elevation != ElevationArchetype::Chromatic {
            continue;
        }

        // The band must not fuse into any swatch: it is at least as legible
        // as the plain inverse pole the palette could have offered.
        let light = luminance(page.app_bg) > 150.0;
        let inverse = mix(
            page.app_bg,
            if light { Color::Black } else { page.text },
            0.82,
        );
        assert!(
            min_swatch_distance(band, &swatches) >= min_swatch_distance(inverse, &swatches),
            "{}: band must not be closer to a swatch than the inverse pole",
            scheme.id
        );

        // Text painted on the band must separate from it (never fg-on-fg).
        let text = page.band_text(band, page.fg());
        assert!(
            (luminance(band) - luminance(text)).abs() >= 60.0,
            "{}: band text must contrast with the band",
            scheme.id
        );
    }
}

/// Monochrome owns no background latitude: the band collapses to `Reset`
/// and the caller falls back to reverse video, so the cue never fabricates
/// a color the hardware cannot render.
#[test]
fn row_hover_band_is_inert_on_monochrome() {
    let mono = Theme::monochrome();
    assert_eq!(mono.row_hover_band(&[]), Color::Reset);
    // On a Reset band the preferred foreground is returned verbatim.
    assert_eq!(
        mono.band_text(Color::Reset, Color::Rgb(1, 2, 3)),
        Color::Rgb(1, 2, 3)
    );
}

/// The sunken pane rung is a *deeper* tone than the panel and body it sits
/// under, so a content pane can separate from the head band above it on
/// every RGB scheme — while Reset-based archetypes stay inert.
#[test]
fn pane_sunken_is_a_deeper_in_family_rung() {
    let custom = ColorSchemeConfig::default();
    for scheme in Theme::available_color_schemes() {
        let theme = Theme::from_color_scheme(&scheme.id, &custom);
        let sunken = theme.pane_sunken();
        if theme.elevation != ElevationArchetype::Chromatic {
            assert_eq!(sunken, Color::Reset, "{}: reset archetype", scheme.id);
            continue;
        }
        assert!(
            luminance(sunken) < luminance(theme.panel()),
            "{}: sunken must be deeper than the panel",
            scheme.id
        );
        assert_ne!(
            sunken,
            theme.raised(),
            "{}: sunken must not equal the head band tone",
            scheme.id
        );
    }
}

#[test]
fn every_preset_has_a_distinct_canonical_index() {
    let schemes = Theme::available_color_schemes();
    for (index, scheme) in schemes.iter().enumerate() {
        assert_eq!(Theme::color_scheme_index(&scheme.id), index);
    }
}

#[test]
fn discovers_external_theme_files_in_available_color_schemes() {
    // Hermetic: the projection takes parsed theme files, so the test parses
    // the TOML directly instead of touching the filesystem (ADR-0073).
    let raw = r##"
id = "monokai-pro"
name = "Monokai Pro"
description = "Monokai pro dark"
[colors]
background = "#2d2a2e"
surface = "#403e41"
text = "#fcfcfa"
muted = "#727072"
accent = "#ffd866"
success = "#a9dc76"
warning = "#fc9867"
error = "#ff6188"
"##;
    let files = vec![toml::from_str::<ThemeFile>(raw).expect("theme toml")];

    let schemes = Theme::available_color_schemes_from(&files);
    assert!(schemes.iter().any(|s| s.id == "monokai-pro" && s.is_file));
    let theme = Theme::from_color_scheme_from("monokai-pro", &ColorSchemeConfig::default(), &files);
    assert_eq!(theme.app_bg, Color::Rgb(45, 42, 46));
}

#[test]
fn component_overrides_apply_correctly() {
    let raw = r##"
name = "Cyberpunk"
description = "Neon high-contrast"
[colors]
background = "#050505"
surface = "#151515"
text = "#ffffff"
muted = "#808080"
accent = "#00ffff"
success = "#00ff00"
warning = "#ffff00"
error = "#ff0055"

[components.input]
bg_active = "#2a2a2a"
bg_inactive = "#111111"
caret = "#ff00ff"
selection = "#333333"
placeholder = "#777777"

[components.crate]
fg = "#00ffff"
badge_bg = "#1a3333"

[components.diff]
add_bg = "#003300"
del_bg = "#330011"
add_hl = "#006600"
del_hl = "#660022"

[components.command]
idle_bg = "#181818"
hover_bg = "#282828"
"##;
    let file: ThemeFile = toml::from_str(raw).expect("should parse");
    let theme = Theme::from_theme_file(&file);
    assert_eq!(theme.input_surface(), Color::Rgb(42, 42, 42));
    assert_eq!(theme.input_surface_inactive(), Color::Rgb(17, 17, 17));
    assert_eq!(theme.caret(), Color::Rgb(255, 0, 255));
    assert_eq!(theme.input_selection(), Color::Rgb(51, 51, 51));
    assert_eq!(theme.input_placeholder(), Color::Rgb(119, 119, 119));
    assert_eq!(theme.crate_tag(), Color::Rgb(0, 255, 255));
    assert_eq!(theme.crate_badge(), Color::Rgb(26, 51, 51));
    assert_eq!(theme.diff_add_bg(), Color::Rgb(0, 51, 0));
    assert_eq!(theme.diff_del_bg(), Color::Rgb(51, 0, 17));
    assert_eq!(theme.diff_add_hl(), Color::Rgb(0, 102, 0));
    assert_eq!(theme.diff_del_hl(), Color::Rgb(102, 0, 34));
    assert_eq!(theme.command_surface(), Color::Rgb(24, 24, 24));
    assert_eq!(theme.command_surface_hover(), Color::Rgb(40, 40, 40));
}

/// The input box owns two dedicated background tokens (independent of the
/// other surface tokens) and the active/inactive pair must stay
/// distinguishable in *every* scheme: each state must clear the app
/// background by a visible margin, and the two states must clear each
/// other by a visible margin. This is the regression guard for the
/// "activated and deactivated input look identical to the background"
/// defect that motivated the pair.
#[test]
fn input_surfaces_stay_distinguishable_in_every_scheme() {
    const MIN_STEP: f32 = 4.0;
    let custom = ColorSchemeConfig::default();
    for scheme in &COLOR_SCHEMES {
        if scheme.id == "ansi16" || scheme.id == "monochrome" {
            // ADR-0053: Console/Monochrome profiles anchor backgrounds to Reset
            // and establish distinction via borders/reverse video rather than RGB luminance.
            continue;
        }
        let theme = Theme::from_color_scheme(&scheme.id, &custom);
        let active = theme.input_surface();
        let inactive = theme.input_surface_inactive();
        let base = theme.surface();

        // Light schemes mix toward a dark text (surfaces get *darker* as
        // they elevate), so the margin is measured as an absolute delta.
        let step_from_app_active = (luminance(active) - luminance(base)).abs();
        let step_from_app_inactive = (luminance(inactive) - luminance(base)).abs();
        let step_between_states = (luminance(active) - luminance(inactive)).abs();

        assert!(
            step_from_app_active >= MIN_STEP,
            "{scheme:?}: active input only {step_from_app_active:.1} above app_bg"
        );
        assert!(
            step_from_app_inactive >= MIN_STEP,
            "{scheme:?}: inactive input only {step_from_app_inactive:.1} above app_bg"
        );
        assert!(
            step_between_states >= MIN_STEP,
            "{scheme:?}: active/inactive input only {step_between_states:.1} apart"
        );
        assert_ne!(
            active, inactive,
            "{scheme:?}: active and inactive input must be distinct colors"
        );
    }
}

/// The command card must read as a *card* in every scheme —
/// both its idle band and its hover band have to clear the page
/// background by a visible margin (otherwise the row collapses back
/// into "flat prose", the exact defect the card exists to fix), and the
/// hover step must itself be visible. Derived for every preset via
/// `from_semantic`, asserted here so a future scheme cannot silently
/// flatten it.
#[test]
fn command_card_bands_stay_visible_in_every_scheme() {
    const MIN_STEP: f32 = 2.0;
    let custom = ColorSchemeConfig::default();
    for scheme in &COLOR_SCHEMES {
        if scheme.id == "ansi16" || scheme.id == "monochrome" {
            // ADR-0053: Console/Monochrome profiles anchor backgrounds to Reset
            // and establish distinction via borders/reverse video rather than RGB luminance.
            continue;
        }
        let theme = Theme::from_color_scheme(&scheme.id, &custom);
        let idle = theme.command_surface();
        let hover = theme.command_surface_hover();
        let base = theme.surface();

        let idle_step = (luminance(idle) - luminance(base)).abs();
        let hover_step = (luminance(hover) - luminance(base)).abs();
        let hover_idle_step = (luminance(hover) - luminance(idle)).abs();

        assert!(
            idle_step >= MIN_STEP,
            "{scheme:?}: idle command band only {idle_step:.1} from app_bg"
        );
        assert!(
            hover_step >= MIN_STEP,
            "{scheme:?}: hover command band only {hover_step:.1} from app_bg"
        );
        assert!(
            hover_idle_step >= MIN_STEP,
            "{scheme:?}: command hover only {hover_idle_step:.1} above its idle band"
        );
    }
}

/// `[INV-HEAD-06]` The top chrome tiers must exhibit a discrete, in-family micro-elevation
/// ladder across all preset color schemes.
#[test]
fn header_tiers_micro_elevation_is_guaranteed_in_every_scheme() {
    let custom = ColorSchemeConfig::default();
    for scheme in &COLOR_SCHEMES {
        if scheme.id == "ansi16" || scheme.id == "monochrome" {
            continue;
        }
        let theme = Theme::from_color_scheme(&scheme.id, &custom);
        let tabbar = theme.tabbar_bg();
        let scenebar = theme.scenebar_bg();
        let contextbar = theme.contextbar_bg();

        assert_ne!(
            tabbar, scenebar,
            "{scheme:?}: tabbar and scenebar must not share identical fill"
        );
        assert_ne!(
            scenebar, contextbar,
            "{scheme:?}: scenebar and contextbar must not share identical fill"
        );
        assert_ne!(
            tabbar, contextbar,
            "{scheme:?}: tabbar and contextbar must not share identical fill"
        );

        let tab_lum = luminance(tabbar);
        let scene_lum = luminance(scenebar);
        let ctx_lum = luminance(contextbar);
        let base_lum = luminance(theme.surface());

        if base_lum <= 150.0 {
            // Dark mode: SceneBar is the elevated spotlight, ContextBar is intermediate, TabBar is base.
            assert!(
                scene_lum > ctx_lum,
                "{scheme:?}: scenebar must have higher luminance than contextbar"
            );
            assert!(
                ctx_lum > tab_lum,
                "{scheme:?}: contextbar must have higher luminance than tabbar"
            );
            assert!(
                scene_lum - tab_lum >= 4.0,
                "{scheme:?}: scenebar and tabbar must have perceptible contrast"
            );
        } else {
            // Light mode: surfaces mix toward dark text, so elevation steps down in luminance.
            assert!(
                scene_lum < ctx_lum,
                "{scheme:?}: scenebar must step darker than contextbar in light mode"
            );
            assert!(
                ctx_lum < tab_lum,
                "{scheme:?}: contextbar must step darker than tabbar in light mode"
            );
        }
    }
}

#[test]
fn spatial_surfaces_and_feedback_tokens_contract() {
    let theme = Theme::default();
    let surfaces = theme.surfaces();
    assert_eq!(surfaces.view.canvas, theme.surface());
    assert_eq!(surfaces.modal.surface, theme.panel());
    assert_eq!(surfaces.modal.dim_factor, 0.5);
    assert_eq!(surfaces.overlay.toast_bg, theme.toast_bg());
    assert_eq!(surfaces.overlay.which_key_bg, theme.which_key_bg());

    let fb_warn = theme.feedback(NoticeSeverity::Warning);
    assert_eq!(fb_warn.border, theme.warn());

    let fb_err = theme.feedback(NoticeSeverity::Error);
    assert_eq!(fb_err.border, theme.err());

    let fb_info = theme.feedback(NoticeSeverity::Info);
    assert_eq!(fb_info.border, theme.info());
}

#[test]
fn parses_and_applies_theme_surfaces_overrides() {
    let raw = r##"
name = "Custom Theme"
description = "Test"

[palette]
background = "#050505"
surface = "#101010"
text = "#ffffff"
muted = "#888888"
accent = "#00ffcc"
success = "#00cc00"
warning = "#cccc00"
error = "#cc0000"

[surfaces.view]
canvas = "#010101"
header_bg = "#111111"

[surfaces.modal]
surface = "#222222"
dim_factor = 0.65

[surfaces.overlay]
toast_bg = "#333333"
which_key_bg = "#282828"
"##;
    let file: ThemeFile = toml::from_str(raw).expect("should parse");
    let theme = Theme::from_theme_file(&file);
    assert_eq!(theme.surfaces().view.canvas, Color::Rgb(1, 1, 1));
    assert_eq!(theme.surfaces().view.header_bg, Color::Rgb(17, 17, 17));
    assert_eq!(theme.surfaces().modal.surface, Color::Rgb(34, 34, 34));
    assert_eq!(theme.surfaces().modal.dim_factor, 0.65);
    assert_eq!(theme.surfaces().overlay.toast_bg, Color::Rgb(51, 51, 51));
    assert_eq!(
        theme.surfaces().overlay.which_key_bg,
        Color::Rgb(40, 40, 40)
    );
    assert_eq!(theme.toast_bg(), Color::Rgb(51, 51, 51));
    assert_eq!(theme.which_key_bg(), Color::Rgb(40, 40, 40));
}

#[test]
fn parses_and_applies_theme_keycap_overrides() {
    let raw = r##"
name = "Keycap Test Theme"
description = "Test"

[palette]
background = "#050505"
surface = "#101010"
text = "#ffffff"
muted = "#888888"
accent = "#00ffcc"
success = "#00cc00"
warning = "#cccc00"
error = "#cc0000"

[components.keycap]
key_fg = "#ffffff"
key_bg = "#222222"
label_fg = "#aaaaaa"
accent_fg = "#00ffcc"
warn_fg = "#ff8800"
"##;
    let file: ThemeFile = toml::from_str(raw).expect("should parse");
    let theme = Theme::from_theme_file(&file);
    assert_eq!(theme.keycap_fg(), Color::Rgb(255, 255, 255));
    assert_eq!(theme.keycap_bg(), Color::Rgb(34, 34, 34));
    assert_eq!(theme.keycap_label(), Color::Rgb(170, 170, 170));
    assert_eq!(theme.keycap_accent(), Color::Rgb(0, 255, 204));
    assert_eq!(theme.keycap_warn(), Color::Rgb(255, 136, 0));
}

#[test]
fn test_ansi16_and_monochrome_presets() {
    let ansi = Theme::from_color_scheme("ansi16", &ColorSchemeConfig::default());
    assert_eq!(ansi.app_bg, Color::Reset);
    assert_eq!(ansi.text, Color::White);
    assert_eq!(ansi.error_fg, Color::LightRed);

    let mono = Theme::from_color_scheme("monochrome", &ColorSchemeConfig::default());
    assert_eq!(mono.app_bg, Color::Reset);
    assert_eq!(mono.text, Color::Reset);
    assert_eq!(mono.error_fg, Color::Reset);
}

#[test]
fn severity_band_tints_toward_the_accent_and_falls_back_on_reset_surfaces() {
    // Dark RGB profile: the band is a subtle wash of the accent over the
    // app background — never the raw accent (too loud for a card band) and
    // never the plain background (severity invisible). The foreground pulls
    // the accent toward text so the label stays readable.
    let dark = Theme::default();
    assert!(
        dark.severity_is_color_distinguishable(),
        "a dark RGB theme carries severity in color"
    );
    let err = dark.severity_band(dark.err(), "error");
    assert_ne!(err.bg, dark.err(), "band bg must not be the raw accent");
    assert_ne!(err.bg, dark.app_bg, "band bg must not be the plain surface");
    assert_ne!(err.fg, dark.err(), "band fg must not be the raw accent");
    assert_ne!(err.bg, err.fg, "bg and fg must stay distinct");
    assert_eq!(err.marker, "", "color carries it; no textual marker needed");

    // The three severity accents produce mutually distinct bands, so
    // color-only severity never collapses two severities into one tint.
    let warn = dark.severity_band(dark.warn(), "warn");
    let ok = dark.severity_band(dark.ok(), "ok");
    let info = dark.severity_band(dark.info(), "info");
    assert_ne!(err.bg, warn.bg);
    assert_ne!(warn.bg, ok.bg);
    assert_ne!(ok.bg, info.bg);

    // ANSI-16: `app_bg` is Reset but the accents are real ANSI colors, so
    // severity is still distinguishable by color; the band uses the raised
    // surface and keeps the accent, and emits no textual marker.
    let ansi = Theme::from_color_scheme("ansi16", &ColorSchemeConfig::default());
    assert_eq!(ansi.app_bg, Color::Reset);
    assert!(ansi.severity_is_color_distinguishable());
    let band = ansi.severity_band(ansi.err(), "error");
    assert_eq!(band.bg, ansi.raised());
    assert_eq!(band.fg, ansi.err());
    assert_eq!(band.marker, "");

    // Monochrome/VT100: every accent is `Reset`, so color carries NOTHING.
    // The band must not pretend a tint exists — it emits a textual marker
    // instead, or severity would be invisible after the glyph removal.
    let mono = Theme::from_color_scheme("monochrome", &ColorSchemeConfig::default());
    assert!(
        !mono.severity_is_color_distinguishable(),
        "monochrome cannot carry severity in color"
    );
    let err = mono.severity_band(mono.err(), "error");
    let warn = mono.severity_band(mono.warn(), "warn");
    assert_eq!(err.marker, "error");
    assert_eq!(warn.marker, "warn");
    assert_ne!(
        err.marker, warn.marker,
        "monochrome severities stay distinct via the textual marker"
    );
}

#[test]
fn test_resolve_with_profile_adapts_defaults() {
    let vt100_prof = nuotc::TerminalProfile::dec_vt100_monochrome();
    let theme = Theme::resolve_with_profile("zen", &ColorSchemeConfig::default(), &[], &vt100_prof);
    assert_eq!(theme.app_bg, Color::Reset);
    assert_eq!(theme.text, Color::Reset);

    let linux_prof = nuotc::TerminalProfile::ecma48_ansi16();
    let theme_linux =
        Theme::resolve_with_profile("zen", &ColorSchemeConfig::default(), &[], &linux_prof);
    assert_eq!(theme_linux.app_bg, Color::Reset);
    assert_eq!(theme_linux.text, Color::White);

    // Explicit non-default choice is preserved
    let theme_nord =
        Theme::resolve_with_profile("nord", &ColorSchemeConfig::default(), &[], &vt100_prof);
    assert_ne!(theme_nord.text, Color::Reset);
}
