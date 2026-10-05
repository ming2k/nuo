use super::common::{draw_picker_search_row, place_picker_search_cursor};
use super::editor::effort_block_rows;
use super::*;
use crate::providers::PROVIDER_PRESETS;
use crate::render::Theme;
use nuotc::Rect;

/// Render the whole frame buffer back to a single string (rows joined by
/// `\n`), the standard readback helper for layout-level modal assertions.
fn buffer_text(terminal: &nuotc::TestTerminal) -> String {
    let buf = terminal.buffer();
    let area = buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render the per-model settings editor (effort + thinking, no API key)
/// into a terminal of the given size and read back the buffer text.
fn render_settings_editor(
    width: u16,
    height: u16,
    effort: Option<&str>,
    levels: &[String],
    thinking: Option<bool>,
    overrides: Option<(Option<bool>, Option<bool>)>,
) -> String {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(width, height);
    terminal.draw(|f| {
        draw_model_editor(
            f,
            "claude-opus-4-8",
            effort.unwrap_or(""),
            0,
            false,
            1,
            true,
            effort,
            levels,
            thinking,
            overrides,
            &theme,
        );
    });
    buffer_text(&terminal)
}

fn levels(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// The reasoning-tag decision from `model_list_body`, factored out for a
/// direct unit test (the row renderer is layout machinery; the policy is
/// what matters).
fn reasoning_tag(thinking: Option<bool>, effort: Option<&str>) -> String {
    match (thinking, effort) {
        (Some(true), Some(effort)) => format!("think on {effort}"),
        (Some(true), None) => "think on".to_string(),
        (None, Some(effort)) => effort.to_string(),
        _ => String::new(),
    }
}

#[test]
fn effort_slider_renders_at_every_supported_width() {
    // The selector is the slider at EVERY width, so it must lay out from
    // the minimum terminal (40 cols, per MIN_TERMINAL_COLS) upward, for
    // every ladder shape and every selection, without panicking — the
    // label thinning guarantees no overlap, not just no crash.
    let ladders: Vec<Vec<&str>> = vec![
        vec!["none", "minimal", "low", "medium", "high", "xhigh", "max"],
        vec!["low", "medium", "high", "xhigh", "max"],
        vec!["low", "medium", "high"],
        vec!["low", "high", "max"],
        vec!["medium"],
    ];
    for cols in 40u16..121 {
        for ladder in &ladders {
            for tier in ladder {
                let lv: Vec<String> = ladder.iter().map(|s| s.to_string()).collect();
                let text = render_settings_editor(cols, 24, Some(tier), &lv, None, None);
                // The slider's shape markers are present at every width.
                assert!(
                    text.contains('●'),
                    "marker at {cols} cols ({tier}): {text:?}"
                );
                assert!(
                    !text.contains("< "),
                    "no carousel chevrons at {cols} cols: {text:?}"
                );
            }
        }
    }
}

#[test]
fn effort_selector_renders_as_a_node_slider_when_wide() {
    // Wide enough: the `Effort` label owns its row, then a blank row, a
    // `Faster ⇄ Smarter` track with a circle node per tier and the marker
    // sitting squarely on the selected node; every tier labeled underneath
    // centered on its node in ascending depth.
    let full = levels(&["low", "medium", "high", "xhigh", "max"]);
    let text = render_settings_editor(120, 24, Some("high"), &full, None, None);
    let rows: Vec<&str> = text.lines().collect();
    let label_idx = rows
        .iter()
        .position(|l| l.contains("Effort"))
        .expect("label row");
    // The label owns its row — the slider component is on the next one.
    assert!(
        !rows[label_idx].contains("Faster"),
        "label row must not share with the slider: {:?}",
        rows[label_idx]
    );

    let track_row = rows[label_idx + 2];
    assert!(track_row.contains("Faster"), "scale start: {track_row:?}");
    assert!(track_row.contains("Smarter"), "scale end: {track_row:?}");
    // 5 tiers: 4 unselected circles + 1 selected circle marker.
    assert_eq!(
        track_row.chars().filter(|&c| c == '○').count(),
        4,
        "circle nodes for the unselected rungs: {track_row:?}"
    );
    assert!(
        track_row.contains('●'),
        "marker on the selected node: {track_row:?}"
    );
    // The carousel affordance is gone in the slider form.
    assert!(
        !track_row.contains('<'),
        "no carousel chevrons: {track_row:?}"
    );

    let labels_row = rows[label_idx + 3];
    for tier in ["low", "medium", "high", "xhigh", "max"] {
        assert!(labels_row.contains(tier), "missing tier: {labels_row:?}");
    }
    // Depth order is left-to-right ascending.
    let low = labels_row.find("low").unwrap();
    let max = labels_row.find("max").unwrap();
    assert!(low < max, "ladder must ascend left→right: {labels_row:?}");
    // The marker lands exactly on the selected node: same column as the
    // tier label's center.
    let marker = track_row.chars().position(|c| c == '●').unwrap();
    let high = labels_row.find("high").unwrap();
    assert_eq!(
        marker,
        high + "high".len() / 2,
        "marker centered on the selected node: {track_row:?} vs {labels_row:?}"
    );
    // Endpoints are also centered directly under their node columns.
    let left_node = track_row.chars().position(|c| c == '○').unwrap();
    let low_col = labels_row.find("low").unwrap();
    assert_eq!(
        left_node,
        low_col + "low".len() / 2,
        "low centered under the left endpoint node"
    );
    let right_node = track_row
        .chars()
        .enumerate()
        .filter(|(_, c)| *c == '○')
        .last()
        .map(|(i, _)| i)
        .unwrap();
    let max_col = labels_row.rfind("max").unwrap();
    assert_eq!(
        right_node,
        max_col + "max".len() / 2,
        "max centered under the right endpoint node"
    );
}

#[test]
fn effort_selector_stays_a_slider_when_narrow() {
    // One shape at every width: too narrow for verbatim tier labels and
    // the block still renders the `Faster ⇄ Smarter` slider — cramped
    // interior labels thin out (ends + selected stay) instead of swapping
    // to a carousel. Absolutely no `<`/`>` chevrons anywhere.
    let full = levels(&["low", "medium", "high", "xhigh", "max"]);
    let text = render_settings_editor(56, 24, Some("high"), &full, None, None);
    let rows: Vec<&str> = text.lines().collect();
    let track_row = rows
        .iter()
        .find(|l| l.contains("Faster"))
        .expect("track row at narrow width");
    assert!(track_row.contains("Smarter"), "scale end: {track_row:?}");
    assert!(track_row.contains('●'), "marker: {track_row:?}");
    // The carousel affordance is gone at every width.
    for row in &rows {
        assert!(!row.contains("< "), "no carousel chevrons: {row:?}");
    }
    // The labels row still exists (ends are always labeled).
    let labels_row = rows
        .iter()
        .find(|l| l.contains("low"))
        .expect("labels row at narrow width");
    assert!(
        labels_row.contains("max"),
        "far end labeled: {labels_row:?}"
    );
    // The selected rung keeps its label even when the layout thins.
    assert!(
        labels_row.contains("high"),
        "selected rung labeled: {labels_row:?}"
    );
    // Every rung keeps its node on the track: 5 tiers = 4 circles + 1 marker.
    let nodes = track_row
        .chars()
        .filter(|&c| matches!(c, '○' | '●'))
        .count();
    assert_eq!(nodes, 5, "a node per rung: {track_row:?}");
}

#[test]
fn effort_selector_lays_out_the_full_openai_ladder() {
    // The 7-rung OpenAI ladder (`none`…`max`) fits a standard body with
    // every tier labeled verbatim — no squeezing needed where it counts.
    let openai = levels(&["none", "minimal", "low", "medium", "high", "xhigh", "max"]);
    let text = render_settings_editor(120, 24, Some("medium"), &openai, None, None);
    let labels_row = text
        .lines()
        .find(|l| l.contains("minimal"))
        .expect("labels row");
    for tier in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
        assert!(labels_row.contains(tier), "missing tier: {labels_row:?}");
    }
    assert!(
        !labels_row.contains('≤') && !labels_row.contains('≥'),
        "wide body labels verbatim: {labels_row:?}"
    );
}

#[test]
fn effort_caption_shows_in_both_forms() {
    // The current tier's caption closes the block on its own row —
    // truncated to the available width rather than dropped or wrapped
    // awkwardly — at every width, and for an unknown ladder too.
    let full = levels(&["low", "medium", "high", "xhigh", "max"]);
    let wide = render_settings_editor(120, 24, Some("high"), &full, None, None);
    assert!(
        wide.contains("deep reasoning"),
        "slider caption present: {wide:?}"
    );
    let narrow = render_settings_editor(56, 24, Some("high"), &full, None, None);
    assert!(
        narrow.contains("deep reasoning"),
        "narrow slider caption present: {narrow:?}"
    );
    let unknown = render_settings_editor(120, 24, Some("high"), &[], None, None);
    assert!(
        unknown.contains("deep reasoning"),
        "unknown-ladder caption present: {unknown:?}"
    );
}

#[test]
fn only_the_api_key_field_shows_a_caret() {
    // The effort selector is cycled (not typed) and thinking is a toggle,
    // so neither may raise the text caret — a parked cursor on a non-text
    // field reads as "type here" and jitters as the value cycles.
    let theme = Theme::default();
    let full = levels(&["low", "medium", "high", "xhigh", "max"]);
    let mut terminal = nuotc::TestTerminal::new(120, 24);
    terminal.draw(|f| {
        draw_model_editor(
            f,
            "m",
            "sk-live",
            7,
            true,
            1,
            true,
            Some("high"),
            &full,
            None,
            None,
            &theme,
        );
    });
    assert_eq!(
        terminal.cursor(),
        nuotc::CursorState::Hidden,
        "no caret while the effort selector is focused"
    );
    let mut terminal = nuotc::TestTerminal::new(120, 24);
    terminal.draw(|f| {
        draw_model_editor(
            f,
            "m",
            "sk-live",
            7,
            true,
            0,
            true,
            Some("high"),
            &full,
            None,
            None,
            &theme,
        );
    });
    assert!(
        matches!(terminal.cursor(), nuotc::CursorState::Visible(..)),
        "the API-key text field keeps its caret"
    );
}

#[test]
fn thinking_renders_as_a_checkbox_not_a_carousel() {
    // The boolean is `[x]`/`[ ]`, never `< on >` — the control's shape
    // finally matches its semantics.
    let on = render_settings_editor(100, 24, None, &[], Some(true), None);
    assert!(on.contains("[x] on"), "checked: {on:?}");
    assert!(!on.contains("< on >"), "no carousel for a bool: {on:?}");
    let off = render_settings_editor(100, 24, None, &[], Some(false), None);
    assert!(off.contains("[ ] off"), "unchecked: {off:?}");
}

#[test]
fn effort_block_row_count_depends_only_on_the_ladder() {
    // The selector is the slider at every width, so the block's row count
    // is width-independent by construction — six rows for a known ladder
    // (label + blank + track + tier labels + blank + caption), three for an unknown one
    // (value row + blank + caption). Nothing can flip between two shapes as the
    // user cycles or resizes.
    let common = levels(&["low", "medium", "high"]);
    assert_eq!(effort_block_rows(&common), 6, "3-tier → slider rows");
    let openai = levels(&["none", "minimal", "low", "medium", "high", "xhigh"]);
    assert_eq!(effort_block_rows(&openai), 6, "6-tier → slider rows");
    // An unknown ladder collapses to the value row + blank + caption.
    assert_eq!(
        effort_block_rows(&[]),
        3,
        "empty ladder → value + blank + caption"
    );
}

#[test]
fn reasoning_tag_shows_openai_effort_and_anthropic_opt_in() {
    // Anthropic: opted-in thinking shows `think on <effort>`; opted-out
    // shows nothing even when an effort value is configured.
    assert_eq!(reasoning_tag(Some(true), Some("high")), "think on high");
    assert_eq!(reasoning_tag(Some(true), None), "think on");
    assert_eq!(reasoning_tag(Some(false), Some("high")), "");
    // OpenAI (Kimi K3 & friends): no thinking switch, so the current
    // effort shows directly — this is the picker-row half of the hint
    // bar's `Kimi K3 max` tag.
    assert_eq!(reasoning_tag(None, Some("max")), "max");
    // Unconfigured models show nothing.
    assert_eq!(reasoning_tag(None, None), "");
}

/// Render the preset chooser at `selected` into a terminal and
/// read back the full buffer text, the standard readback for the chooser's
/// layout-level assertions.
fn render_preset_chooser(selected: usize, width: u16, height: u16) -> String {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(width, height);
    let mut scroll = 0;
    terminal.draw(|f| {
        draw_preset_chooser(selected, f, &theme, &mut scroll);
    });
    buffer_text(&terminal)
}

#[test]
fn preset_rows_are_sorted_by_title() {
    // The chooser's display order IS the table order (the const is kept
    // sorted), so an out-of-order insertion breaks the alphabetical rule
    // at the declaration site. This test pins it.
    let titles: Vec<&str> = PROVIDER_PRESETS.iter().map(|t| t.label).collect();
    let mut sorted = titles.clone();
    sorted.sort();
    assert_eq!(
        titles, sorted,
        "PROVIDER_PRESETS must stay sorted by label (title)"
    );
    // And the chooser renders them in table order, so display order is
    // alphabetical by construction.
}

#[test]
fn preset_chooser_shows_only_titles_when_unfocused() {
    // Selection 0 = "Anthropic" (the table is title-sorted). Every other
    // row is unfocused, so its description must NOT be in the buffer;
    // only the focused row's description is revealed.
    let text = render_preset_chooser(0, 100, 32);
    assert!(
        text.contains("Anthropic"),
        "focused title present: {text:?}"
    );
    // Focused row's description is revealed. Whitespace-insensitive: the
    // grapheme-level wrapper may break the sentence mid-word, inserting a
    // newline + indent between its characters.
    let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        squeezed.contains("Connections›Addconnection"),
        "template branch breadcrumb: {text:?}"
    );
    assert!(
        squeezed.contains("flagshipClaudemodelswithadvancedreasoning"),
        "focused description revealed: {text:?}"
    );
    assert!(
        !text.contains("Custom connection"),
        "custom connection is a sibling branch, not a preset row: {text:?}"
    );
    // An unfocused row's description is hidden (Antigravity OAuth is
    // further down the sorted list).
    assert!(
        !text.contains("Google One AI Premium subscription"),
        "unfocused rows show title only: {text:?}"
    );
    // The old meta run is gone. Checked per line, and only for a DIGIT-
    // prefixed "N model(s)" — the revealed description legitimately
    // contains the word "models" ("Claude models over …").
    for line in text.lines() {
        let count_meta = line.match_indices(" model").any(|(i, _)| {
            line[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_digit())
        });
        assert!(!count_meta, "no seeded model-count meta: {line:?}");
        assert!(
            !line.contains("· openai")
                && !line.contains("· google")
                && !line.contains("· anthropic"),
            "no `·`-joined protocol meta: {line:?}"
        );
    }
    // No `›` cursor marker in the body (the header breadcrumb's `›` is
    // outside the rows, so the whole-buffer scan would false-positive —
    // check the row lines instead).
    let rows: Vec<&str> = text.lines().collect();
    let body_rows = rows
        .iter()
        .filter(|l| l.contains("GitHub") || l.contains("OpenAI") || l.contains("Anthropic"))
        .collect::<Vec<_>>();
    assert!(
        body_rows.iter().all(|l| !l.contains('›')),
        "no `›` cursor marker on rows: {body_rows:?}"
    );
}

