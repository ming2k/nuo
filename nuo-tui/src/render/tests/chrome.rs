//! Chrome and layout shells: footer stack, too-small notice, empty states, brand head band, H1 underline rules, config appearance pages.

use super::*;

/// A fixed session head for the scene tests (ADR-0024): the head band's top row
/// is now the uniform session identity drawn by the settings/dashboard views.
fn test_session_head() -> SessionHead<'static> {
    SessionHead {
        session_id: "sess-01a2b3c4",
        workspace: "~/workspace",
        role: Some("developer"),
        switching_target: None,
        tabs: None,
        active_tab: 0,
    }
}

/// Smoke-render every redesigned component into a buffer to catch panics
/// (border math, rect underflows, empty content) without a live terminal.
#[test]
fn redesigned_components_render_without_panicking() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 30);

    terminal
            .draw(|f| {
                let mut layout_map = LayoutMap::new();
                let mut thinking = TranscriptMessage::reasoning("Reasoning about the task step by step.");
                thinking.set_reasoning_expanded(true);
                let mut tool = TranscriptMessage::tool_step("call_1", "list_dir", r#"{"path":"."}"#);
                tool.set_tool_step_expanded(true);
                tool.finish_tool_step("call_1", "file_a\nfile_b", nuo_wire::ToolOutput::text("file_a\nfile_b"), 12);
                let messages = vec![
                    TranscriptMessage::new(nuo_wire::Role::User, "hi"),
                    TranscriptMessage::new(
                        nuo_wire::Role::Assistant,
                        "Here is a table:\n\n| Tool | Count |\n| --- | ---: |\n| read | 1 |\n| webfetch | 250 |",
                    ),
                    thinking,
                    tool,
                ];
                let _ = draw_transcript(
                    f,
                    &mut layout_map,
                    TranscriptProps {
                        messages: &messages,
                        scroll: 0,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        backoff_clause: None,
                        activity: "waiting for model",
                        awaiting_permission: false,
                        spinner_phase: 0,
                        input: "hello",
                        byte_cursor: 5,
                        chrome_hidden: false,
                        queue_bar: QueueBarProps {
                            items: &[],
                            paused: false,
                            blocked: false,
                            expand_key: Some(crate::keymap::Key::CTRL_Q),
                        },
                        tasks_bar: Default::default(),
                        persistence_health: None,
                        subagent_bar: None,
                        side_banner: None,
                        page_hints: None,
                    session_head: None,
                                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme: &theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: None,
                    },
                );
                draw_composer(
                    ComposerProps {
                        frame: f,
                        input_rect: Rect::new(0, 21, 80, 3),
                        theme: &theme,
                        layout_map: &mut LayoutMap::new(),
                        input_scroll: &mut 0,
                        selection: &SelectionState::None,
                    },
                    ComposerText {
                        input: "hello",
                        byte_cursor: 5,
                    },
                    true,
                    true,
                    true,
                    0,
                    0,
                    crate::components::composer_hints::ComposerHints::default(),
                );
                draw_completion_menu(
                    f,
                    &mut layout_map,
                    None,
                    &[
                        crate::completion::Completion {
                            label: "/new".to_string(),
                            description: "New".to_string(),
                            insert_text: "/new".to_string(),
                            replace_start: 0,
                            replace_end: 0,
                            kind: crate::completion::CompletionItemKind::Slash,
                            alias_of: None,
                            doc: None,
                        },
                    ],
                    Some(0),
                    Rect::new(2, 20, 1, 1),
                    &theme,
                );
                draw_copy_toast(f, "copied to clipboard", false, &theme);
                draw_armed_toast(f, "press Ctrl-c again to exit", &theme);
            });

    // Modals + permission sheet on a fresh frame.
    terminal.draw(|f| {
        draw_connections_modal(
            f,
            &mut LayoutMap::new(),
            crate::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
                current_provider: "mock",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut 0,
                follow_selection: true,
                search: false,
                show_caret: true,
                connection_info_detail: false,
                connection_detail: None,
                connection_info_scroll: &mut 0,
                spinner_phase: 0,
                connection_info_standalone: false,
                refreshing: false,
                connection_models_expanded: false,
                connection_usages: None,
            },
            &theme,
            &crate::model::selection::SelectionState::None,
        );
        draw_models_modal(
            f,
            crate::overlays::provider::models::ModelsModalProps {
                models: &[],
                current_provider: "mock",
                current_model: "mock-model",
                modal_index: 0,
                query: "",
                cursor_position: 0,
                scroll: &mut 0,
                follow_selection: true,
                search: false,
                show_caret: true,
                refreshing: false,
                spinner_phase: 0,
            },
            &theme,
        );
        let history_roster: Vec<nuo_wire::HistoryEntry> =
            [nuo_wire::HistoryEntry::new(
                "a".to_string(),
                None,
                None,
                0,
            )]
            .into_iter()
            .collect();
        let ranked: Vec<(usize, crate::fuzzy::FuzzyMatch)> = crate::fuzzy::rank(&["a"], "");
        let input_rect = nuotc::Rect::new(0, 20, 80, 3);
        let _ = draw_history_panel(
            f,
            crate::overlays::history::HistoryPanelProps {
                history: &history_roster,
                ranked: &ranked,
                modal_index: 0,
                scroll: &mut 0,
                follow_selection: true,
                input_rect,
                activity_height: 0,
                query: "",
                cursor_position: 0,
                show_caret: false,
            },
            &theme,
        );
        draw_model_editor(
            f,
            "OpenAI",
            "",
            0,
            true,
            0,
            true,
            None,
            &[],
            None,
            None,
            &theme,
        );
        // Preset chooser.
        let mut preset_scroll = 0;
        draw_preset_chooser(0, f, &theme, &mut preset_scroll);
        // Provider editor on the Model text field.
        use crate::providers::CustomField;
        let mut scroll = 0;
        draw_custom_provider_editor(
            CustomEditorProps {
                fields: &[
                    CustomField::Name,
                    CustomField::BaseUrl,
                    CustomField::Token,
                    CustomField::Model,
                    CustomField::Protocol,
                    CustomField::ClientIdentity,
                ],
                field: 3,
                editing: false,
                custom: true,
                title: "Custom OpenAI",
                name_buf: "My Relay",
                base_url_buf: "https://relay/v1/chat/completions",
                token_buf: "tok",
                model_buf: "GPT-4o",
                protocol_display: "Chat Completions",
                identity_display: "nuo (Native)",
                url_hint: "https://relay.example.com/v1/chat/completions",
                input: "gpt",
                cursor_position: 3,
            },
            f,
            &theme,
            &mut scroll,
            true,
        );
        let selection = crate::model::selection::SelectionState::None;
        let mut layout_map = crate::model::layout::LayoutMap::new();
        let sessions_list = [
            nuo_wire::SessionOverview {
                id: "abc123".to_string(),
                overview: "Refactor the renderer".to_string(),
                created_at: 0,
                updated_at: 0,
                message_count: 12,
                active: true,
                parent_id: None,
                fork_kind: nuo_wire::SessionForkKind::Trunk,
                digest: None,
            },
            nuo_wire::SessionOverview {
                id: "def456".to_string(),
                overview: "Fix the tool_call_id bug".to_string(),
                created_at: 0,
                updated_at: 0,
                message_count: 4,
                active: false,
                parent_id: None,
                fork_kind: nuo_wire::SessionForkKind::Trunk,
                digest: None,
            },
        ];
        draw_sessions_modal(
            f,
            crate::overlays::session::SessionsModalProps {
                sessions: &sessions_list,
                expanded_sessions: None,
                selected: 0,
                scroll: &mut scroll,
                follow: false,
                startup_picker: false,
                spinner_phase: 0,
                session_info_detail: false,
                session_detail: None,
                session_info_scroll: &mut 0,
                sessions_loading: false,
            },
            &theme,
            &selection,
            &mut layout_map,
        );
        let mut keys_scroll = 0;
        let ctx = crate::keymap::AppContext {
            active_dialog: Some(crate::surfaces::DialogKind::Sessions),
            ..Default::default()
        };
        draw_dialog_keys(
            f,
            crate::surfaces::DialogKind::Sessions,
            &mut keys_scroll,
            &ctx,
            &theme,
            &selection,
            &mut layout_map,
        );
        let question_request = UserQuestionRequest {
            id: "q1".to_string(),
            questions: vec![nuo_wire::UserQuestion {
                header: Some("Style".to_string()),
                question: "Which error handling crate?".to_string(),
                options: vec![
                    nuo_wire::UserQuestionOption {
                        label: "anyhow (Recommended)".to_string(),
                        description: Some("Simple".to_string()),
                    },
                    nuo_wire::UserQuestionOption {
                        label: "thiserror".to_string(),
                        description: Some("Structured".to_string()),
                    },
                ],
                multi_select: false,
            }],
            origin: None,
        };
        let mut hit_map = crate::ui::ComponentTree::new();
        hit_map.begin(f.area());
        draw_question_modal(
            f,
            &mut hit_map,
            &question_request,
            0,
            &[vec![1]],
            &[String::new()],
            1,
            &mut 0,
            true,
            0,
            nuotc::Rect::new(0, 0, 60, 10),
            true,
            &theme,
        );
    });

    terminal.draw(|f| {
        let request = PermissionRequest {
            id: "p1".to_string(),
            tool: "execute_command".to_string(),
            label: "execute_command".to_string(),
            description: "run a command".to_string(),
            arguments: r#"{"command":"ls"}"#.to_string(),
            scope: "*".to_string(),
            elevation: false,
            one_off: false,
            origin: None,
            ..Default::default()
        };
        let rect = nuotc::Rect::new(0, 0, 60, 3);
        let mut hit_map = crate::ui::ComponentTree::new();
        hit_map.begin(f.area());
        let _ = draw_permission_sheet(
            f,
            &mut hit_map,
            &request,
            0,
            false,
            false,
            0,
            0,
            rect,
            &theme,
            &crate::model::selection::SelectionState::None,
            &mut crate::model::layout::LayoutMap::new(),
        );
    });
}

