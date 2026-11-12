use super::*;

// Sectioned Models list (Favorites / Recent / All models)

/// A snapshot with one favorite, two used models, and two plain models,
/// so all three sections render and RECENT has a meaningful internal
/// order (gpt-5.5 newer than claude-opus-4-8).
fn sectioned_snapshot() -> nuo_protocol::ProviderPickerSnapshot {
    // `enabled` is the provider's picker declaration: `true` = an ordinary
    // selectable row, `false` = a subscription-locked one. The lock case has
    // its own test (`models_modal_lists_provider_locked_rows_greyed_with_a_lock_tag`)
    // and builds its own rows; everything here is the normal path.
    let info = |model: &str, favorite: bool, used: Option<u64>, enabled: bool| {
        nuo_protocol::ProviderModelInfo {
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
                nuo_protocol::Availability::usable()
            } else {
                nuo_protocol::Availability::locked(None)
            }),
            availability_overridden: false,
            advertised: None,
            availability_stale: false,
        }
    };
    let row = |id: &str, name: &str, models: Vec<nuo_protocol::ProviderModelInfo>| {
        nuo_protocol::ProviderPickerRow {
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
            headers: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }
    };
    nuo_protocol::ProviderPickerSnapshot {
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
    let ranked =
        nuo_view::providers::models_flat_filtered_from(&picker, "openai", "gpt-5.5", query);
    let mut terminal = nuotc::TestTerminal::new(72, 28);
    terminal.draw(|f| {
        let mut scroll = 0;
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_models_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::models::ModelsModalProps {
                models: &ranked,
                modal_index,
                query,
                cursor_position: query.len(),
                scroll: &mut scroll,
                follow_selection: true,
                search,
                show_caret: true,
                refreshing: false,
                spinner_phase: 0,
                info_detail: false,
                info_scroll: &mut 0,
                reveal_active: &mut false,
            },
            &theme,
            &selection,
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
    let info = |model: &str, availability: Option<nuo_protocol::Availability>| {
        nuo_protocol::ProviderModelInfo {
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
    picker.rows.push(nuo_protocol::ProviderPickerRow {
        id: "qoder".into(),
        name: "Qoder".into(),
        model: "gmodel".into(),
        models: vec!["qfmodel".into(), "gmodel".into()],
        model_info: vec![
            info("qfmodel", Some(nuo_protocol::Availability::usable())),
            info("gmodel", Some(nuo_protocol::Availability::locked(None))),
        ],
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        provider: String::new(),
        headers: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    });
    let ranked = nuo_view::providers::models_flat_filtered_from(&picker, "openai", "gpt-5.5", "");
    assert!(ranked.iter().any(|r| r.model == "gmodel" && !r.usable));
    assert!(ranked.iter().any(|r| r.model == "qfmodel" && r.usable));
    let mut terminal = nuotc::TestTerminal::new(72, 28);
    terminal.draw(|f| {
        let mut scroll = 0;
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_models_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::models::ModelsModalProps {
                models: &ranked,
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
                info_detail: false,
                info_scroll: &mut 0,
                reveal_active: &mut false,
            },
            &theme,
            &selection,
        );
    });
    let text = buffer_text(&terminal);
    assert!(text.contains("gmodel"), "locked row is listed");
    assert!(text.contains("disabled"), "disabled tag renders");

    // ADR-0076: verify that locked rows render completely dimmed without bold weight
    let (lines, _, _) =
        crate::shell::overlays::provider::models::model_list_body_at(&ranked, 0, &theme, 72, 0);
    let locked_line = lines
        .iter()
        .find(|l| {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            text.contains("gmodel")
        })
        .expect("locked line present");
    for span in &locked_line.spans {
        if span.content.trim().is_empty() {
            continue;
        }
        assert!(
            !span.style.add.contains(nuotc::Modifier::BOLD),
            "locked model text must not be bold: {:?}",
            span.content
        );
        assert_eq!(
            span.style.fg,
            theme.dim(),
            "locked model text must be styled dimmed: {:?}",
            span.content
        );
    }
}

/// When the provider states a reason, the row shows *that* — nuo
/// must never invent "locked for the current plan", which is a different,
/// unstated diagnosis (the `403 code 110` quota incident is exactly a case
/// where the plan is fine).
#[test]
fn models_modal_shows_the_providers_own_reason_verbatim() {
    let theme = Theme::default();
    let mut picker = sectioned_snapshot();
    let info = |model: &str, availability: Option<nuo_protocol::Availability>| {
        nuo_protocol::ProviderModelInfo {
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
    picker.rows.push(nuo_protocol::ProviderPickerRow {
        id: "qoder".into(),
        name: "Qoder".into(),
        model: "degraded".into(),
        models: vec!["degraded".into(), "overridden".into()],
        model_info: vec![
            info(
                "degraded",
                Some(nuo_protocol::Availability::locked(Some(
                    "Billing daily count exceeded".to_string(),
                ))),
            ),
            nuo_protocol::ProviderModelInfo {
                availability: Some(nuo_protocol::Availability::usable()),
                availability_overridden: true,
                ..info("overridden", None)
            },
        ],
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        provider: String::new(),
        headers: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    });
    let ranked = nuo_view::providers::models_flat_filtered_from(&picker, "openai", "gpt-5.5", "");
    let mut terminal = nuotc::TestTerminal::new(96, 28);
    terminal.draw(|f| {
        let mut scroll = 0;
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_models_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::models::ModelsModalProps {
                models: &ranked,
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: true,
                search: false,
                show_caret: true,
                refreshing: false,
                spinner_phase: 0,
                info_detail: false,
                info_scroll: &mut 0,
                reveal_active: &mut false,
            },
            &theme,
            &selection,
        );
    });
    let text = buffer_text(&terminal);
    assert!(
        text.contains("disabled"),
        "clean disabled badge rendered: {text}"
    );
    assert!(
        text.contains("overridden"),
        "an override of an upstream lock is disclosed, not silent: {text}"
    );

    // Verify the detail view renders the provider's reason verbatim
    let degraded_idx = ranked
        .iter()
        .position(|r| r.model == "degraded")
        .expect("degraded row present");
    terminal.draw(|f| {
        let mut scroll = 0;
        let mut info_scroll = 0;
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_models_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::models::ModelsModalProps {
                models: &ranked,
                modal_index: degraded_idx,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: true,
                search: false,
                show_caret: true,
                refreshing: false,
                spinner_phase: 0,
                info_detail: true,
                info_scroll: &mut info_scroll,
                reveal_active: &mut false,
            },
            &theme,
            &selection,
        );
    });
    let detail_text = buffer_text(&terminal);
    assert!(
        detail_text.contains("Billing daily count exceeded"),
        "the provider's own reason is rendered verbatim in detail view: {detail_text}"
    );
    assert!(
        !text.contains("locked for the current plan"),
        "nuo must not invent a plan diagnosis the provider never stated"
    );
    assert!(
        text.contains("overridden"),
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
    // The id column is the row's first casualty: a fragment of the only
    // typeable handle reads as a *different* model, so the column vanishes
    // whole rather than degrading (ADR-0152 `[INV-ROW-GRID-01]`). The label it
    // annotates is never sacrificed to keep it alive.
    let narrow = render_labelled_models_modal(40);
    let row = narrow
        .lines()
        .find(|line| line.contains("DeepSeek V4.1"))
        .expect("labelled row renders on a narrow terminal");
    assert!(
        !row.contains("gemini-3-pro"),
        "no room for the id column means no id column: {row:?}"
    );
    // The label is *not* what paid for the id: at this width it is squeezed by
    // the shared budget like every other column, and never by a neighbour that
    // no longer fits. What the assertion pins is that dropping the id bought the
    // label room, not that the label is immune to truncation.
    assert!(
        !row.contains("Deep…"),
        "the label keeps its own head: {row:?}"
    );

    // The wider render proves the two differ only by the id column, so the
    // assertion above is about the column and not about a missing row.
    let wide = render_labelled_models_modal(110);
    assert!(wide.contains("DeepSeek V4.1 Flash"));
    assert!(wide.contains("gemini-3-pro"));
}

#[test]
fn models_modal_static_rows_truncate_by_semantics_while_the_focus_reveals() {
    // Two halves of one contract (ADR-0152):
    //
    // 1. A **static** row degrades a wire id in the middle, because both its
    //    ends discriminate (`claude…0219` names the family *and* the version).
    //    Head-truncating it would destroy exactly the part that differs from the
    //    model above it.
    // 2. The **focused** row instead reveals the full string by sliding a window
    //    over it — the terminal has no tooltip, so motion is the only way a
    //    truncated tail is reachable without widening the window. The window
    //    discloses itself: a leading marker once it has scrolled past the head.
    let theme = Theme::default();
    let model = "claude-3-7-sonnet-20250219";
    let row = |name: &str| nuo_view::providers::RankedModel {
        section: nuo_view::providers::ModelSection::All,
        provider_id: "anthropic".to_string(),
        model: model.to_string(),
        name: Some(name.to_string()),
        provider_label: "anthropic".to_string(),
        effort: None,
        thinking: None,
        effort_levels: vec![],
        favorite: false,
        usable: true,
        status_label: None,
        resets_at_ms: None,
        vision: None,
        context_window: 200_000,
        reason: None,
        availability_overridden: false,
        availability_stale: false,
        last_used_ms: None,
        match_id: None,
        match_name: None,
        match_connection: None,
        relevance_score: 0,
    };
    let ranked = vec![
        row("Claude 3.7 Sonnet Production"),
        row("Claude 3.7 Sonnet Preview"),
    ];

    // The row that is *not* focused is the second one, so it truncates by the
    // field's semantics; the first row reveals.
    let render = |selected: usize, elapsed_ms: u64| {
        let (lines, _, revealing) = crate::shell::overlays::provider::models::model_list_body_at(
            &ranked, selected, &theme, 70, elapsed_ms,
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        (texts, revealing)
    };

    let (texts, _) = render(0, 0);
    let unfocused = texts
        .iter()
        .find(|t| t.contains("Previ") || t.contains("Pre…"))
        .expect("second row renders");
    assert!(
        unfocused.contains('…'),
        "a static row truncates its id: {unfocused:?}"
    );
    assert!(
        unfocused.contains("0219") || unfocused.contains("20250"),
        "middle truncation keeps the id's discriminating tail: {unfocused:?}"
    );

    // The focused row reveals instead of truncating: at rest it shows the head,
    // and later in the cycle the same cell shows a *different* slice of the same
    // string — which is what proves the window slides rather than the text being
    // re-truncated.
    let (rest, revealing) = render(0, 0);
    assert!(revealing, "the focused row reports that it is revealing");
    let focused_at_rest = rest
        .iter()
        .find(|t| t.contains("Production") || t.contains("Produ…"))
        .expect("focused row renders");
    let (slid, _) = render(0, 2_000);
    let focused_later = slid
        .iter()
        .find(|t| t.contains("Produ") || t.contains("20250219") || t.contains("…aude"))
        .expect("focused row still renders");
    assert_ne!(
        focused_at_rest, focused_later,
        "the focused row's cell slides instead of staying truncated"
    );
    assert!(
        focused_later.contains('…'),
        "a window discloses that it is a window: {focused_later:?}"
    );
}

#[test]
fn models_modal_long_label_preserves_connection_column_alignment_and_gap() {
    let theme = Theme::default();
    let row1 = nuo_view::providers::RankedModel {
        section: nuo_view::providers::ModelSection::Recent,
        provider_id: "cc-main".to_string(),
        model: "deepseek-flash".to_string(),
        name: Some("DeepSeek V4.1 Flash Fast (latest)".to_string()),
        provider_label: "cc-main".to_string(),
        effort: Some("high".to_string()),
        thinking: None,
        effort_levels: vec![],
        favorite: false,
        usable: true,
        status_label: None,
        resets_at_ms: None,
        vision: None,
        context_window: 128_000,
        reason: None,
        availability_overridden: false,
        availability_stale: false,
        last_used_ms: None,
        match_id: None,
        match_name: None,
        match_connection: None,
        relevance_score: 0,
    };
    let row2 = nuo_view::providers::RankedModel {
        section: nuo_view::providers::ModelSection::Recent,
        provider_id: "ga-main".to_string(),
        model: "gemini-3.8-flash".to_string(),
        name: Some("Gemini 3.8 Flash".to_string()),
        provider_label: "ga-main".to_string(),
        effort: Some("high".to_string()),
        thinking: None,
        effort_levels: vec![],
        favorite: false,
        usable: true,
        status_label: None,
        resets_at_ms: None,
        vision: None,
        context_window: 128_000,
        reason: None,
        availability_overridden: false,
        availability_stale: false,
        last_used_ms: None,
        match_id: None,
        match_name: None,
        match_connection: None,
        relevance_score: 0,
    };
    let ranked = vec![row1, row2];
    // body_width = 58: the width a 72-column terminal leaves the MODELS modal's
    // body. The name no longer fits, so this is exactly the case the column
    // budget exists for.
    let (lines, _, _) =
        crate::shell::overlays::provider::models::model_list_body_at(&ranked, 0, &theme, 58, 0);
    let line1 = lines
        .iter()
        .find(|l| {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            text.contains("DeepSeek V4.1")
        })
        .expect("row1 line rendered");
    let line2 = lines
        .iter()
        .find(|l| {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            text.contains("Gemini 3.8 Flash")
        })
        .expect("row2 line rendered");

    let text1: String = line1.spans.iter().map(|s| s.content.as_ref()).collect();
    let text2: String = line2.spans.iter().map(|s| s.content.as_ref()).collect();

    // Display columns, not byte offsets: a truncated cell carries `…`, which is
    // three bytes wide but one column, so byte arithmetic would report a
    // misalignment that does not exist on screen.
    let col_of = |text: &str, needle: &str| {
        let byte = text.find(needle).expect("needle rendered");
        unicode_width::UnicodeWidthStr::width(&text[..byte])
    };
    let cc_main_idx = col_of(&text1, "cc-main");
    let ga_main_idx = col_of(&text2, "ga-main");

    // 1. The connection column starts at the same column in both rows, however
    //    long the name above it happens to be — positions come from the shared
    //    budget, never from a neighbour's demand.
    assert_eq!(
        cc_main_idx, ga_main_idx,
        "connection column must align across rows: line1={text1:?}, line2={text2:?}"
    );

    // 2. A truncated label still leaves at least GROUP_GAP before the connection
    //    column: the columns may shrink, they may never touch.
    let label1_end =
        unicode_width::UnicodeWidthStr::width(&text1[..text1.find("DeepSeek V4.1").unwrap()])
            + "DeepSeek V4.1".len();
    assert!(
        cc_main_idx >= label1_end + nuo_ui_primitives::components::row::GROUP_GAP,
        "label and connection column must keep GROUP_GAP separation: \
         gap = {}, text1={text1:?}",
        cc_main_idx - label1_end
    );

    // 3. The connection is legible in full. The reported defect was that a long
    //    model name starved this column down to `c…`; a floor of ten columns
    //    means the account a model spends is always readable.
    assert!(
        text1.contains("cc-main") && text2.contains("ga-main"),
        "the connection is never truncated to a fragment: {text1:?} / {text2:?}"
    );
}

#[test]
fn models_modal_omits_context_size_from_list_rows_but_keeps_specs_tags() {
    let theme = Theme::default();
    let row = nuo_view::providers::RankedModel {
        section: nuo_view::providers::ModelSection::All,
        provider_id: "anthropic".to_string(),
        model: "claude-3-7-sonnet".to_string(),
        name: Some("Claude 3.7 Sonnet".to_string()),
        provider_label: "Anthropic".to_string(),
        effort: Some("high".to_string()),
        thinking: Some(true),
        effort_levels: vec!["low".into(), "medium".into(), "high".into()],
        favorite: false,
        usable: true,
        status_label: None,
        resets_at_ms: None,
        vision: Some(true),
        context_window: 200_000,
        reason: None,
        availability_overridden: false,
        availability_stale: false,
        last_used_ms: None,
        match_id: None,
        match_name: None,
        match_connection: None,
        relevance_score: 0,
    };
    let ranked = vec![row];
    let (lines, _, _) =
        crate::shell::overlays::provider::models::model_list_body_at(&ranked, 0, &theme, 90, 0);
    let line = lines
        .iter()
        .find(|l| {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            text.contains("Claude 3.7 Sonnet")
        })
        .expect("model row renders");

    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

    // 1. Context size (200k / tokens) MUST NOT be present in the model list row
    assert!(
        !text.contains("200k") && !text.contains("200K") && !text.contains("tokens"),
        "context size column must be removed from model list row: {text:?}"
    );

    // 2. Vision and reasoning effort tags MUST remain present in specs
    assert!(
        text.contains("vis"),
        "vision capability tag must be preserved in specs: {text:?}"
    );
    assert!(
        text.contains("high"),
        "effort capability tag must be preserved in specs: {text:?}"
    );

    // 3. Provider/connection column must render
    assert!(
        text.contains("Anthropic"),
        "provider column must render: {text:?}"
    );
}

#[test]
fn models_modal_column_widths_respect_percentage_caps_and_demand() {
    let theme = Theme::default();
    let row1 = nuo_view::providers::RankedModel {
        section: nuo_view::providers::ModelSection::All,
        provider_id: "openai".to_string(),
        model: "gpt-4o-2024-11-20".to_string(),
        name: Some("GPT-4o Omnimodel High Throughput Production Version".to_string()),
        provider_label: "OpenAI Platform Direct".to_string(), // 22 chars
        effort: None,
        thinking: None,
        effort_levels: vec![],
        favorite: false,
        usable: true,
        status_label: None,
        resets_at_ms: None,
        vision: Some(true),
        context_window: 128_000,
        reason: None,
        availability_overridden: false,
        availability_stale: false,
        last_used_ms: None,
        match_id: None,
        match_name: None,
        match_connection: None,
        relevance_score: 0,
    };
    let row2 = nuo_view::providers::RankedModel {
        section: nuo_view::providers::ModelSection::All,
        provider_id: "openai".to_string(),
        model: "o3-mini".to_string(),
        name: Some("o3 Mini".to_string()),
        provider_label: "OpenAI Platform Direct".to_string(),
        effort: Some("high".to_string()),
        thinking: Some(true),
        effort_levels: vec!["low".into(), "medium".into(), "high".into()],
        favorite: false,
        usable: true,
        status_label: None,
        resets_at_ms: None,
        vision: None,
        context_window: 200_000,
        reason: None,
        availability_overridden: false,
        availability_stale: false,
        last_used_ms: None,
        match_id: None,
        match_name: None,
        match_connection: None,
        relevance_score: 0,
    };
    let ranked = vec![row1, row2];

    // On body_width = 100:
    // Connection cap is 20% (20 cols), status is 0, specs cap is 12% (12 cols).
    // Primary label cap is 60% (60 cols).
    let (lines, _, _) =
        crate::shell::overlays::provider::models::model_list_body_at(&ranked, 0, &theme, 100, 0);

    let text1: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
    let text2: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();

    // 1. Connection column should start at the same column index in both rows
    let conn1_byte = text1.find("OpenAI Platform").expect("row1 conn rendered");
    let conn2_byte = text2.find("OpenAI Platform").expect("row2 conn rendered");
    let conn1_col = unicode_width::UnicodeWidthStr::width(&text1[..conn1_byte]);
    let conn2_col = unicode_width::UnicodeWidthStr::width(&text2[..conn2_byte]);
    assert_eq!(
        conn1_col, conn2_col,
        "connection columns must align in display width"
    );

    // 2. Neither row should show context size tokens
    assert!(!text1.contains("128k") && !text1.contains("128K"));
    assert!(!text2.contains("200k") && !text2.contains("200K"));

    // 3. Row 1 has vision, Row 2 has effort - specs aligns
    assert!(text1.contains("vis"), "row 1 has vision");
    assert!(text2.contains("high"), "row 2 has effort");
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
    let ranked = nuo_view::providers::models_flat_filtered_from(&picker, "openai", "gpt-5.5", "");
    let mut terminal = nuotc::TestTerminal::new(width, 40);
    terminal.draw(|f| {
        let mut scroll = 0;
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_models_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::models::ModelsModalProps {
                models: &ranked,
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: true,
                search: false,
                refreshing: false,
                show_caret: true,
                spinner_phase: 0,
                info_detail: false,
                info_scroll: &mut 0,
                reveal_active: &mut false,
            },
            &theme,
            &selection,
        );
    });
    buffer_text(&terminal)
}

/// One Qoder catalog entry: an opaque `key` plus the provider's own
/// `display_name`, which is what actually identifies the model to a human.
fn qoder_model_info(
    model: &str,
    name: &str,
    availability: Option<nuo_protocol::Availability>,
) -> nuo_protocol::ProviderModelInfo {
    nuo_protocol::ProviderModelInfo {
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
    let picker = nuo_protocol::ProviderPickerSnapshot {
        default_id: "qoder".into(),
        rows: vec![nuo_protocol::ProviderPickerRow {
            id: "qoder".into(),
            name: "Qoder".into(),
            model: "qfmodel".into(),
            models: vec!["auto".into(), "qfmodel".into(), "gmodel".into()],
            model_info: vec![
                qoder_model_info("auto", "Auto", None),
                qoder_model_info(
                    "qfmodel",
                    "Qwen3.8-Flash",
                    Some(nuo_protocol::Availability::usable()),
                ),
                qoder_model_info(
                    "gmodel",
                    "GLM-5.3",
                    Some(nuo_protocol::Availability::locked(None)),
                ),
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            headers: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };
    let ranked = nuo_view::providers::models_flat_filtered_from(&picker, "qoder", "qfmodel", query);
    let mut terminal = nuotc::TestTerminal::new(width, 20);
    terminal.draw(|f| {
        let mut scroll = 0;
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_models_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::models::ModelsModalProps {
                models: &ranked,
                modal_index: 0,
                query,
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: true,
                search: !query.is_empty(),
                show_caret: true,
                refreshing: false,
                spinner_phase: 0,
                info_detail: false,
                info_scroll: &mut 0,
                reveal_active: &mut false,
            },
            &theme,
            &selection,
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
    assert!(
        locked.contains("disabled"),
        "disabled tag renders: {locked:?}"
    );

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
    let by_id = nuo_view::providers::models_flat_filtered_from(&picker, "", "", "gpt-5.5");
    assert!(
        by_id
            .iter()
            .any(|m| m.model == "gpt-5.5" && m.match_id.is_some())
    );

    // 2. Match by model display name
    let by_name = nuo_view::providers::models_flat_filtered_from(&picker, "", "", "Turbo");
    assert!(
        by_name
            .iter()
            .any(|m| m.model == "gpt-5.5" && m.match_name.is_some())
    );

    // 3. Match by connection/provider name
    let by_conn = nuo_view::providers::models_flat_filtered_from(&picker, "", "", "Official");
    assert!(
        by_conn
            .iter()
            .any(|m| m.provider_label == "OpenAI Official" && m.match_connection.is_some())
    );

    // 4. [INV-MODEL-SEARCH-01, INV-MODEL-SEARCH-03] Cross-field hybrid match: connection + model ID
    let by_conn_and_id =
        nuo_view::providers::models_flat_filtered_from(&picker, "", "", "Official gpt-5.5");
    assert_eq!(by_conn_and_id.len(), 2); // In Recent and All sections
    assert!(
        by_conn_and_id
            .iter()
            .all(|m| m.model == "gpt-5.5" && m.match_connection.is_some() && m.match_id.is_some())
    );

    // 5. [INV-MODEL-SEARCH-02] Path-style and reversed-order hybrid match
    let by_slash =
        nuo_view::providers::models_flat_filtered_from(&picker, "", "", "Official/Turbo");
    assert_eq!(by_slash.len(), 2);
    assert!(
        by_slash.iter().all(|m| m.model == "gpt-5.5"
            && m.match_connection.is_some()
            && m.match_name.is_some())
    );

    let by_rev = nuo_view::providers::models_flat_filtered_from(&picker, "", "", "Turbo Official");
    assert_eq!(by_rev.len(), 2);
    assert!(
        by_rev.iter().all(|m| m.model == "gpt-5.5"
            && m.match_connection.is_some()
            && m.match_name.is_some())
    );
}

#[test]
fn models_modal_empty_state_centered_copy_and_footer() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(72, 24);
    terminal.draw(|f| {
        let mut scroll = 0;
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_models_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::models::ModelsModalProps {
                models: &[],
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                refreshing: false,
                show_caret: true,
                spinner_phase: 0,
                info_detail: false,
                info_scroll: &mut 0,
                reveal_active: &mut false,
            },
            &theme,
            &selection,
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
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_models_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::models::ModelsModalProps {
                models: &[],
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut scroll,
                follow_selection: false,
                search: false,
                show_caret: true,
                refreshing: true,
                spinner_phase: 1,
                info_detail: false,
                info_scroll: &mut 0,
                reveal_active: &mut false,
            },
            &theme,
            &selection,
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
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_models_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::models::ModelsModalProps {
                models: &[],
                modal_index: 0,
                query: "xyz",
                cursor_position: 3,
                scroll: &mut scroll,
                follow_selection: false,
                search: true,
                refreshing: false,
                show_caret: true,
                spinner_phase: 0,
                info_detail: false,
                info_scroll: &mut 0,
                reveal_active: &mut false,
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
fn models_modal_never_paints_a_one_column_stub_in_place_of_a_field() {
    // A one-column `…` identifies nothing, so the solver drops the field whole
    // (ADR-0152 phase 3c). This pins the *visible* consequence: at no width may
    // a connection column be rendered as a single-column fragment, and the row's
    // identity must always survive.
    let theme = Theme::default();
    let row =
        |name: &str, model: &str, conn: &str, usable: bool| nuo_view::providers::RankedModel {
            section: nuo_view::providers::ModelSection::All,
            provider_id: "p".to_string(),
            model: model.to_string(),
            name: Some(name.to_string()),
            provider_label: conn.to_string(),
            effort: None,
            thinking: None,
            effort_levels: vec![],
            favorite: false,
            usable,
            status_label: (!usable).then(|| "disabled".to_string()),
            resets_at_ms: None,
            vision: None,
            context_window: 1,
            reason: None,
            availability_overridden: false,
            availability_stale: false,
            last_used_ms: None,
            match_id: None,
            match_name: None,
            match_connection: None,
            relevance_score: 0,
        };
    let ranked = vec![
        row("Model Name", "model-id", "connection", false),
        row("Ok", "ok", "c", true),
    ];

    for body_width in 12..=60 {
        let (lines, _, _) = crate::shell::overlays::provider::models::model_list_body_at(
            &ranked, 5, &theme, body_width, 0,
        );
        let text: String = lines
            .iter()
            .find(|l| {
                let t: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                t.contains("Model") || t.contains("Mode")
            })
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .unwrap_or_default();
        assert!(
            !text.is_empty(),
            "body {body_width}: the row's identity is never dropped"
        );
        // A lone ellipsis field would read as `…` surrounded by gaps.
        assert!(
            !text.contains("  …  ") && !text.trim_end().ends_with("  …"),
            "body {body_width}: no field may degrade to a one-column stub: {text:?}"
        );
        assert!(
            unicode_width::UnicodeWidthStr::width(text.as_str()) <= body_width,
            "body {body_width}: the row never overflows: {text:?}"
        );
    }
}