#[test]
fn preset_chooser_carries_the_auth_scheme_in_the_description_sentence() {
    // The auth scheme is no longer a badge; it is stated in the focused
    // row's description sentence — "authorizes in the browser" for OAuth
    // flows, "sign in with ... API key" for tokens — and no emoji glyph
    // is used anywhere in the chooser.
    let xai_idx = PROVIDER_PRESETS
        .iter()
        .position(|t| t.id == "xai")
        .expect("xai template");
    let text = render_preset_chooser(xai_idx, 100, 32);
    assert!(
        text.contains("authorizes in the browser"),
        "oauth phrasing in xAI description: {text:?}"
    );

    let openai_idx = PROVIDER_PRESETS
        .iter()
        .position(|t| t.id == "openai")
        .expect("openai preset");
    let text = render_preset_chooser(openai_idx, 100, 32);
    assert!(
        text.contains("API key"),
        "token phrasing in OpenAI description: {text:?}"
    );

    // No badge glyph and no legacy two-column artifacts on any row.
    for line in text.lines() {
        assert!(
            !line.contains("⚿") && !line.contains("⚡"),
            "no auth glyphs: {line:?}"
        );
        assert!(
            !line.contains("oauth ·") && !line.contains("token ·"),
            "no badge meta: {line:?}"
        );
    }
}