#[test]
fn config_appearance_pages_render_at_minimum_terminal_size() {
    let theme = Theme::default();
    let custom = nuo_wire::ColorSchemeConfig::default();
    let tui_config = crate::config::TuiConfig::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);

    terminal.draw(|frame| {
        draw_settings_view(
            frame,
            SettingsProps {
                category_index: 0,
                detail_index: 0,
                hover_index: None,
                focus: ConfigFocus::Categories,
                color_scheme: "zen",
                custom_color_scheme: &custom,
                websearch: None,
                workspace: "~/workspace",
                category_scroll: &mut 0,
                detail_scroll: &mut 0,
                breadcrumbs: Some("Main › Settings"),
                theme: &theme,
                profile: &nuotc::TerminalProfile::direct_color(),
                tui_config: &tui_config,
                session_head: Some(test_session_head()),
                unattended: false,
                confined: true,
            },
        );
    });
    // Row 1 is the uniform session identity; the scene name + breadcrumb live on
    // row 2 (ADR-0024).
    assert!(grid_row(&terminal, 0).contains("SESSION"));
    assert!(!grid_row(&terminal, 0).contains("⚙"));
    assert!(!grid_row(&terminal, 0).contains("Appearance"));
    let scene_row = grid_row(&terminal, 1);
    assert!(scene_row.contains("settings"), "scene name on row 2: {scene_row:?}");
    assert!(
        scene_row.contains("Main › Settings"),
        "Row 2 must show the view stack breadcrumbs: {scene_row:?}"
    );
    assert!(!grid_row(&terminal, 3).contains("◐"));

    terminal.draw(|frame| {
        draw_settings_view(
            frame,
            SettingsProps {
                category_index: 0,
                detail_index: 5,
                hover_index: None,
                focus: ConfigFocus::Detail,
                color_scheme: "custom",
                custom_color_scheme: &custom,
                websearch: None,
                workspace: "~/workspace",
                category_scroll: &mut 0,
                detail_scroll: &mut 0,
                breadcrumbs: Some("Main › Settings"),
                theme: &theme,
                profile: &nuotc::TerminalProfile::direct_color(),
                tui_config: &tui_config,
                session_head: Some(test_session_head()),
                unattended: false,
                confined: true,
            },
        );
    });
    assert!(grid_row(&terminal, 0).contains("SESSION"));
}