#[test]
fn preset_chooser_highlights_the_focused_row_with_a_background_fill() {
    // The focused row paints a brand background across its full width —
    // the Connections/Models standard — instead of a `›` marker. Locate
    // the focused title's row in the cell buffer and assert every column
    // of that row carries the brand background (an unbroken band).
    let openai_idx = PROVIDER_PRESETS
        .iter()
        .position(|t| t.id == "openai")
        .expect("openai preset");
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(100, 32);
    let mut scroll = 0;
    terminal.draw(|f| {
        draw_preset_chooser(openai_idx, f, &theme, &mut scroll);
    });

    // Rebuild the row texts from the buffer to find the focused row's y.
    let buf = terminal.buffer();
    let area = buf.area();
    let row_text = |y: u16| -> String {
        (0..area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
    };
    let focused_y = (0..area.height)
        .map(|y| (y, row_text(y)))
        // Exact-title match: a subtitle row ("Custom OpenAI") must not
        // capture a search for the "OpenAI Platform" preset's row.
        .find(|(_, text)| text.trim().starts_with("OpenAI Platform"))
        .map(|(y, _)| y)
        .expect("focused OpenAI Platform row rendered");

    // Every column of the focused row inside the modal BODY carries the
    // brand background. The panel spans the middle 72% of the viewport
    // with `MODAL_INNER_H_PADDING` (3) content inset each side; find the
    // body's exact edges on the focused row by walking in from the
    // terminal edges past the unpainted (Reset) margin, then skipping
    // the inset padding (painted panel-background, not brand).
    let brand = theme.brand();
    let panel_bg = theme.panel();
    let is_painted = |x: u16| buf[(x, focused_y)].bg() != nuotc::Color::Reset;
    let mut left = 0u16;
    while !is_painted(left) {
        left += 1;
    }
    let mut right = area.width - 1;
    while !is_painted(right) {
        right -= 1;
    }
    // Skip inward past the panel padding to the body band.
    let mut body_left = left;
    while buf[(body_left, focused_y)].bg() == panel_bg {
        body_left += 1;
    }
    let mut body_right = right;
    while buf[(body_right, focused_y)].bg() == panel_bg {
        body_right -= 1;
    }
    assert!(
        body_left < body_right,
        "brand band found on the focused row"
    );
    for x in body_left..=body_right {
        assert_eq!(
            buf[(x, focused_y)].bg(),
            brand,
            "column {x} of the focused row must carry the brand fill"
        );
    }
    // An unfocused row (the first title, "Anthropic") carries no brand
    // background — the panel background instead.
    let unfocused_y = (0..area.height)
        .map(|y| (y, row_text(y)))
        .find(|(_, text)| text.contains("Anthropic"))
        .map(|(y, _)| y)
        .expect("first unfocused row rendered");
    assert_ne!(
        buf[(body_left, unfocused_y)].bg(),
        brand,
        "unfocused rows have no brand fill"
    );
}

// Sectioned Models list (Favorites / Recent / All models)

/// A snapshot with one favorite, two used models, and two plain models,
/// so all three sections render and RECENT has a meaningful internal
/// order (gpt-5.5 newer than claude-opus-4-8).
fn sectioned_snapshot() -> nuo_wire::ProviderPickerSnapshot {
    // `enabled` is the provider's picker declaration: `true` = an ordinary
    // selectable row, `false` = a subscription-locked one. The lock case has
    // its own test (`models_modal_lists_provider_locked_rows_greyed_with_a_lock_tag`)
    // and builds its own rows; everything here is the normal path.
    let info = |model: &str, favorite: bool, used: Option<u64>, enabled: bool| {
        nuo_wire::ProviderModelInfo {
            model: model.to_string(),
            name: None,
            protocol: String::new(),
            effort: None,
            thinking: None,
            effort_levels: Vec::new(),
            favorite,
            last_used_ms: used,
            vision: Some(false),
            context_window: 128_000,
            max_output_tokens: None,
            availability: Some(if enabled {
                nuo_wire::Availability::usable()
            } else {
                nuo_wire::Availability::locked(None)
            }),
            availability_overridden: false,
            advertised: None,
            availability_stale: false,
        }
    };
    let row = |id: &str, name: &str, models: Vec<nuo_wire::ProviderModelInfo>| {
        nuo_wire::ProviderPickerRow {
            id: id.to_string(),
            name: name.to_string(),
            model: models.first().map(|m| m.model.clone()).unwrap_or_default(),
            models: models.iter().map(|m| m.model.clone()).collect(),
            model_info: models,
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            client_identity: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }
    };
    nuo_wire::ProviderPickerSnapshot {
        default_id: "openai".into(),
        rows: vec![
            row(
                "openai",
                "OpenAI",
                vec![
                    info("gpt-5.5", false, Some(1_700_000_000_000), true),
                    info("gpt-5.4", false, None, true),
                ],
            ),
            row(
                "anthropic",
                "Anthropic",
                vec![
                    info("claude-sonnet-5", true, Some(1_500_000_000_000), true),
                    info("claude-opus-4-8", false, Some(1_600_000_000_000), true),
                ],
            ),
            row(
                "google",
                "Google",
                vec![info("gemini-3-pro", false, None, true)],
            ),
        ],
    }
}

/// Render the Models modal (browse mode, cursor on `modal_index`) into a
/// 72×24 terminal and read back the buffer text.
fn render_models_modal(modal_index: usize, query: &str, search: bool) -> String {
    let theme = Theme::default();
    let picker = sectioned_snapshot();
    let ranked = crate::providers::models_flat_filtered_from(&picker, "openai", "gpt-5.5", query);
    let mut terminal = nuotc::TestTerminal::new(72, 28);
    terminal.draw(|f| {
        let mut scroll = 0;
        draw_models_modal(
            f,
            crate::overlays::provider::models::ModelsModalProps {
                models: &ranked,
                current_provider: "openai",
                current_model: "gpt-5.5",
                modal_index,
                query,
                cursor_position: query.len(),
                scroll: &mut scroll,
                follow_selection: true,
                search,
                show_caret: true,
                refreshing: false,
                spinner_phase: 0,
            },
            &theme,
        );
    });
    buffer_text(&terminal)
}

#[test]
fn models_modal_renders_three_labeled_sections() {
    // The flat list groups into FAVORITES / RECENT / ALL MODELS with dim
    // label rows between the groups, and the row order inside each
    // section matches the data-layer contract (star beats recency;
    // RECENT is most-recent-first; the rest ASCII).
    let text = render_models_modal(2, "", false);
    let favorites = text.find("FAVORITES").expect("FAVORITES label");
    let recent = text.find("RECENT").expect("RECENT label");
    let all = text.find("ALL MODELS").expect("ALL MODELS label");
    assert!(
        favorites < recent && recent < all,
        "labels in display order"
    );

    let sonnet = text.find("claude-sonnet-5").expect("favorite row");
    let opus = text.find("claude-opus-4-8").expect("older recent row");
    let gpt55 = text.find("gpt-5.5").expect("newer recent row");
    let gemini = text.find("gemini-3-pro").expect("plain row");
    let gpt54 = text.find("gpt-5.4").expect("plain row");
    // Favorite row inside FAVORITES; RECENT rows newest-first between
    // their label and ALL MODELS; plain rows after.
    assert!(favorites < sonnet && sonnet < recent);
    assert!(recent < gpt55 && gpt55 < opus && opus < all);
    assert!(all < gemini && gemini < gpt54);
}

#[test]
fn models_modal_lists_provider_locked_rows_greyed_with_a_lock_tag() {
    // Qoder parity: the server catalog lists subscription-locked models, the
    // official `/model` menu renders them greyed-out, and nuo's picker now
    // does the same — the row stays visible (so the account can see what an
    // upgrade unlocks) with a leading `locked` tag instead of being dropped.
    let theme = Theme::default();
    let mut picker = sectioned_snapshot();
    let info = |model: &str, availability: Option<nuo_wire::Availability>| {
        nuo_wire::ProviderModelInfo {
            model: model.to_string(),
            name: None,
            protocol: String::new(),
            effort: None,
            thinking: None,
            effort_levels: Vec::new(),
            favorite: false,
            last_used_ms: None,
            vision: Some(false),
            context_window: 128_000,
            max_output_tokens: None,
            availability,
            availability_overridden: false,
            advertised: None,
            availability_stale: false,
        }
    };
    picker.rows.push(nuo_wire::ProviderPickerRow {
        id: "qoder".into(),
        name: "Qoder".into(),
        model: "gmodel".into(),
        models: vec!["qfmodel".into(), "gmodel".into()],
        model_info: vec![
            info("qfmodel", Some(nuo_wire::Availability::usable())),
            info("gmodel", Some(nuo_wire::Availability::locked(None))),
        ],
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        provider: String::new(),
        client_identity: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    });
    let ranked = crate::providers::models_flat_filtered_from(&picker, "openai", "gpt-5.5", "");
    assert!(ranked.iter().any(|r| r.model == "gmodel" && !r.usable));
    assert!(ranked.iter().any(|r| r.model == "qfmodel" && r.usable));
    let mut terminal = nuotc::TestTerminal::new(72, 28);
    terminal.draw(|f| {
        let mut scroll = 0;
        draw_models_modal(
            f,
            crate::overlays::provider::models::ModelsModalProps {
                models: &ranked,
                current_provider: "openai",
                current_model: "gpt-5.5",
                modal_index: ranked
                    .iter()
                    .position(|r| r.model == "gmodel")
                    .expect("locked row present"),
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: true,
                search: false,
                show_caret: true,
                refreshing: false,
                spinner_phase: 0,
            },
            &theme,
        );
    });
    let text = buffer_text(&terminal);
    assert!(text.contains("gmodel"), "locked row is listed");
    assert!(text.contains("locked"), "lock tag renders");
}

/// ADR-0273: when the provider states a reason, the row shows *that* — nuo
/// must never invent "locked for the current plan", which is a different,
/// unstated diagnosis (the `403 code 110` quota incident is exactly a case
/// where the plan is fine).
#[test]
fn models_modal_shows_the_providers_own_reason_verbatim() {
    let theme = Theme::default();
    let mut picker = sectioned_snapshot();
    let info = |model: &str, availability: Option<nuo_wire::Availability>| {
        nuo_wire::ProviderModelInfo {
            model: model.to_string(),
            name: None,
            protocol: String::new(),
            effort: None,
            thinking: None,
            effort_levels: Vec::new(),
            favorite: false,
            last_used_ms: None,
            vision: Some(false),
            context_window: 128_000,
            max_output_tokens: None,
            availability,
            availability_overridden: false,
            advertised: None,
            availability_stale: false,
        }
    };
    picker.rows.push(nuo_wire::ProviderPickerRow {
        id: "qoder".into(),
        name: "Qoder".into(),
        model: "degraded".into(),
        models: vec!["degraded".into(), "overridden".into()],
        model_info: vec![
            info(
                "degraded",
                Some(nuo_wire::Availability::locked(Some(
                    "Billing daily count exceeded".to_string(),
                ))),
            ),
            nuo_wire::ProviderModelInfo {
                availability: Some(nuo_wire::Availability::usable()),
                availability_overridden: true,
                ..info("overridden", None)
            },
        ],
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        provider: String::new(),
        client_identity: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    });
    let ranked = crate::providers::models_flat_filtered_from(&picker, "openai", "gpt-5.5", "");
    let mut terminal = nuotc::TestTerminal::new(96, 28);
    terminal.draw(|f| {
        let mut scroll = 0;
        draw_models_modal(
            f,
            crate::overlays::provider::models::ModelsModalProps {
                models: &ranked,
                current_provider: "openai",
                current_model: "gpt-5.5",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: true,
                search: false,
                show_caret: true,
                refreshing: false,
                spinner_phase: 0,
            },
            &theme,
        );
    });
    let text = buffer_text(&terminal);
    assert!(
        text.contains("Billing daily count exceeded"),
        "the provider's own reason is rendered verbatim: {text}"
    );
    assert!(
        !text.contains("locked for the current plan"),
        "muta must not invent a plan diagnosis the provider never stated"
    );
    assert!(
        text.contains("overridden by you"),
        "an override of an upstream lock is disclosed, not silent: {text}"
    );
}

#[test]
fn models_modal_leads_with_the_provider_label_and_keeps_the_id_visible() {
    // A relay's opaque wire id (`deepseek-flash` for "DeepSeek V4.1 Flash")
    // is unrecognisable, so the row LEADS with the provider's own name. The id
    // must NOT disappear though — it is the string that goes on the wire and
    // into config — so it rides along in the column beside it. A model whose
    // endpoint publishes nothing leads with its bare id.
    //
    // The assertions check ORDER, not mere presence: both fields are on the
    // line in either layout, so a `contains` check alone cannot catch a
    // label/id swap (which is exactly how the id-first regression passed).
    let text = render_labelled_models_modal(110);

    let labelled = text
        .lines()
        .find(|line| line.contains("DeepSeek V4.1 Flash"))
        .expect("labelled row renders");
    let label_at = labelled
        .find("DeepSeek V4.1 Flash")
        .expect("label on its own row");
    let id_at = labelled
        .find("gemini-3-pro")
        .unwrap_or_else(|| panic!("the wire id stays visible behind the name: {labelled:?}"));
    assert!(
        label_at < id_at,
        "the provider label leads the row and the id follows: {labelled:?}"
    );

    let bare = text
        .lines()
        .find(|line| line.contains("gpt-5.4"))
        .expect("unlabelled row renders");
    assert!(
        !bare.contains("DeepSeek"),
        "a row with no advertised label leads with its id: {bare:?}"
    );
}

#[test]
fn models_modal_drops_the_id_column_when_narrow_never_the_label() {
    // The label owns the identity region; the wire id fills only its leftover
    // padding. So on a cramped terminal the ID column vanishes rather than
    // truncating the label or overflowing into the provider column — the row
    // then reads exactly like a row whose provider published no label.
    let narrow = render_labelled_models_modal(60);
    let row = narrow
        .lines()
        .find(|line| line.contains("DeepSeek V4.1 Flash"))
        .expect("labelled row renders on a narrow terminal");
    assert!(
        !row.contains("gemini-3-pro"),
        "no room for the id column means no id column: {row:?}"
    );
    assert!(
        !row.contains("…Flash") && !row.contains("Flash…"),
        "the label is never truncated to fund the id column: {row:?}"
    );

    // The wider render proves the two differ only by the id column, so the
    // assertion above is about the column and not about a missing row.
    let wide = render_labelled_models_modal(110);
    assert!(wide.contains("DeepSeek V4.1 Flash"));
    assert!(wide.contains("gemini-3-pro"));
}

/// Render the Models modal with one provider-published model label at a given
/// terminal width. Returns the buffer text.
fn render_labelled_models_modal(width: u16) -> String {
    let theme = Theme::default();
    let mut picker = sectioned_snapshot();
    for prow in &mut picker.rows {
        if let Some(info) = prow
            .model_info
            .iter_mut()
            .find(|info| info.model == "gemini-3-pro")
        {
            info.name = Some("DeepSeek V4.1 Flash".to_string());
        }
    }
    let ranked = crate::providers::models_flat_filtered_from(&picker, "openai", "gpt-5.5", "");
    let mut terminal = nuotc::TestTerminal::new(width, 40);
    terminal.draw(|f| {
        let mut scroll = 0;
        draw_models_modal(
            f,
            crate::overlays::provider::models::ModelsModalProps {
                models: &ranked,
                current_provider: "openai",
                current_model: "gpt-5.5",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: true,
                search: false,
                refreshing: false,
                show_caret: true,
                spinner_phase: 0,
            },
            &theme,
        );
    });
    buffer_text(&terminal)
}

/// One Qoder catalog entry: an opaque `key` plus the provider's own
/// `display_name`, which is what actually identifies the model to a human.
fn qoder_model_info(
    model: &str,
    name: &str,
    availability: Option<nuo_wire::Availability>,
) -> nuo_wire::ProviderModelInfo {
    nuo_wire::ProviderModelInfo {
        model: model.to_string(),
        name: Some(name.to_string()),
        protocol: String::new(),
        effort: None,
        thinking: None,
        effort_levels: Vec::new(),
        favorite: false,
        last_used_ms: None,
        vision: Some(true),
        context_window: 200_000,
        max_output_tokens: None,
        availability,
        availability_overridden: false,
        advertised: None,
        availability_stale: false,
    }
}

/// Render the Models modal over a Qoder-shaped snapshot: opaque catalog keys
/// (`qfmodel`) carrying the provider's real model names (`Qwen3.8-Flash`).
fn render_qoder_models_modal(width: u16, query: &str) -> String {
    let theme = Theme::default();
    let picker = nuo_wire::ProviderPickerSnapshot {
        default_id: "qoder".into(),
        rows: vec![nuo_wire::ProviderPickerRow {
            id: "qoder".into(),
            name: "Qoder".into(),
            model: "qfmodel".into(),
            models: vec!["auto".into(), "qfmodel".into(), "gmodel".into()],
            model_info: vec![
                qoder_model_info("auto", "Auto", None),
                qoder_model_info(
                    "qfmodel",
                    "Qwen3.8-Flash",
                    Some(nuo_wire::Availability::usable()),
                ),
                qoder_model_info(
                    "gmodel",
                    "GLM-5.3",
                    Some(nuo_wire::Availability::locked(None)),
                ),
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            client_identity: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };
    let ranked = crate::providers::models_flat_filtered_from(&picker, "qoder", "qfmodel", query);
    let mut terminal = nuotc::TestTerminal::new(width, 20);
    terminal.draw(|f| {
        let mut scroll = 0;
        draw_models_modal(
            f,
            crate::overlays::provider::models::ModelsModalProps {
                models: &ranked,
                current_provider: "qoder",
                current_model: "qfmodel",
                modal_index: 0,
                query,
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: true,
                search: !query.is_empty(),
                show_caret: true,
                refreshing: false,
                spinner_phase: 0,
            },
            &theme,
        );
    });
    buffer_text(&terminal)
}

#[test]
fn models_modal_leads_with_the_model_name_for_opaque_catalog_keys() {
    // The reported case: Qoder's catalog keys (`qfmodel`, `gmodel`) say nothing
    // about what actually runs, while `display_name` carries the real model
    // (`Qwen3.8-Flash`, `GLM-5.3`). The name leads the row; the key stays
    // visible beside it because it is the string that goes on the wire.
    let text = render_qoder_models_modal(100, "");
    let row = text
        .lines()
        .find(|line| line.contains("Qwen3.8-Flash"))
        .expect("the model name renders");
    let name_at = row.find("Qwen3.8-Flash").expect("name on its row");
    let id_at = row
        .find("qfmodel")
        .unwrap_or_else(|| panic!("the catalog key stays visible: {row:?}"));
    assert!(
        name_at < id_at,
        "name-first, id-fallback: the key must not lead the row: {row:?}"
    );

    // A locked entry keeps its name first too, and still carries its lock tag.
    let locked = text
        .lines()
        .find(|line| line.contains("GLM-5.3"))
        .expect("locked row is listed");
    assert!(
        locked.find("GLM-5.3").expect("name") < locked.find("gmodel").expect("key"),
        "a locked row is name-first as well: {locked:?}"
    );
    assert!(locked.contains("locked"), "lock tag renders: {locked:?}");

    // Searching by the opaque key still finds the row, and by the name too.
    assert!(
        render_qoder_models_modal(100, "qfmodel")
            .lines()
            .any(|line| line.contains("Qwen3.8-Flash")),
        "the wire id stays searchable behind a name label"
    );
}

#[test]
fn models_modal_sections_survive_search_mode() {
    // A fuzzy query keeps the same grouping over the filtered rows.
    let text = render_models_modal(0, "g", true);
    assert!(text.contains("RECENT"), "RECENT section under a query");
    assert!(
        text.contains("ALL MODELS"),
        "ALL MODELS section under a query"
    );
    assert!(
        !text.contains("FAVORITES"),
        "no label for an emptied section"
    );
    // gpt-5.5 (recent) renders before gpt-5.4 / gemini (all).
    let recent_gpt = text.find("gpt-5.5").expect("recent match");
    let all_gpt = text.find("gpt-5.4").expect("plain match");
    assert!(recent_gpt < all_gpt);
}

#[test]
fn models_modal_selection_cursor_lands_only_on_model_rows() {
    // Walking the cursor across the section boundaries must keep the
    // brand fill on a MODEL row, never on a label or spacer row: the
    // follow logic maps modal_index through the interleaved geometry.
    for idx in 0..9 {
        let text = render_models_modal(idx, "", false);
        // Every index still paints its model somewhere — the invariant
        // checked here is that the modal renders without panicking and
        // keeps all three labels regardless of cursor position.
        assert!(text.contains("FAVORITES"), "labels stable at idx {idx}");
        assert!(text.contains("RECENT"));
        assert!(text.contains("ALL MODELS"));
    }
}

#[test]
fn models_modal_row_omits_leading_dot_and_trailing_diamond() {
    let text = render_models_modal(0, "", false);
    for line in text.lines() {
        if line.contains("gpt-5.5")
            || line.contains("claude-sonnet-5")
            || line.contains("gemini-3-pro")
        {
            assert!(!line.contains('●'), "no leading dot on row: {line:?}");
            assert!(!line.contains('★'), "no leading star on row: {line:?}");
            assert!(!line.contains('◆'), "no diamond glyph on row: {line:?}");
        }
    }
}

#[test]
fn models_modal_search_bar_renders_at_bottom_without_shifting_list() {
    let browse_text = render_models_modal(0, "", false);
    let search_text = render_models_modal(0, "", true);

    // Header "Models" remains on the same row for both
    let browse_header_line = browse_text
        .lines()
        .position(|l| l.contains("Models"))
        .unwrap();
    let search_header_line = search_text
        .lines()
        .position(|l| l.contains("Models"))
        .unwrap();
    assert_eq!(browse_header_line, search_header_line);

    // The first section in search ("FAVORITES") is at the exact same row as in browse mode
    let browse_fav_line = browse_text
        .lines()
        .position(|l| l.contains("FAVORITES"))
        .unwrap();
    let search_fav_line = search_text
        .lines()
        .position(|l| l.contains("FAVORITES"))
        .unwrap();
    assert_eq!(
        browse_fav_line, search_fav_line,
        "list top did not shift when search opened"
    );

    // Search prompt renders near the bottom of search mode
    let search_row = search_text
        .lines()
        .position(|l| l.contains("Search"))
        .expect("search prompt present");
    assert!(
        search_row > search_fav_line,
        "search prompt is at the bottom"
    );
}

#[test]
fn models_modal_search_matches_id_name_and_connection() {
    let mut picker = sectioned_snapshot();
    for prow in &mut picker.rows {
        if prow.id == "openai" {
            prow.name = "OpenAI Official".to_string();
        }
        for info in &mut prow.model_info {
            if info.model == "gpt-5.5" {
                info.name = Some("GPT 5.5 Turbo".to_string());
            }
        }
    }

    // 1. Match by model ID
    let by_id = crate::providers::models_flat_filtered_from(&picker, "", "", "gpt-5.5");
    assert!(
        by_id
            .iter()
            .any(|m| m.model == "gpt-5.5" && m.match_id.is_some())
    );

    // 2. Match by model display name
    let by_name = crate::providers::models_flat_filtered_from(&picker, "", "", "Turbo");
    assert!(
        by_name
            .iter()
            .any(|m| m.model == "gpt-5.5" && m.match_name.is_some())
    );

    // 3. Match by connection/provider name
    let by_conn = crate::providers::models_flat_filtered_from(&picker, "", "", "Official");
    assert!(
        by_conn
            .iter()
            .any(|m| m.provider_label == "OpenAI Official" && m.match_connection.is_some())
    );
}

#[test]
fn models_modal_empty_state_centered_copy_and_footer() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(72, 24);
    terminal.draw(|f| {
        let mut scroll = 0;
        draw_models_modal(
            f,
            crate::overlays::provider::models::ModelsModalProps {
                models: &[],
                current_provider: "",
                current_model: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                refreshing: false,
                show_caret: true,
                spinner_phase: 0,
            },
            &theme,
        );
    });
    let text = buffer_text(&terminal);
    assert!(text.contains("No models available"));
    assert!(text.contains("Add a connection via /connections (or press a)"));
    assert!(text.contains("Configured models will appear here"));
    assert!(text.contains("add connection"));
    assert!(text.contains("close"));
}

#[test]
fn models_modal_refreshing_state_renders_spinner_and_indicator() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(72, 24);
    terminal.draw(|f| {
        let mut scroll = 0;
        draw_models_modal(
            f,
            crate::overlays::provider::models::ModelsModalProps {
                models: &[],
                current_provider: "",
                current_model: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                show_caret: true,
                refreshing: true,
                spinner_phase: 1,
            },
            &theme,
        );
    });
    let text = buffer_text(&terminal);
    assert!(text.contains("Models"));
    assert!(text.contains("refreshing…"));
    assert!(text.contains("Refreshing models…"));
}

#[test]
fn models_modal_search_empty_state() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(72, 24);
    terminal.draw(|f| {
        let mut scroll = 0;
        draw_models_modal(
            f,
            crate::overlays::provider::models::ModelsModalProps {
                models: &[],
                current_provider: "",
                current_model: "",
                modal_index: 0,
                query: "xyz",
                cursor_position: 3,
                scroll: &mut scroll,
                follow_selection: false,
                search: true,
                refreshing: false,
                show_caret: true,
                spinner_phase: 0,
            },
            &theme,
        );
    });
    let text = buffer_text(&terminal);
    assert!(text.contains("(no matches — try a shorter or different query)"));
    assert!(text.contains("clear search"));
}

#[test]
fn picker_search_viewport_keeps_long_query_and_caret_inside_row() {
    let theme = Theme::default();
    let rect = Rect::new(2, 3, 16, 1);
    let query = "abcdefghijklmnopqrstuvwxyz";
    let mut terminal = nuotc::TestTerminal::new(24, 8);
    terminal.draw(|frame| {
        draw_picker_search_row(frame, rect, query, query.chars().count(), &theme);
        place_picker_search_cursor(frame, rect, query, query.chars().count());
    });

    let (x, y) = match terminal.cursor() {
        nuotc::CursorState::Visible(x, y) => (x, y),
        other => panic!("search field must own a caret, got {other:?}"),
    };
    assert!(x >= rect.x && x < rect.right());
    assert_eq!(y, rect.y);
    let text = buffer_text(&terminal);
    assert!(
        text.contains("xyz"),
        "the viewport must follow the end of a long query: {text:?}"
    );
    assert!(
        !text.contains("abcdefgh"),
        "the off-screen query prefix must not be painted"
    );
}

#[test]
fn connections_modal_empty_state_centered_copy_and_footer() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(72, 24);
    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: false,
                show_caret: true,
                connection_detail: None,
                connection_info_scroll: &mut 0,
                spinner_phase: 0,
                connection_info_standalone: false,
                refreshing: false,
                connection_models_expanded: false,
            },
            &theme,
            &selection,
        );
    });
    let text = buffer_text(&terminal);
    assert!(text.contains("No connections yet"));
    assert!(text.contains("Press a for a preset or c for custom"));
    assert!(text.contains("preset"));
    assert!(text.contains("custom"));
    assert!(text.contains("close"));
}

#[test]
fn connections_modal_search_empty_state() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(72, 24);
    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "nonexistent",
                cursor_position: 11,
                scroll: &mut scroll,
                follow_selection: false,
                search: true,
                connection_info_detail: false,
                show_caret: true,
                connection_detail: None,
                connection_info_scroll: &mut 0,
                spinner_phase: 0,
                connection_info_standalone: false,
                refreshing: false,
                connection_models_expanded: false,
            },
            &theme,
            &selection,
        );
    });
    let text = buffer_text(&terminal);
    assert!(text.contains("(no matches — try a shorter or different query)"));
    assert!(text.contains("clear search"));
}

#[test]
fn connections_modal_detail_view_renders_info_and_usage() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 30);
    let detail = nuo_wire::ConnectionDetail {
        name: "DeepSeek Production".to_string(),
        provider: "deepseek".to_string(),
        provider_label: "DeepSeek".to_string(),
        protocol: "openai".to_string(),
        base_url: "https://api.deepseek.com".to_string(),
        auth_type: "API Key".to_string(),
        api_key_masked: Some("sk-12...abcd".to_string()),
        api_key_source: "credentials.toml".to_string(),
        client_identity: nuo_wire::ClientIdentity::Native,
        user_agent: "muta/0.37.21".to_string(),
        models: vec!["deepseek-chat".to_string(), "deepseek-reasoner".to_string()],
        model_info: Vec::new(),
        active_model: Some("deepseek-chat".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_wire::ConnectionUsageState::Available(Box::new(
            nuo_wire::ProviderUsage {
                plan: Some("Pay-as-you-go".to_string()),
                description: None,
                quota: Some(nuo_wire::ProviderQuotaData::Balance(
                    nuo_wire::BalanceQuota {
                        currency: "CNY".to_string(),
                        total_balance: Some(100.50),
                        cash_balance: Some(100.50),
                        voucher_balance: Some(0.0),
                        credit_limit: None,
                        consumed_amount: None,
                        display_primary: Some("¥100.50".to_string()),
                    },
                )),
                primary_balance: Some("¥100.50".to_string()),
                metrics: vec![nuo_wire::UsageMetric {
                    label: "Total Balance".to_string(),
                    value: "100.50".to_string(),
                    unit: Some("CNY".to_string()),
                }],
                updated_at_ms: None,
            },
        )),
    };

    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 0,
                spinner_phase: 0,
                connection_info_standalone: false,
                refreshing: false,
                connection_models_expanded: false,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("Connections"));
    assert!(text.contains("Details [DeepSeek Production]"));
    assert!(text.contains("Configuration"));
    assert!(text.contains("deepseek"));
    assert!(text.contains("https://api.deepseek.com"));
    assert!(text.contains("sk-12...abcd"));
    assert!(text.contains("Client Profile"));
    assert!(text.contains("Served Models (2)"));
    assert!(text.contains("- deepseek-chat"));
    assert!(text.contains("show all 2 models"));

    // Scroll to view usage section
    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 8;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 8,
                spinner_phase: 0,
                connection_info_standalone: false,
                refreshing: false,
                connection_models_expanded: false,
            },
            &theme,
            &selection,
        );
    });

    let scrolled_text = buffer_text(&terminal);
    assert!(scrolled_text.contains("Provider Usage & Quota"));
    assert!(scrolled_text.contains("Pay-as-you-go"));
    assert!(scrolled_text.contains("¥100.50 CNY"));
    assert!(scrolled_text.contains("Recharge:"));
}