/// The Settings center has no bottom footer band and no per-pane prose header:
/// the selected category is already named by the highlighted left-nav item, so
/// the right pane is pure content. Each pane is padded by 1 row / 2 columns, and
/// the regions carry two distinct tones — the left nav's `panel` and the right
/// detail body's sunken tone — neither collapsing onto the other.
#[test]
fn settings_view_has_padded_panes_and_distinct_zone_tones_without_a_prose_header() {
    let theme = Theme::default();
    let custom = nuo_wire::ColorSchemeConfig::default();
    let tui_config = crate::config::TuiConfig::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);

    terminal.draw(|frame| {
        draw_settings_view(
            frame,
            SettingsProps {
                category_index: 0,
                detail_index: 0,
                hover_index: None,
                focus: ConfigFocus::Detail,
                color_scheme: "zen",
                custom_color_scheme: &custom,
                websearch: None,
                workspace: "",
                category_scroll: &mut 0,
                detail_scroll: &mut 0,
                breadcrumbs: Some("Main › Settings"),
                theme: &theme,
                profile: &nuotc::TerminalProfile::direct_color(),
                tui_config: &tui_config,
                session_head: Some(test_session_head()),
                unattended: false,
                confined: true,
            },
        );
    });

    let buffer = terminal.buffer();
    let nav_bg = buffer[(2, 12)].bg;
    let detail_bg = buffer[(40, 12)].bg;
    assert_ne!(
        nav_bg, detail_bg,
        "left nav and right detail panes must be colour-differentiated"
    );
    assert_eq!(nav_bg, theme.panel(), "the left nav sits on the panel tone");
    assert_eq!(
        detail_bg,
        theme.pane_sunken(),
        "the right detail body sits on the sunken tone"
    );

    // No prose header: the filler summary that simply restated the nav item is
    // gone, and the detail body now owns the whole pane (top row included).
    let joined: Vec<String> = (0..24).map(|y| grid_row(&terminal, y)).collect();
    let joined = joined.join("\n");
    assert!(
        !joined.contains("Theme selection and color palette customization"),
        "the filler category description must not render: {joined}"
    );
    assert!(
        !joined.contains("Choose how the agent"),
        "no per-category prose header on any pane: {joined}"
    );
    // The pane under the bookend rows is one body surface (no `raised` header
    // strip). Rows carrying the keyboard cursor's hover band are exempt.
    let band = theme.row_hover_band(&[]);
    for y in 2..24u16 {
        let bg = buffer[(40, y)].bg;
        assert_ne!(
            bg,
            theme.raised(),
            "row {y} of the detail pane must not be a `raised` header strip"
        );
        assert!(
            bg == theme.pane_sunken() || bg == band,
            "row {y} must be the sunken body (or its cursor band), got {bg:?}"
        );
    }

    // 1-row / 2-column padding: the pane's own leading columns are a gutter, and
    // the first body row is padding, not content.
    for gutter_x in [22u16, 23] {
        assert_eq!(
            buffer[(gutter_x, 2)].bg,
            theme.pane_sunken(),
            "the pane's leading columns are the 2-column padding gutter"
        );
    }

    // No keycap legend survives anywhere in the view (the footer is gone).
    assert!(!joined.contains("apply/toggle"), "footer removed: {joined}");
    assert!(!joined.contains("back to nav"), "footer removed: {joined}");
}

#[test]
fn settings_scene_renders_without_chevron_indicators_and_with_clean_alignment() {
    let theme = Theme::default();
    let custom = nuo_wire::ColorSchemeConfig::default();
    let web = nuo_wire::WebSearchConfigView {
        revision: 0,
        provider: nuo_wire::WebSearchProvider::Exa,
        reader: nuo_wire::WebReaderProvider::Jina,
        timeout_secs: 20,
        searxng_url: None,
        search_credential: nuo_wire::WebCredentialStatus::Stored,
        reader_credential: nuo_wire::WebCredentialStatus::Stored,
        capabilities: nuo_wire::web_provider_capabilities(),
    };

    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let tui_config = crate::config::TuiConfig::default();

    for cat_idx in 0..ConfigCategory::ALL.len() {
        for focus in [ConfigFocus::Categories, ConfigFocus::Detail] {
            terminal.draw(|frame| {
                draw_settings_view(
                    frame,
                    SettingsProps {
                        category_index: cat_idx,
                        detail_index: 0,
                        hover_index: None,
                        focus,
                        color_scheme: "zen",
                        custom_color_scheme: &custom,
                        websearch: Some(&web),
                        workspace: "",
                        category_scroll: &mut 0,
                        detail_scroll: &mut 0,
                        breadcrumbs: None,
                        theme: &theme,
                        profile: &nuotc::TerminalProfile::direct_color(),
                        tui_config: &tui_config,
                        session_head: Some(test_session_head()),
                        unattended: false,
                        confined: true,
                    },
                );
            });

            // With breadcrumbs: None, no row in the entire settings view should contain `›`
            for y in 0..24 {
                let row = grid_row(&terminal, y);
                assert!(
                    !row.contains('›'),
                    "settings scene must not contain '›' indicator at cat={cat_idx}, focus={focus:?}, row {y}: {row}"
                );
            }
        }
    }
}