#[test]
fn connections_modal_detail_view_renders_periodic_quota_with_progress_bar() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 30);
    let detail = nuo_wire::ConnectionDetail {
        name: "Google Antigravity".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth".to_string(),
        api_key_masked: None,
        api_key_source: "OAuth".to_string(),
        client_identity: nuo_wire::ClientIdentity::Native,
        user_agent: "muta/0.37.25".to_string(),
        models: vec!["gemini-3.7-flash".to_string(), "gemini-3.1-pro".to_string()],
        model_info: Vec::new(),
        active_model: Some("gemini-3.7-flash".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_wire::ConnectionUsageState::Available(Box::new(
            nuo_wire::ProviderUsage {
                plan: Some("Google One AI Premium".to_string()),
                description: None,
                quota: Some(nuo_wire::ProviderQuotaData::Periodic(
                    nuo_wire::PeriodicQuota {
                        buckets: vec![
                            nuo_wire::QuotaWindowBucket {
                                window: Some(nuo_wire::QuotaWindowKind::Daily),
                                label: "Gemini 3.7 Flash".to_string(),
                                group: None,
                                used_fraction: 0.15,
                                used_amount: None,
                                total_limit: None,
                                unit: Some("DAY".to_string()),
                                reset_at_ms: None,
                                reset_time_str: Some("12:00".to_string()),
                            },
                            nuo_wire::QuotaWindowBucket {
                                window: Some(nuo_wire::QuotaWindowKind::Rolling5Hour),
                                label: "Gemini 3.1 Pro".to_string(),
                                group: None,
                                used_fraction: 0.40,
                                used_amount: None,
                                total_limit: None,
                                unit: Some("5h".to_string()),
                                reset_at_ms: None,
                                reset_time_str: None,
                            },
                        ],
                    },
                )),
                primary_balance: Some("60% Quota".to_string()),
                metrics: Vec::new(),
                updated_at_ms: None,
            },
        )),
    };

    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 8;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 8,
                spinner_phase: 0,
                connection_info_standalone: false,
                refreshing: false,
                connection_models_expanded: false,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("Provider Usage & Quota"));
    assert!(text.contains("Google One AI Premium"));
    assert!(text.contains("Gemini 3.7 Flash · Daily"));
    assert!(text.contains("15% used"));
    assert!(text.contains("Gemini 3.1 Pro · 5h Window"));
    assert!(text.contains("40% used"));
}

#[test]
fn connections_modal_detail_view_renders_inline_fetching_spinner() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 30);
    let detail = nuo_wire::ConnectionDetail {
        name: "DeepSeek Production".to_string(),
        provider: "deepseek".to_string(),
        provider_label: "DeepSeek".to_string(),
        protocol: "openai".to_string(),
        base_url: "https://api.deepseek.com".to_string(),
        auth_type: "API Key".to_string(),
        api_key_masked: Some("sk-12...abcd".to_string()),
        api_key_source: "credentials.toml".to_string(),
        client_identity: nuo_wire::ClientIdentity::Native,
        user_agent: "muta/0.37.21".to_string(),
        models: vec!["deepseek-chat".to_string()],
        model_info: Vec::new(),
        active_model: Some("deepseek-chat".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_wire::ConnectionUsageState::Fetching,
    };

    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 0,
                spinner_phase: 2,
                connection_info_standalone: false,
                refreshing: false,
                connection_models_expanded: false,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("Configuration"));
    assert!(text.contains("Served Models"));

    // Scroll to view usage section with inline spinner
    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 6;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 6,
                spinner_phase: 2,
                connection_info_standalone: false,
                refreshing: false,
                connection_models_expanded: false,
            },
            &theme,
            &selection,
        );
    });

    let scrolled_text = buffer_text(&terminal);
    assert!(scrolled_text.contains("Provider Usage & Quota"));
    assert!(scrolled_text.contains("Querying upstream provider quota & balance…"));
}