#[test]
fn settings_view_adapts_to_terminal_profile_capabilities() {
    let custom = nuo_wire::ColorSchemeConfig::default();
    let tui_config = crate::config::TuiConfig::default();

    // 1. DirectColor profile: Appearance shows chromatic presets
    let direct_profile = nuotc::TerminalProfile::direct_color();
    assert!(direct_profile.supports_color_themes());
    assert_eq!(ConfigCategory::ALL.len(), 5);
    assert_eq!(ConfigCategory::from_index(0), ConfigCategory::Appearance);

    let theme_direct = Theme::default();
    let mut term_direct = nuotc::TestTerminal::new(80, 24);
    term_direct.draw(|frame| {
        draw_settings_view(
            frame,
            SettingsProps {
                category_index: 0,
                detail_index: 0,
                hover_index: None,
                focus: ConfigFocus::Detail,
                color_scheme: "zen",
                custom_color_scheme: &custom,
                websearch: None,
                workspace: "",
                category_scroll: &mut 0,
                detail_scroll: &mut 0,
                breadcrumbs: None,
                theme: &theme_direct,
                profile: &direct_profile,
                tui_config: &tui_config,
                session_head: Some(test_session_head()),
                unattended: false,
                confined: true,
            },
        );
    });

    let direct_screen = (0..24).map(|y| grid_row(&term_direct, y)).collect::<Vec<_>>().join("\n");
    assert!(direct_screen.contains("Appearance"));
    assert!(direct_screen.contains("Zen"));

    // 2. Monochrome profile: Appearance adapts to hardware mode without broken RGB swatches
    let mono_profile = nuotc::TerminalProfile::dec_vt100_monochrome();
    assert!(!mono_profile.supports_color_themes());

    let theme_mono = Theme::monochrome();
    let mut term_mono = nuotc::TestTerminal::new(80, 24);
    let tui_config = crate::config::TuiConfig::default();
    term_mono.draw(|frame| {
        draw_settings_view(
            frame,
            SettingsProps {
                category_index: 0,
                detail_index: 0,
                hover_index: None,
                focus: ConfigFocus::Detail,
                color_scheme: "monochrome",
                custom_color_scheme: &custom,
                websearch: None,
                workspace: "",
                category_scroll: &mut 0,
                detail_scroll: &mut 0,
                breadcrumbs: None,
                theme: &theme_mono,
                profile: &mono_profile,
                tui_config: &tui_config,
                session_head: Some(test_session_head()),
                unattended: false,
                confined: true,
            },
        );
    });

    let mono_screen = (0..24).map(|y| grid_row(&term_mono, y)).collect::<Vec<_>>().join("\n");
    assert!(mono_screen.contains("Appearance"));
    assert!(mono_screen.contains("Monochrome Hardware Mode"));
    assert!(mono_screen.contains("[ Active ]"));
    assert!(!mono_screen.contains("Zen"));
}

/// The Appearance rows carry their state by color, not by a `●`/`○` glyph: the
/// applied scheme highlights its label text, and a hovered (or keyboard-cursor)
/// row is dressed by a full-width background band derived from the palette so
/// its own swatches stay legible against it.
#[test]
fn appearance_rows_use_text_and_palette_band_not_selection_dots() {
    let theme = Theme::default();
    let custom = nuo_wire::ColorSchemeConfig::default();
    let tui_config = crate::config::TuiConfig::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);

    fn draw_appearance(
        terminal: &mut nuotc::TestTerminal,
        hover: Option<usize>,
        theme: &Theme,
        custom: &nuo_wire::ColorSchemeConfig,
        tui_config: &crate::config::TuiConfig,
    ) {
        terminal.draw(|frame| {
            draw_settings_view(
                frame,
                SettingsProps {
                    category_index: 0, // Appearance
                    detail_index: 0,
                    hover_index: hover,
                    focus: ConfigFocus::Detail,
                    color_scheme: "zen",
                    custom_color_scheme: custom,
                    websearch: None,
                    workspace: "",
                    category_scroll: &mut 0,
                    detail_scroll: &mut 0,
                    breadcrumbs: None,
                    theme,
                    profile: &nuotc::TerminalProfile::direct_color(),
                    tui_config,
                    session_head: Some(test_session_head()),
                    unattended: false,
                    confined: true,
                },
            );
        });
    }

    draw_appearance(&mut terminal, None, &theme, &custom, &tui_config);
    let idle: Vec<String> = (0..24).map(|y| grid_row(&terminal, y)).collect();
    let idle_joined = idle.join("\n");
    // Zen is the applied scheme and the cursor row: no selection dot anywhere.
    assert!(
        !idle_joined.contains('●') && !idle_joined.contains('○'),
        "appearance rows must not carry selection dots:\n{idle_joined}"
    );
    // The description sits on its own line beneath the identity row, so it is
    // never wrapped into the swatch row.
    let zen_row = idle
        .iter()
        .position(|row| row.contains("Zen") && row.contains('█'))
        .expect("the Zen identity row must render");
    assert!(
        !idle[zen_row].contains("Quiet charcoal"),
        "the description must not share the identity line: {:?}",
        idle[zen_row]
    );
    assert!(
        idle.iter()
            .skip(zen_row + 1)
            .take(2)
            .any(|row| row.contains("Quiet charcoal")),
        "the description must appear on the following line(s)"
    );

    // The cursor row is banded while the pane is focused: its background is the
    // palette-derived band, not the resting body tone.
    let preview = Theme::from_color_scheme("zen", &custom);
    let band = theme.row_hover_band(&[
        preview.body(),
        preview.panel(),
        preview.brand(),
        preview.info(),
        preview.ok(),
        preview.warn(),
    ]);
    assert_ne!(
        band,
        theme.body(),
        "the hover band must differ from the body"
    );
    let cursor_row_bg = terminal.buffer()[(40, zen_row as u16)].style().bg;
    assert_eq!(
        cursor_row_bg, band,
        "the cursor row must wear the palette-derived band"
    );

    // Hovering a *different* row moves the band there and off the cursor row.
    draw_appearance(&mut terminal, Some(2), &theme, &custom, &tui_config);
    let buffer = terminal.buffer();
    let wide_band_rows = (0..24u16)
        .filter(|&y| buffer[(40, y)].style().bg == band)
        .count();
    assert!(
        wide_band_rows >= 2,
        "the hovered row's identity + description lines must both be banded"
    );
}