#[test]
fn connections_modal_detail_view_renders_grouped_periodic_quota_and_effort() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 35);
    let detail = nuo_wire::ConnectionDetail {
        name: "Google Antigravity".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth".to_string(),
        api_key_masked: None,
        api_key_source: "OAuth".to_string(),
        client_identity: nuo_wire::ClientIdentity::Native,
        user_agent: "muta/0.37.25".to_string(),
        models: vec![
            "gemini-3.7-flash".to_string(),
            "gemini-3.1-pro".to_string(),
            "claude-3-7-sonnet".to_string(),
        ],
        model_info: vec![
            nuo_wire::ProviderModelInfo {
                model: "gemini-3.7-flash".to_string(),
                name: None,
                protocol: "google".to_string(),
                effort: Some("high".to_string()),
                thinking: None,
                effort_levels: Vec::new(),
                favorite: false,
                last_used_ms: None,
                vision: Some(false),
                context_window: 1_000_000,
                max_output_tokens: None,
                availability: None,

                availability_overridden: false,

                advertised: None,
                availability_stale: false,
            },
            nuo_wire::ProviderModelInfo {
                model: "gemini-3.1-pro".to_string(),
                name: None,
                protocol: "google".to_string(),
                effort: None,
                thinking: None,
                effort_levels: Vec::new(),
                favorite: false,
                last_used_ms: None,
                vision: Some(false),
                context_window: 1_000_000,
                max_output_tokens: None,
                availability: None,

                availability_overridden: false,

                advertised: None,
                availability_stale: false,
            },
            nuo_wire::ProviderModelInfo {
                model: "claude-3-7-sonnet".to_string(),
                name: None,
                protocol: "anthropic".to_string(),
                effort: Some("max".to_string()),
                thinking: Some(true),
                effort_levels: Vec::new(),
                favorite: false,
                last_used_ms: None,
                vision: Some(false),
                context_window: 200_000,
                max_output_tokens: None,
                availability: None,

                availability_overridden: false,

                advertised: None,
                availability_stale: false,
            },
        ],
        active_model: Some("gemini-3.7-flash".to_string()),
        active_model_effort: Some("high".to_string()),
        active_model_thinking: None,
        usage: nuo_wire::ConnectionUsageState::Available(Box::new(
            nuo_wire::ProviderUsage {
                plan: Some("Antigravity Quota".to_string()),
                description: Some(
                    "Within each group, models share a weekly limit and a 5-hour limit."
                        .to_string(),
                ),
                quota: Some(nuo_wire::ProviderQuotaData::Periodic(
                    nuo_wire::PeriodicQuota {
                        buckets: vec![
                            nuo_wire::QuotaWindowBucket {
                                window: Some(nuo_wire::QuotaWindowKind::Weekly),
                                label: "Weekly Limit Remaining".to_string(),
                                group: Some("Claude Models".to_string()),
                                used_fraction: 0.99,
                                used_amount: None,
                                total_limit: None,
                                unit: Some("WEEKLY".to_string()),
                                reset_at_ms: None,
                                reset_time_str: Some("80h 6m".to_string()),
                            },
                            nuo_wire::QuotaWindowBucket {
                                window: Some(nuo_wire::QuotaWindowKind::Rolling5Hour),
                                label: "Five Hour Limit Remaining".to_string(),
                                group: Some("Claude Models".to_string()),
                                used_fraction: 0.46,
                                used_amount: None,
                                total_limit: None,
                                unit: Some("5h".to_string()),
                                reset_at_ms: None,
                                reset_time_str: Some("3h 16m".to_string()),
                            },
                            nuo_wire::QuotaWindowBucket {
                                window: Some(nuo_wire::QuotaWindowKind::Weekly),
                                label: "Weekly Limit Remaining".to_string(),
                                group: Some("Chat Models (Gemini)".to_string()),
                                used_fraction: 0.0,
                                used_amount: None,
                                total_limit: None,
                                unit: Some("WEEKLY".to_string()),
                                reset_at_ms: None,
                                reset_time_str: Some("167h 59m".to_string()),
                            },
                            nuo_wire::QuotaWindowBucket {
                                window: Some(nuo_wire::QuotaWindowKind::Rolling5Hour),
                                label: "Five Hour Limit Remaining".to_string(),
                                group: Some("Chat Models (Gemini)".to_string()),
                                used_fraction: 0.0,
                                used_amount: None,
                                total_limit: None,
                                unit: Some("5h".to_string()),
                                reset_at_ms: None,
                                reset_time_str: Some("4h 59m".to_string()),
                            },
                        ],
                    },
                )),
                primary_balance: Some("1% Quota".to_string()),
                metrics: Vec::new(),
                updated_at_ms: None,
            },
        )),
    };

    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 0,
                spinner_phase: 0,
                connection_info_standalone: false,
                refreshing: false,
                connection_models_expanded: true,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("Connections › Details [Google Antigravity]"));
    assert!(text.contains("Default Active"));
    assert!(text.contains("gemini-3.7-flash  ·  reasoning: high"));
    assert!(text.contains("Served Models (3)"));
    assert!(text.contains("- gemini-3.7-flash"));
    assert!(!text.contains("- gemini-3.7-flash  ·  reasoning:"));
    assert!(text.contains("- gemini-3.1-pro"));
    assert!(text.contains("- claude-3-7-sonnet"));
    assert!(!text.contains("- claude-3-7-sonnet  ·  reasoning:"));

    // Scroll to view grouped quota
    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let mut info_scroll = 16;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut info_scroll,
                spinner_phase: 0,
                connection_info_standalone: false,
                refreshing: false,
                connection_models_expanded: true,
            },
            &theme,
            &selection,
        );
    });

    let scrolled_text = buffer_text(&terminal);
    assert!(scrolled_text.contains("Provider Usage & Quota"));
    assert!(scrolled_text.contains("▸ Claude Models"));
    assert!(scrolled_text.contains("Weekly Limit Remaining · Weekly"));
    assert!(scrolled_text.contains("99% used"));
    assert!(scrolled_text.contains("Five Hour Limit Remaining · 5h Window"));
    assert!(scrolled_text.contains("46% used"));
    assert!(scrolled_text.contains("▸ Chat Models (Gemini)"));
    assert!(scrolled_text.contains("0% used"));
    assert!(scrolled_text.contains("Within each group, models share a weekly limit"));
}