/// The Components rows follow the Appearance grammar: state is carried by color
/// (an active row highlights its label text; the hover/cursor band paints the
/// whole row), never by a `●`/`○` glyph, and the description sits on its own
/// line beneath the identity line.
#[test]
fn components_rows_use_text_and_band_not_selection_dots_with_description_below() {
    let theme = Theme::default();
    let custom = nuo_wire::ColorSchemeConfig::default();
    let tui_config = crate::config::TuiConfig::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);

    terminal.draw(|frame| {
        draw_settings_view(
            frame,
            SettingsProps {
                category_index: 1, // Components
                detail_index: 0,
                hover_index: None,
                focus: ConfigFocus::Detail,
                color_scheme: "zen",
                custom_color_scheme: &custom,
                websearch: None,
                workspace: "",
                category_scroll: &mut 0,
                detail_scroll: &mut 0,
                breadcrumbs: None,
                theme: &theme,
                profile: &nuotc::TerminalProfile::direct_color(),
                tui_config: &tui_config,
                session_head: Some(test_session_head()),
                unattended: false,
                confined: true,
            },
        );
    });

    let rows: Vec<String> = (0..24).map(|y| grid_row(&terminal, y)).collect();
    let joined = rows.join("\n");
    assert!(
        !joined.contains('●') && !joined.contains('○'),
        "components rows must not carry selection dots:\n{joined}"
    );
    // The identity line carries the label + badge; the description is on its own
    // line beneath, so it never shares the label line.
    let reasoning_row = rows
        .iter()
        .position(|row| row.contains("Reasoning Traces"))
        .expect("the Reasoning identity row must render");
    assert!(
        rows[reasoning_row].contains("[ Collapsed ]"),
        "the badge stays on the identity line: {:?}",
        rows[reasoning_row]
    );
    assert!(
        !rows[reasoning_row].contains("Expand model reasoning"),
        "the description must not share the identity line: {:?}",
        rows[reasoning_row]
    );
    assert!(
        rows.iter()
            .skip(reasoning_row + 1)
            .take(3)
            .any(|row| row.contains("Expand model reasoning")),
        "the description must appear on the following line(s)"
    );
}

#[test]
fn web_settings_split_search_and_reader_into_clear_panels() {
    let theme = Theme::default();
    let custom = nuo_wire::ColorSchemeConfig::default();
    let tui_config = crate::config::TuiConfig::default();
    let web = nuo_wire::WebSearchConfigView {
        revision: 0,
        provider: nuo_wire::WebSearchProvider::Exa,
        reader: nuo_wire::WebReaderProvider::Jina,
        timeout_secs: 20,
        searxng_url: None,
        search_credential: nuo_wire::WebCredentialStatus::Stored,
        reader_credential: nuo_wire::WebCredentialStatus::Stored,
        capabilities: nuo_wire::web_provider_capabilities(),
    };

    let mut terminal = nuotc::TestTerminal::new(80, 24);
    for (category_index, expected_axis) in [(2, "Used by search_web"), (3, "Used by read_url")] {
        terminal.draw(|frame| {
            draw_settings_view(
                frame,
                SettingsProps {
                    category_index,
                    detail_index: 0,
                    hover_index: None,
                    focus: ConfigFocus::Detail,
                    color_scheme: "zen",
                    custom_color_scheme: &custom,
                    websearch: Some(&web),
                    workspace: "~/workspace",
                    category_scroll: &mut 0,
                    detail_scroll: &mut 0,
                    breadcrumbs: Some("Main › Settings"),
                    theme: &theme,
                    profile: &nuotc::TerminalProfile::direct_color(),
                    tui_config: &tui_config,
                    session_head: Some(test_session_head()),
                    unattended: false,
                    confined: true,
                },
            );
        });
        let screen = (0..24)
            .map(|y| grid_row(&terminal, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Web Search"));
        assert!(screen.contains("Web Reader"));
        assert!(screen.contains(expected_axis));
        assert!(screen.contains("PROVIDER"));
        assert!(screen.contains("NETWORK"));
        assert!(screen.contains("Provider"));
        assert!(screen.contains("Timeout"));
    }
}

#[test]
fn settings_view_reports_selected_row_rect_for_popover_anchoring() {
    let theme = Theme::default();
    let custom = nuo_wire::ColorSchemeConfig::default();
    let tui_config = crate::config::TuiConfig::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);

    let mut selected_rect = None;
    terminal.draw(|frame| {
        let rects = draw_settings_view(
            frame,
            SettingsProps {
                category_index: 0, // Appearance
                detail_index: 1,
                hover_index: None,
                focus: ConfigFocus::Detail,
                color_scheme: "zen",
                custom_color_scheme: &custom,
                websearch: None,
                workspace: "",
                category_scroll: &mut 0,
                detail_scroll: &mut 0,
                breadcrumbs: Some("Main › Settings"),
                theme: &theme,
                profile: &nuotc::TerminalProfile::direct_color(),
                tui_config: &tui_config,
                session_head: Some(test_session_head()),
                unattended: false,
                confined: true,
            },
        );
        selected_rect = rects.selected_row_rect;
    });

    let rect = selected_rect.expect("selected settings row rect should be present");
    assert!(rect.y > 0);
    assert!(rect.width > 20);
    assert_eq!(rect.height, 1);
}

#[test]
fn footer_keeps_one_blank_row_below_transcript_when_active_or_idle() {
    fn assert_gap(activity: &str) {
        let backoff_clause: Option<&str> = None;
        let theme = Theme::default();
        let messages = vec![TranscriptMessage::new(
            nuo_wire::Role::Assistant,
            "A finished response above the footer.",
        )];
        let mut terminal = nuotc::TestTerminal::new(60, 20);
        let mut footer_anchor_y = 0;
        let mut transcript_height = 0;

        terminal.draw(|frame| {
            let mut layout_map = LayoutMap::new();
            let rendered = draw_transcript(
                frame,
                &mut layout_map,
                TranscriptProps {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    backoff_clause,
                    activity,
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        paused: false,
                        blocked: false,
                        expand_key: Some(crate::keymap::Key::CTRL_Q),
                    },
                    tasks_bar: Default::default(),
                    persistence_health: None,
                    subagent_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
            footer_anchor_y = footer_stack::rect_of(&rendered.footer, FooterRowId::Activity)
                .map(|rect| rect.y)
                .unwrap_or(rendered.input_rect.y);
            transcript_height = rendered.view_height;
        });

        // The footer stack attaches directly below the transcript viewport
        // (FOOTER_TOP_GAP_ROWS = 0). The queue bar in this fixture is empty,
        // so the leading footer element is the activity bar when responding
        // or the input box when idle.
        let expected_anchor = 1 + transcript_height + FOOTER_TOP_GAP_ROWS;
        assert_eq!(footer_anchor_y, expected_anchor);
    }

    assert_gap("responding");
    assert_gap("idle");
}

/// When the terminal is resized below the usable minimum,
/// `draw_transcript` must not render the normal UI (which would underflow
/// the footer layout math). Instead it hides everything, shows a centered
/// "terminal too small" notice, and returns a zeroed `TranscriptRender` so
/// the app loop draws no chrome over it.
#[test]
fn too_small_terminal_shows_notice_and_zeroed_render() {
    let theme = Theme::default();
    let messages = vec![TranscriptMessage::new(nuo_wire::Role::User, "hello")];

    let mut terminal = nuotc::TestTerminal::new(20, 8);
    let mut render_opt: Option<TranscriptRender> = None;
    terminal.draw(|f| {
        render_opt = Some(draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        ));
    });

    let render = render_opt.expect("draw_transcript must return a render");
    // The guard suppresses all chrome geometry.
    assert_eq!(render.input_rect, Rect::default());
    assert_eq!(render.hint_rect, Rect::default());
    assert_eq!(render.view_height, 0);
    assert_eq!(render.content_lines, 0);

    // The notice text must be present somewhere in the rendered buffer.
    let buffer = terminal.buffer();
    let rendered: String = (0..buffer.area().height)
        .flat_map(|y| (0..buffer.area().width).map(move |x| buffer[(x, y)].symbol().to_string()))
        .collect::<String>();
    assert!(
        rendered.contains("Terminal too small"),
        "expected the too-small notice in the rendered buffer"
    );
}

/// The loop's frozen screen (`draw_too_small`) paints the notice and the
/// paused-state line, and nothing else — it is the one frame the event loop
/// commits while collapsed below the minimum.
#[test]
fn draw_too_small_paints_the_frozen_notice_only() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(30, 10);
    terminal.draw(|f| draw_too_small(f, &theme));
    let buffer = terminal.buffer();
    let rendered: String = (0..buffer.area().height)
        .flat_map(|y| (0..buffer.area().width).map(move |x| buffer[(x, y)].symbol().to_string()))
        .collect::<String>();
    assert!(
        rendered.contains("Terminal too small"),
        "notice: {rendered}"
    );
    assert!(
        rendered.contains("paused"),
        "frozen state announces that input is blocked: {rendered}"
    );
    // The normal chrome must not be present.
    assert!(
        !rendered.contains("Enter send"),
        "no composer chrome while frozen: {rendered}"
    );
}

/// With no messages, `draw_transcript` renders the empty-state hero in
/// place of the stream: `content_lines` is non-zero (so the app loop does
/// not treat it as a zero-height stream) and the call does not panic.
#[test]
fn empty_session_renders_empty_state_with_nonzero_height() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let messages: Vec<TranscriptMessage> = Vec::new();

    let mut render_opt: Option<TranscriptRender> = None;
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        render_opt = Some(draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "idle",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        ));
    });
    let render = render_opt.expect("draw_transcript must return a render");

    // The empty-state hero replaces the transcript; it occupies the logo
    // rows plus a gap, never zero, so scroll-follow logic stays honest.
    assert!(
        render.content_lines > 0,
        "empty state should report non-zero content_lines"
    );
    assert!(render.sticky.is_none(), "no sticky header on empty state");
    assert!(
        render.view_height > 0,
        "view_height should reflect the viewport, not be zero"
    );
}

/// A non-empty session skips the empty-state branch entirely — the hero
/// never competes with real content.
#[test]
fn nonempty_session_does_not_render_empty_state() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let messages = vec![TranscriptMessage::new(nuo_wire::Role::User, "hello")];

    let mut render_opt: Option<TranscriptRender> = None;
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        render_opt = Some(draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "idle",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        ));
    });
    let render = render_opt.expect("draw_transcript must return a render");

    // With a real message the stream is rendered normally — content_lines
    // reflects at least one rendered message rather than the fixed
    // empty-state height.
    assert!(
        render.content_lines > 0,
        "non-empty session should render its messages"
    );
}

/// A user-supplied logo (from `logo.txt`) replaces the built-in wordmark
/// on the empty state, and `content_lines` tracks its (clamped) height so
/// scroll accounting stays honest. A three-line user logo yields five
/// reported lines (3 + blank gap + carousel page), distinct from the
/// built-in wordmark's height.
#[test]
fn empty_session_uses_user_logo_and_reports_its_height() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let messages: Vec<TranscriptMessage> = Vec::new();
    // Three 9-column lines → reported content is 3 + 1 (gap) + 1 (carousel) = 5.
    let logo: Vec<String> = vec![
        "NN     NN".to_string(),
        "NNN   NNN".to_string(),
        "NN     NN".to_string(),
    ];

    let mut render_opt: Option<TranscriptRender> = None;
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        render_opt = Some(draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "idle",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: Some(&logo),
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        ));
    });
    let render = render_opt.expect("draw_transcript must return a render");

    // 3 logo lines + 1 blank gap + 1 carousel page = 5.
    assert_eq!(
        render.content_lines, 5,
        "user-logo content_lines must be logo rows + gap + guidance rows"
    );
}