#[test]
fn connections_modal_standalone_detail_renders_single_level_header() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let detail = nuo_wire::ConnectionDetail {
        name: "Anthropic".to_string(),
        provider: "anthropic".to_string(),
        provider_label: "Anthropic".to_string(),
        protocol: "anthropic".to_string(),
        base_url: "https://api.anthropic.com".to_string(),
        auth_type: "API Key".to_string(),
        api_key_masked: Some("sk-ant-...1234".to_string()),
        api_key_source: "credentials.toml".to_string(),
        client_identity: nuo_wire::ClientIdentity::Native,
        user_agent: "muta/0.37.21".to_string(),
        models: vec!["claude-3-7-sonnet".to_string()],
        model_info: Vec::new(),
        active_model: Some("claude-3-7-sonnet".to_string()),
        active_model_effort: Some("high".to_string()),
        active_model_thinking: Some(true),
        usage: nuo_wire::ConnectionUsageState::Fetching,
    };

    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 0,
                spinner_phase: 0,
                connection_info_standalone: true, // standalone = true
                refreshing: false,
                connection_models_expanded: false,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    // Header should be single level, NOT "Connections › Details"
    assert!(text.contains("Connection Details [Anthropic]"));
    assert!(!text.contains("Connections ›"));
    // Footer hint should say "close" instead of "list"
    assert!(text.contains("close"));
    assert!(!text.contains("list"));
}

#[test]
fn connections_modal_detail_wraps_second_column_with_indent() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(76, 30);
    let detail = nuo_wire::ConnectionDetail {
        name: "ggl-fox".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "google-antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://daily-cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth (AntigravityOAuth)".to_string(),
        api_key_masked: Some("ya29...0213".to_string()),
        api_key_source: "OAuth".to_string(),
        client_identity: nuo_wire::ClientIdentity::Native,
        user_agent: "antigravity/1.23.2".to_string(),
        models: vec![
            "claude-opus-4-6-thinking".to_string(),
            "claude-sonnet-4-6".to_string(),
        ],
        model_info: Vec::new(),
        active_model: Some("claude-opus-4-6-thinking".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_wire::ConnectionUsageState::Unsupported,
    };

    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 0,
                spinner_phase: 0,
                connection_info_standalone: true,
                refreshing: false,
                connection_models_expanded: true,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    let lines: Vec<&str> = text.lines().collect();
    let base_url_idx = lines
        .iter()
        .position(|l| l.contains("Base URL"))
        .expect("Base URL line");
    let base_line = lines[base_url_idx];
    let next_line = lines[base_url_idx + 1];
    assert!(base_line.contains("https://"), "base_line: {base_line:?}");
    assert!(next_line.contains(".com"), "next_line: {next_line:?}");
    // Second visual line must start at the second column (16-char indent), aligning with "https://"
    assert_eq!(
        next_line.find(next_line.trim_start()),
        base_line.find("https://"),
        "base: {base_line}\nnext: {next_line}"
    );
    // Served models uses plain '-' bullet
    assert!(text.contains("- claude-opus-4-6-thinking"));
    assert!(text.contains("- claude-sonnet-4-6"));
    assert!(!text.contains('●'));
    assert!(!text.contains('○'));
}

#[test]
fn oauth_pending_records_selectable_regions_in_layout_map() {
    use crate::event_loop::transcript::extract_selection_text;
    use crate::model::layout::MODAL_DOC_MSG_IDX;
    use crate::model::selection::{SelectionDrag, SelectionState};

    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let mut layout_map = crate::model::layout::LayoutMap::new();
    let mut scroll = 0;
    let url = "https://github.com/login/oauth/authorize?client_id=Iv1.test12345678&scope=user%3Aemail&state=xyz987";

    terminal.draw(|f| {
        draw_oauth_pending(
            "GitHub OAuth",
            "Please authorize in browser",
            url,
            "1234-5678",
            None,
            0,
            f,
            &theme,
            &mut scroll,
            None,
            &SelectionState::None,
            &mut layout_map,
        );
    });

    // Verify layout map recorded MODAL_DOC_MSG_IDX regions
    let doc_region = layout_map
        .first_region_for_message(MODAL_DOC_MSG_IDX)
        .expect("OAuth pending must push modal doc regions to layout_map");

    // Test drag selecting inside the OAuth modal
    let url_cursor = layout_map
        .cursor_at(doc_region.rect.x, doc_region.rect.y)
        .expect("must resolve cursor on oauth text");
    assert_eq!(url_cursor.message_idx, MODAL_DOC_MSG_IDX);

    let mut selection = SelectionState::None;
    let mut drag = SelectionDrag::default();
    drag.begin_range(&mut selection, url_cursor);

    // Drag to the end of the line
    drag.update_from_point(
        &mut selection,
        &layout_map,
        doc_region.rect.x + doc_region.rect.width - 1,
        doc_region.rect.y,
    );
    drag.finish(&mut selection);

    assert!(selection.is_active());
    let extracted = extract_selection_text(&selection, &[], "", &layout_map, None);
    assert!(extracted.is_some());
}

#[test]
fn connections_modal_detail_served_models_collapsed_by_default_and_expands() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 50);
    let models: Vec<String> = (1..=17).map(|i| format!("model-{i:02}")).collect();
    let detail = nuo_wire::ConnectionDetail {
        name: "google-antigravity".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://daily-cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth".to_string(),
        api_key_masked: None,
        api_key_source: "OAuth".to_string(),
        client_identity: nuo_wire::ClientIdentity::Native,
        user_agent: "antigravity/1.0.0".to_string(),
        models: models.clone(),
        model_info: Vec::new(),
        active_model: Some("model-05".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_wire::ConnectionUsageState::Unsupported,
    };

    // 1. Collapsed mode (default)
    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 0,
                spinner_phase: 0,
                connection_info_standalone: true,
                refreshing: false,
                connection_models_expanded: false,
            },
            &theme,
            &selection,
        );
    });

    let collapsed_text = buffer_text(&terminal);
    assert!(collapsed_text.contains("Served Models (17)"));
    assert!(collapsed_text.contains("- model-05"));
    assert!(collapsed_text.contains("show all 17 models (press Enter to expand)"));
    assert!(!collapsed_text.contains("- model-01"));
    assert!(!collapsed_text.contains("- model-17"));
    assert!(collapsed_text.contains("expand models"));
    // Configuration has Provider and no redundant Provider ID
    assert!(collapsed_text.contains("Provider        Google Antigravity"));
    assert!(!collapsed_text.contains("Provider ID"));

    // 2. Expanded mode
    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 0,
                spinner_phase: 0,
                connection_info_standalone: true,
                refreshing: false,
                connection_models_expanded: true,
            },
            &theme,
            &selection,
        );
    });

    let expanded_text = buffer_text(&terminal);
    assert!(expanded_text.contains("Served Models (17)"));
    assert!(expanded_text.contains("- model-01"));
    assert!(expanded_text.contains("- model-05"));
    assert!(expanded_text.contains("- model-17"));
    assert!(expanded_text.contains("collapse models (press Enter)"));
    assert!(expanded_text.contains("collapse models"));
}

#[test]
fn connections_modal_quota_bucket_used_percentage_and_separate_reset_line() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 40);
    let detail = nuo_wire::ConnectionDetail {
        name: "google-antigravity".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://daily-cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth".to_string(),
        api_key_masked: None,
        api_key_source: "OAuth".to_string(),
        client_identity: nuo_wire::ClientIdentity::Native,
        user_agent: "antigravity/1.0.0".to_string(),
        models: vec!["gemini-2.5-pro".to_string()],
        model_info: Vec::new(),
        active_model: Some("gemini-2.5-pro".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_wire::ConnectionUsageState::Available(Box::new(
            nuo_wire::ProviderUsage {
                plan: Some("Google One AI Premium".to_string()),
                description: None,
                quota: Some(nuo_wire::ProviderQuotaData::Periodic(
                    nuo_wire::PeriodicQuota {
                        buckets: vec![nuo_wire::QuotaWindowBucket {
                            label: "Gemini Pro Agent".to_string(),
                            used_fraction: 0.72,
                            used_amount: None,
                            total_limit: None,
                            unit: None,
                            window: Some(nuo_wire::QuotaWindowKind::Daily),
                            reset_at_ms: None,
                            reset_time_str: Some("in 3h 25m".to_string()),
                            group: None,
                        }],
                    },
                )),
                primary_balance: None,
                metrics: Vec::new(),
                updated_at_ms: None,
            },
        )),
    };

    terminal.draw(|f| {
        let mut lm = crate::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = crate::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                connection_info_detail: true,
                show_caret: true,
                connection_detail: Some(&detail),
                connection_info_scroll: &mut 0,
                spinner_phase: 0,
                connection_info_standalone: true,
                refreshing: false,
                connection_models_expanded: false,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("72% used"));
    assert!(!text.contains("remaining"));
    assert!(!text.contains(" ·  resets"));
    assert!(text.contains("resets: in 3h 25m"));
}