/// ADR-0023/0024: the head band's scene row stands up on **every** scene. On
/// the main view it names the scene (`thread`) and carries the chat title
/// as its context, with the `C-x menu` namespace pair on the right.
#[test]
fn main_view_shows_the_thread_scene_row() {
    let terminal = render_full_view(
        80,
        24,
        &[],
        Some(ViewHints {
            kind: crate::surfaces::SceneKind::Thread,
            context: Some("Fix the retry loop"),
            context_warn: false,
            unattended: false,
            confined: true,
            workspace: Some("~/workspace"),
            breadcrumbs: None,
            can_back: false,
            can_forward: false,
        }),
    );
    let row0 = grid_row(&terminal, 0);
    assert!(row0.contains("SESSION"));
    assert!(
        row0.contains("menu C-x"),
        "client top bar offers the C-x menu namespace: {row0:?}"
    );
    let row1 = grid_row(&terminal, 1);
    assert!(row1.starts_with("  thread"), "scene name: {row1:?}");
    assert!(
        row1.contains("Fix the retry loop"),
        "chat title context: {row1:?}"
    );
    assert!(
        row1.contains("~/workspace"),
        "workspace belongs to thread scene row: {row1:?}"
    );
    assert!(
        !row1.contains("menu C-x") && !row1.contains("C-x"),
        "scene row no longer carries C-x: {row1:?}"
    );
}

/// ADR-0024: the run-mode flags ride the scene row's right edge, so a
/// thread read-out states the session's persistent posture alongside the
/// scene name and title.
#[test]
fn main_view_scene_row_carries_run_mode_flags() {
    let terminal = render_full_view(
        80,
        24,
        &[],
        Some(ViewHints {
            kind: crate::surfaces::SceneKind::Thread,
            context: None,
            context_warn: false,
            unattended: true,
            confined: false,
            workspace: None,
            breadcrumbs: None,
            can_back: false,
            can_forward: false,
        }),
    );
    let row0 = grid_row(&terminal, 0);
    assert!(row0.contains("menu C-x"), "client menu on row 0: {row0:?}");
    let row1 = grid_row(&terminal, 1);
    assert!(row1.contains("UNATTENDED"), "unattended flag: {row1:?}");
    assert!(row1.contains("UNCONFINED"), "unconfined flag: {row1:?}");
    assert!(!row1.contains("menu C-x") && !row1.contains("C-x"), "namespace on row 0 not row 1: {row1:?}");
    assert!(!row1.contains("Esc"), "no interrupt pair: {row1:?}");
    assert!(!row1.contains("F1"), "no global help pair: {row1:?}");
}

/// The empty-state tour renders the current carousel page beneath the
/// logo (ADR-0104) — no static tagline, no dot indicator.
#[test]
fn empty_state_tour_renders_the_current_carousel_page() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let messages: Vec<TranscriptMessage> = Vec::new();
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 2,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    let buffer = terminal.buffer();
    let width = buffer.area().width as usize;
    let all: Vec<String> = (0..buffer.area().height)
        .map(|y| (0..width).map(|x| buffer[(x as u16, y)].symbol()).collect())
        .collect();
    let joined = all.join("\n");
    // The static tagline is retired (ADR-0104): the carousel's first
    // page already answers "how do I start", so no duplicate line.
    assert!(
        !joined.contains("Type a message below to begin."),
        "no static tagline: {joined}"
    );
    // Page 2 of the tour is the /btw page.
    assert!(joined.contains("/btw"), "page 2 visible: {joined}");
    // No dot indicator row (ADR-0104): the carousel is a single line and
    // the rotation is self-explaining.
    assert!(!joined.contains('●'), "no dot indicator anywhere: {joined}");
}

/// An H1 heading renders with an UNDERLINED modifier. The underline must
/// cover only the prefix + text cells and must not bleed into the trailing
/// whitespace of the heading row. Inspects the rendered grid cells
/// directly to pin the clamp in `draw_message_body`.
#[test]
fn h1_underline_clamps_to_text_extent() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(60, 12);
    let messages = vec![TranscriptMessage::new(
        nuo_wire::Role::Assistant,
        "# QQ_H1_TEST\n\nbody text here\n",
    )];
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    let buffer = terminal.buffer();
    let width = buffer.area().width;
    let underline = nuotc::Modifier::UNDERLINE;

    let mut head = None;
    'outer: for y in 0..buffer.area().height {
        for x in 0..width {
            if buffer[(x, y)].symbol() == "Q" {
                head = Some((x, y));
                break 'outer;
            }
        }
    }
    let (hx, hy) = head.expect("heading 'Q' cell exists");

    // "QQ_H1_TEST" is 10 cells; prefix is 3 cells. All 13 are underlined.
    for x in hx..hx + 10 {
        assert!(
            buffer[(x, hy)].style.add.contains(underline),
            "heading text cell at x={x} must be UNDERLINED"
        );
    }
    let trailing = hx + 10;
    assert!(trailing < width, "trailing cell within grid");
    assert!(
        !buffer[(trailing, hy)].style.add.contains(underline),
        "underline must not bleed into trailing whitespace at x={trailing}"
    );
    assert!(
        !buffer[(width - 1, hy)].style.add.contains(underline),
        "underline must not reach the right edge"
    );
}

/// A wide (emoji) glyph in an H1 heading occupies a head cell plus a
/// wide-continuation cell. The grid stores the continuation without the
/// `add` modifiers (it is a non-emitted placeholder), but the diff skips
/// continuations and emits the head's run style — so the backend prints
/// the wide glyph while the UNDERLINED SGR is active, underlining both
/// columns. This pins that emitted behavior at the `Draw`-command layer.
#[test]
fn h1_underline_emits_wide_glyph_in_underlined_run() {
    let theme = Theme::default();
    let width = 60u16;
    let mut terminal = nuotc::TestTerminal::new(width, 12);
    let messages = vec![TranscriptMessage::new(
        nuo_wire::Role::Assistant,
        "# Hello😀\n\nbody\n",
    )];
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    let back = terminal.buffer();
    let front = nuotc::Grid::new(width, 12);
    let cmd = nuotc::diff::diff(back, &front);
    let underline = nuotc::Modifier::UNDERLINE;

    let wide_run_style = cmd.draws.iter().find_map(|d| match d {
        nuotc::Draw::Cells { style, cells, .. } => cells
            .iter()
            .any(|(sym, w)| sym == "😀" && *w == 2)
            .then_some(*style),
        _ => None,
    });
    let style =
        wide_run_style.expect("a Draw::Cells run containing wide glyph '😀' must be emitted");
    assert!(
        style.add.contains(underline),
        "wide glyph '😀' must be emitted in an UNDERLINED run so the terminal \
             underlines both columns, got add={:?}",
        style.add,
    );
}

/// Regression: a long H1 heading that wraps to multiple lines. The heading
/// *prefix* (the leading indent on row 0 and the continuation indent
/// on rows 1+) is decoration, not heading text, so it must NOT carry the
/// UNDERLINED modifier. Previously the prefix shared the UNDERLINED style,
/// which underlined the leading whitespace of every wrapped row — the
/// underline appeared to "cross the line head" and cover the blank indent.
///
/// We render a heading that wraps to ≥2 rows and assert that, on every
/// row, the underline begins exactly at the text column (prefix width) and
/// that the indent columns themselves are never underlined. The trailing
/// blank columns must also stay un-underlined (the existing clamp).
#[test]
fn h1_underline_excludes_prefix_indent_on_wrapped_rows() {
    let theme = Theme::default();
    // Use a terminal at/above the render minimum so `draw_transcript` does
    // not trip its too-small guard. A 76-column transcript band still
    // wraps this ~95-char heading to ≥2 rows.
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let messages = vec![TranscriptMessage::new(
        nuo_wire::Role::Assistant,
        "# This is a very long heading that intentionally wraps to multiple rows for the underline-prefix test\n\nbody\n",
    )];
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    let buffer = terminal.buffer();
    let width = buffer.area().width;
    let underline = nuotc::Modifier::UNDERLINE;

    // The heading prefix is "   " (3 columns); locate the heading's rows
    // as the contiguous non-blank rows at the top (before the blank gap +
    // body). The heading "This is a very long heading that wraps to
    // multiple lines" wraps to several rows here.
    let mut heading_rows: Vec<u16> = Vec::new();
    let mut found_body = false;
    for y in 0..buffer.area().height {
        let row_has_text = (0..width).any(|x| buffer[(x, y)].symbol() != " ");
        if !row_has_text {
            if !heading_rows.is_empty() {
                found_body = true;
            }
            continue;
        }
        if found_body {
            break;
        }
        heading_rows.push(y);
    }
    assert!(
        heading_rows.len() >= 2,
        "heading must wrap to at least 2 rows, got {}",
        heading_rows.len()
    );

    for &y in &heading_rows {
        // Indent columns [0, text_start) must never be underlined.
        // The heading prefix is `TRANSCRIPT_BODY_LEADING_INDENT` cols
        // (matching body prose — see the `Block::Heading` arm), applied
        // inside the already-inset band: entry inset (TRANSCRIPT_H_INSET)
        // + heading prefix (TRANSCRIPT_BODY_LEADING_INDENT). Text starts
        // at col `TRANSCRIPT_H_INSET + TRANSCRIPT_BODY_LEADING_INDENT`.
        let text_start = super::TRANSCRIPT_H_INSET + super::TRANSCRIPT_BODY_LEADING_INDENT;
        for x in 0..text_start {
            let cell = &buffer[(x, y)];
            assert!(
                !cell.style.add.contains(underline),
                "indent cell at (x={x}, y={y}) must NOT be underlined \
                     (it is heading decoration, not text), symbol={:?}",
                cell.symbol(),
            );
        }
        // The trailing blank tail (rightmost column) must not be underlined.
        let last = width - 1;
        assert!(
            !buffer[(last, y)].style.add.contains(underline),
            "trailing cell at (x={last}, y={y}) must NOT be underlined"
        );
        // And at least the first text column must be underlined (the
        // heading text itself is still underlined).
        let first_text_cell = &buffer[(text_start, y)];
        assert!(
            first_text_cell.style.add.contains(underline),
            "first heading-text cell at (x={text_start}, y={y}) must be UNDERLINED, \
                 symbol={:?}",
            first_text_cell.symbol(),
        );
    }
}

/// The durability-health banner (ADR-0196 D4) renders its state label, the
/// cause detail, and truncates under width pressure; a healthy writer never
/// places the row at all, so no code path draws a healthy banner.
#[test]
fn persistence_health_banner_renders_state_and_detail() {
    use crate::render::draw_persistence_health_bar;
    let theme = Theme::default();

    let render = |health: &nuo_wire::monitor::PersistenceHealth, width: u16| {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|f| {
            draw_persistence_health_bar(f, f.area(), health, &theme);
        });
        let buffer = terminal.buffer();
        (0..buffer.area().width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>()
    };

    let recovering = nuo_wire::monitor::PersistenceHealth::Recovering {
        attempt: 1,
        since_ms: 0,
        error: "engine open failed: database is locked".into(),
    };
    let recovering_row = render(&recovering, 80);
    assert!(
        recovering_row.contains("RECOVERING"),
        "banner carries the recovering label: {recovering_row:?}"
    );
    assert!(
        recovering_row.contains("database is locked"),
        "banner carries the cause: {recovering_row:?}"
    );

    let down = nuo_wire::monitor::PersistenceHealth::Down {
        attempt: 7,
        since_ms: 0,
        error: "persistence writer stopped".into(),
    };
    let down_row = render(&down, 80);
    assert!(
        down_row.contains("STORAGE DOWN") && down_row.contains("persistence writer stopped"),
        "down banner carries label + cause: {down_row:?}"
    );

    // Width pressure truncates the detail, never panics.
    let squeezed = render(&recovering, 24);
    assert!(
        squeezed.contains("RECOVERING") && squeezed.chars().count() <= 24,
        "narrow banner truncates cleanly: {squeezed:?}"
    );
}
