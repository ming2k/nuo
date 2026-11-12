use super::*;

#[test]
fn connections_modal_empty_state_centered_copy_and_footer() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(72, 24);
    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
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
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
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
    let detail = nuo_protocol::ConnectionDetail {
        name: "DeepSeek Production".to_string(),
        provider: "deepseek".to_string(),
        provider_label: "DeepSeek".to_string(),
        protocol: "openai".to_string(),
        base_url: "https://api.deepseek.com".to_string(),
        auth_type: "API Key".to_string(),
        api_key_masked: Some("sk-12...abcd".to_string()),
        api_key_source: "credentials.toml".to_string(),
        headers: Default::default(),
        user_agent: "nuo/0.37.21".to_string(),
        models: vec!["deepseek-chat".to_string(), "deepseek-reasoner".to_string()],
        model_info: Vec::new(),
        active_model: Some("deepseek-chat".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_protocol::ConnectionUsageState::Available(Box::new(
            nuo_protocol::ProviderUsage {
                plan: Some("Pay-as-you-go".to_string()),
                description: None,
                quota: Some(nuo_protocol::ProviderQuotaData::Balance(
                    nuo_protocol::BalanceQuota {
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
                metrics: vec![nuo_protocol::UsageMetric {
                    label: "Total Balance".to_string(),
                    value: "100.50".to_string(),
                    unit: Some("CNY".to_string()),
                }],
                model_rules: Vec::new(),
                updated_at_ms: None,
            },
        )),
        budget: Default::default(),
    };

    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("Connections"));
    assert!(text.contains("Connections › DeepSeek Production"));
    assert!(text.contains("Configuration"));
    assert!(text.contains("deepseek"));
    assert!(text.contains("https://api.deepseek.com"));
    assert!(text.contains("sk-12...abcd"));
    assert!(text.contains("Transport"));
    assert!(text.contains("Served Models (2)"));
    assert!(text.contains("2 discovered"));
    assert!(text.contains("Browse & filter in /models"));

    // Scroll to view usage section
    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 8;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
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
    let detail = nuo_protocol::ConnectionDetail {
        name: "Google Antigravity".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth".to_string(),
        api_key_masked: None,
        api_key_source: "OAuth".to_string(),
        headers: Default::default(),
        user_agent: "nuo/0.37.25".to_string(),
        models: vec!["gemini-3.7-flash".to_string(), "gemini-3.1-pro".to_string()],
        model_info: Vec::new(),
        active_model: Some("gemini-3.7-flash".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_protocol::ConnectionUsageState::Available(Box::new(
            nuo_protocol::ProviderUsage {
                plan: Some("Google One AI Premium".to_string()),
                description: None,
                quota: Some(nuo_protocol::ProviderQuotaData::Periodic(
                    nuo_protocol::PeriodicQuota {
                        buckets: vec![
                            nuo_protocol::QuotaWindowBucket {
                                window: Some(nuo_protocol::QuotaWindowKind::Daily),
                                label: "Gemini 3.7 Flash".to_string(),
                                group: None,
                                used_fraction: 0.15,
                                used_amount: None,
                                total_limit: None,
                                unit: Some("DAY".to_string()),
                                reset_at_ms: None,
                                reset_time_str: Some("12:00".to_string()),
                            },
                            nuo_protocol::QuotaWindowBucket {
                                window: Some(nuo_protocol::QuotaWindowKind::Rolling5Hour),
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
                model_rules: Vec::new(),
                updated_at_ms: None,
            },
        )),
        budget: Default::default(),
    };

    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 8;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("Provider Usage & Quota"));
    assert!(text.contains("Google One AI Premium"));
    assert!(text.contains("Gemini 3.7 Flash | Daily"));
    assert!(text.contains("15% used"));
    assert!(text.contains("Gemini 3.1 Pro | 5h Window"));
    assert!(text.contains("40% used"));
}

#[test]
fn loading_dots_pure_ascii_and_fixed_display_width() {
    use crate::shell::overlays::provider::connections::format_loading_dots;

    // Verify all 3 cyclical phases
    assert_eq!(format_loading_dots(0), "Loading.  ");
    assert_eq!(format_loading_dots(1), "Loading.. ");
    assert_eq!(format_loading_dots(2), "Loading...");
    assert_eq!(format_loading_dots(3), "Loading.  ");
    assert_eq!(format_loading_dots(4), "Loading.. ");
    assert_eq!(format_loading_dots(5), "Loading...");

    // Extreme phases (overflow / large tick numbers)
    for phase in [0, 1, 2, 99, 100, usize::MAX - 1, usize::MAX] {
        let text = format_loading_dots(phase);
        assert!(text.is_ascii(), "must be strictly ASCII: {text:?}");
        assert_eq!(text.len(), 10, "byte length must be 10");
        assert_eq!(text.chars().count(), 10, "char count must be 10");
        assert!(text.starts_with("Loading."));
    }
}

#[test]
fn connections_modal_detail_view_renders_initial_loading_state_when_detail_is_none() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 30);

    for (phase, expected_trimmed, forbidden_next) in [
        (0, "Loading.", Some("Loading..")),
        (1, "Loading..", Some("Loading...")),
        (2, "Loading...", None),
    ] {
        terminal.draw(|f| {
            let mut lm = nuo_view::model::layout::LayoutMap::new();
            let mut scroll = 0;
            let selection = nuo_view::model::selection::SelectionState::None;
            draw_connections_modal(
                f,
                &mut lm,
                crate::shell::overlays::provider::connections::ConnectionsModalProps {
                    providers: &[],
                    modal_index: 0,
                    query: "",
                    cursor_position: 0,
                    scroll: &mut scroll,
                    follow_selection: false,
                    search: false,
                    connection_info_detail: true,
                    show_caret: true,
                    connection_detail: None,
                    connection_info_scroll: &mut 0,
                    spinner_phase: phase,
                    connection_info_standalone: false,
                    refreshing: false,
                    connection_usages: None,
                },
                &theme,
                &selection,
            );
        });

        let text = buffer_text(&terminal);
        assert!(
            text.contains(expected_trimmed),
            "text must contain {expected_trimmed:?} at phase {phase}"
        );
        if let Some(forbidden) = forbidden_next {
            assert!(
                !text.contains(forbidden),
                "text must not yet contain {forbidden:?} at phase {phase}"
            );
        }
        assert!(!text.contains("Loading connection details and provider usage…"));
        assert!(!text.contains('⠸'));
    }
}

#[test]
fn fetching_dots_pure_ascii_and_fixed_display_width() {
    use crate::shell::overlays::provider::connections::format_fetching_dots;

    // Verify all 3 cyclical phases
    assert_eq!(format_fetching_dots(0), "Querying.  ");
    assert_eq!(format_fetching_dots(1), "Querying.. ");
    assert_eq!(format_fetching_dots(2), "Querying...");
    assert_eq!(format_fetching_dots(3), "Querying.  ");
    assert_eq!(format_fetching_dots(4), "Querying.. ");
    assert_eq!(format_fetching_dots(5), "Querying...");

    // Extreme phases (overflow / large tick numbers)
    for phase in [0, 1, 2, 99, 100, usize::MAX - 1, usize::MAX] {
        let text = format_fetching_dots(phase);
        assert!(text.is_ascii(), "must be strictly ASCII: {text:?}");
        assert_eq!(text.len(), 11, "byte length must be 11");
        assert_eq!(text.chars().count(), 11, "char count must be 11");
        assert!(text.starts_with("Querying."));
    }
}

#[test]
fn connections_modal_detail_view_renders_fetching_state() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 30);
    let detail = nuo_protocol::ConnectionDetail {
        name: "DeepSeek Production".to_string(),
        provider: "deepseek".to_string(),
        provider_label: "DeepSeek".to_string(),
        protocol: "openai".to_string(),
        base_url: "https://api.deepseek.com".to_string(),
        auth_type: "API Key".to_string(),
        api_key_masked: Some("sk-12...abcd".to_string()),
        api_key_source: "credentials.toml".to_string(),
        headers: Default::default(),
        user_agent: "nuo/0.37.21".to_string(),
        models: vec!["deepseek-chat".to_string()],
        model_info: Vec::new(),
        active_model: Some("deepseek-chat".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_protocol::ConnectionUsageState::Fetching,
        budget: Default::default(),
    };

    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("Configuration"));
    assert!(text.contains("Served Models"));

    // Scroll to view usage section across all 3 phases
    // Note: buffer_text trims line-trailing whitespace, so we verify exact dot counts.
    for (phase, expected_trimmed, forbidden_next) in [
        (0, "Querying.", Some("Querying..")),
        (1, "Querying..", Some("Querying...")),
        (2, "Querying...", None),
    ] {
        terminal.draw(|f| {
            let mut lm = nuo_view::model::layout::LayoutMap::new();
            let mut scroll = 6;
            let selection = nuo_view::model::selection::SelectionState::None;
            draw_connections_modal(
                f,
                &mut lm,
                crate::shell::overlays::provider::connections::ConnectionsModalProps {
                    providers: &[],
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
                    spinner_phase: phase,
                    connection_info_standalone: false,
                    refreshing: false,
                    connection_usages: None,
                },
                &theme,
                &selection,
            );
        });

        let scrolled_text = buffer_text(&terminal);
        assert!(scrolled_text.contains("Provider Usage & Quota"));
        assert!(
            scrolled_text.contains(expected_trimmed),
            "scrolled text must contain {expected_trimmed:?} at phase {phase}"
        );
        if let Some(forbidden) = forbidden_next {
            assert!(
                !scrolled_text.contains(forbidden),
                "scrolled text must not yet contain {forbidden:?} at phase {phase}"
            );
        }
        assert!(!scrolled_text.contains("Querying upstream provider quota & balance…"));
        assert!(!scrolled_text.contains('⠸'));
    }
}

#[test]
fn connections_modal_detail_view_renders_grouped_periodic_quota_and_effort() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 35);
    let detail = nuo_protocol::ConnectionDetail {
        name: "Google Antigravity".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth".to_string(),
        api_key_masked: None,
        api_key_source: "OAuth".to_string(),
        headers: Default::default(),
        user_agent: "nuo/0.37.25".to_string(),
        models: vec![
            "gemini-3.7-flash".to_string(),
            "gemini-3.1-pro".to_string(),
            "claude-3-7-sonnet".to_string(),
        ],
        model_info: vec![
            nuo_protocol::ProviderModelInfo {
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
            nuo_protocol::ProviderModelInfo {
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
            nuo_protocol::ProviderModelInfo {
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
        usage: nuo_protocol::ConnectionUsageState::Available(Box::new(
            nuo_protocol::ProviderUsage {
                plan: Some("Antigravity Quota".to_string()),
                description: Some(
                    "Within each group, models share a weekly limit and a 5-hour limit."
                        .to_string(),
                ),
                quota: Some(nuo_protocol::ProviderQuotaData::Periodic(
                    nuo_protocol::PeriodicQuota {
                        buckets: vec![
                            nuo_protocol::QuotaWindowBucket {
                                window: Some(nuo_protocol::QuotaWindowKind::Weekly),
                                label: "Weekly Limit Remaining".to_string(),
                                group: Some("Claude Models".to_string()),
                                used_fraction: 0.99,
                                used_amount: None,
                                total_limit: None,
                                unit: Some("WEEKLY".to_string()),
                                reset_at_ms: None,
                                reset_time_str: Some("80h 6m".to_string()),
                            },
                            nuo_protocol::QuotaWindowBucket {
                                window: Some(nuo_protocol::QuotaWindowKind::Rolling5Hour),
                                label: "Five Hour Limit Remaining".to_string(),
                                group: Some("Claude Models".to_string()),
                                used_fraction: 0.46,
                                used_amount: None,
                                total_limit: None,
                                unit: Some("5h".to_string()),
                                reset_at_ms: None,
                                reset_time_str: Some("3h 16m".to_string()),
                            },
                            nuo_protocol::QuotaWindowBucket {
                                window: Some(nuo_protocol::QuotaWindowKind::Weekly),
                                label: "Weekly Limit Remaining".to_string(),
                                group: Some("Chat Models (Gemini)".to_string()),
                                used_fraction: 0.0,
                                used_amount: None,
                                total_limit: None,
                                unit: Some("WEEKLY".to_string()),
                                reset_at_ms: None,
                                reset_time_str: Some("167h 59m".to_string()),
                            },
                            nuo_protocol::QuotaWindowBucket {
                                window: Some(nuo_protocol::QuotaWindowKind::Rolling5Hour),
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
                model_rules: Vec::new(),
                updated_at_ms: None,
            },
        )),
        budget: Default::default(),
    };

    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("Connections › Google Antigravity"));
    assert!(text.contains("Default Active"));
    assert!(text.contains("gemini-3.7-flash  |  reasoning: high"));
    assert!(text.contains("Served Models (3)"));
    assert!(text.contains("3 discovered"));
    assert!(text.contains("Browse & filter in /models"));

    // Scroll to view grouped quota
    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let mut info_scroll = 16;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
            },
            &theme,
            &selection,
        );
    });

    let scrolled_text = buffer_text(&terminal);
    assert!(scrolled_text.contains("Provider Usage & Quota"));
    assert!(!scrolled_text.contains("[ Antigravity Quota ]"));
    // ADR-0111: No obscure symbols like `▸`, clean group header
    assert!(scrolled_text.contains("Claude Models"));
    assert!(!scrolled_text.contains('▸'));
    // 5-Hour limit displayed before Weekly limit
    let pos_5h = scrolled_text
        .find("5-Hour Limit")
        .expect("5-Hour Limit should exist");
    let pos_weekly = scrolled_text
        .find("Weekly Limit")
        .expect("Weekly Limit should exist");
    assert!(
        pos_5h < pos_weekly,
        "5-Hour limit must be displayed before Weekly limit"
    );
    // Clean label merged with reset countdown on the same line, no redundant tautology
    assert!(scrolled_text.contains("5-Hour Limit  |  resets in 3h 16m"));
    assert!(scrolled_text.contains("46% used"));
    assert!(scrolled_text.contains("Weekly Limit  |  resets in 80h 6m"));
    assert!(scrolled_text.contains("99% used"));
    assert!(!scrolled_text.contains("Five Hour Limit Remaining | 5h Window"));
    assert!(!scrolled_text.contains("Weekly Limit Remaining | Weekly"));
    assert!(scrolled_text.contains("Chat Models (Gemini)"));
    assert!(scrolled_text.contains("0% used"));
    assert!(scrolled_text.contains("Within each group, models share a weekly limit"));
}

#[test]
fn connections_modal_standalone_detail_renders_single_level_header() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let detail = nuo_protocol::ConnectionDetail {
        name: "Anthropic".to_string(),
        provider: "anthropic".to_string(),
        provider_label: "Anthropic".to_string(),
        protocol: "anthropic".to_string(),
        base_url: "https://api.anthropic.com".to_string(),
        auth_type: "API Key".to_string(),
        api_key_masked: Some("sk-ant-...1234".to_string()),
        api_key_source: "credentials.toml".to_string(),
        headers: Default::default(),
        user_agent: "nuo/0.37.21".to_string(),
        models: vec!["claude-3-7-sonnet".to_string()],
        model_info: Vec::new(),
        active_model: Some("claude-3-7-sonnet".to_string()),
        active_model_effort: Some("high".to_string()),
        active_model_thinking: Some(true),
        usage: nuo_protocol::ConnectionUsageState::Fetching,
        budget: Default::default(),
    };

    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    // Header should be clean single level "Connection Details", without "[Anthropic]" or breadcrumbs
    assert!(text.contains("Connection Details"));
    assert!(!text.contains("[Anthropic]"));
    assert!(!text.contains("Connections ›"));
    // Footer hint should say "close" instead of "list"
    assert!(text.contains("close"));
    assert!(!text.contains("list"));
}

#[test]
fn connections_modal_commandcode_details_has_clean_headers_and_left_aligned_usage() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 30);
    let detail = nuo_protocol::ConnectionDetail {
        name: "Command Code".to_string(),
        provider: "commandcode-goat".to_string(),
        provider_label: "CommandCode GOAT".to_string(),
        protocol: "openai".to_string(),
        base_url: "https://api.commandcode.ai/provider/v1".to_string(),
        auth_type: "API Key".to_string(),
        api_key_masked: Some("cmd-...9999".to_string()),
        api_key_source: "credentials.toml".to_string(),
        headers: Default::default(),
        user_agent: "nuo/0.38.0".to_string(),
        models: vec!["claude-3-7-sonnet".to_string()],
        model_info: Vec::new(),
        active_model: Some("claude-3-7-sonnet".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_protocol::ConnectionUsageState::Available(Box::new(
            nuo_protocol::ProviderUsage {
                plan: Some("Command Code".to_string()),
                description: Some("Command Code API credits".to_string()),
                quota: Some(nuo_protocol::ProviderQuotaData::Balance(
                    nuo_protocol::BalanceQuota {
                        currency: "USD".to_string(),
                        total_balance: Some(42.50),
                        cash_balance: Some(30.00),
                        voucher_balance: Some(12.50),
                        display_primary: None,
                        consumed_amount: None,
                        credit_limit: None,
                    },
                )),
                primary_balance: Some("42.50 credits".to_string()),
                metrics: Vec::new(),
                model_rules: Vec::new(),
                updated_at_ms: None,
            },
        )),
        budget: Default::default(),
    };

    // 1. Standalone header check
    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let mut info_scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_info_standalone: true,
                refreshing: false,
                connection_usages: None,
            },
            &theme,
            &selection,
        );
    });

    let standalone_text = buffer_text(&terminal);
    assert!(standalone_text.contains("Connection Details"));
    assert!(!standalone_text.contains("[Command Code]"));
    assert!(!standalone_text.contains("[ Command Code ]"));

    // 2. Child mode header check
    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let mut info_scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
            },
            &theme,
            &selection,
        );
    });

    let child_text = buffer_text(&terminal);
    assert!(child_text.contains("Connections › Command Code"));
    assert!(!child_text.contains("Details ["));

    // 3. Provider Usage & Quota section check
    let rows =
        crate::shell::overlays::provider::connections::connection_detail_body(&detail, 0, &theme);
    let rendered = rows
        .into_iter()
        .map(|r| {
            let mut s = String::new();
            if let Some(p) = r.prefix {
                s.push_str(&p.text);
            }
            for seg in r.segments {
                s.push_str(&seg.text);
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Title must be clean: "Provider Usage & Quota", NEVER "Provider Usage & Quota  [ Command Code ]"
    assert!(rendered.contains("Provider Usage & Quota"));
    assert!(!rendered.contains("[ Command Code ]"));
    assert!(!rendered.contains("[Command Code]"));

    // Total Balance row must start flush left (no "  Total Balance")
    assert!(
        rendered.lines().any(|l| l.starts_with("Total Balance")),
        "Total Balance must be left-aligned at column 0: {rendered}"
    );
}

#[test]
fn connections_modal_detail_wraps_second_column_with_indent() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(76, 30);
    let detail = nuo_protocol::ConnectionDetail {
        name: "ggl-fox".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "google-antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://daily-cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth (AntigravityOAuth)".to_string(),
        api_key_masked: Some("ya29...0213".to_string()),
        api_key_source: "OAuth".to_string(),
        headers: Default::default(),
        user_agent: "antigravity/1.23.2".to_string(),
        models: vec![
            "claude-opus-4-6".to_string(),
            "claude-sonnet-4-6".to_string(),
        ],
        model_info: Vec::new(),
        active_model: Some("claude-opus-4-6".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_protocol::ConnectionUsageState::Unsupported,
        budget: Default::default(),
    };

    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
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
    assert!(text.contains("Served Models (2)"));
    assert!(text.contains("2 discovered"));
    assert!(text.contains("m browse models"));
}

#[test]
fn connections_modal_detail_served_models_collapsed_by_default_and_expands() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 50);
    let models: Vec<String> = (1..=17).map(|i| format!("model-{i:02}")).collect();
    let detail = nuo_protocol::ConnectionDetail {
        name: "google-antigravity".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://daily-cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth".to_string(),
        api_key_masked: None,
        api_key_source: "OAuth".to_string(),
        headers: Default::default(),
        user_agent: "antigravity/1.0.0".to_string(),
        models: models.clone(),
        model_info: Vec::new(),
        active_model: Some("model-05".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_protocol::ConnectionUsageState::Unsupported,
        budget: Default::default(),
    };

    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("Served Models (17)"));
    assert!(text.contains("17 discovered  |  default: model-05"));
    assert!(text.contains("Browse & filter in /models"));
    assert!(text.contains("browse models"));
    // Configuration has Provider and no redundant Provider ID
    assert!(text.contains("Provider          Google Antigravity"));
    assert!(!text.contains("Provider ID"));
}

#[test]
fn connections_modal_quota_bucket_used_percentage_and_separate_reset_line() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 40);
    let detail = nuo_protocol::ConnectionDetail {
        name: "google-antigravity".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://daily-cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth".to_string(),
        api_key_masked: None,
        api_key_source: "OAuth".to_string(),
        headers: Default::default(),
        user_agent: "antigravity/1.0.0".to_string(),
        models: vec!["gemini-2.5-pro".to_string()],
        model_info: Vec::new(),
        active_model: Some("gemini-2.5-pro".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_protocol::ConnectionUsageState::Available(Box::new(
            nuo_protocol::ProviderUsage {
                plan: Some("Google One AI Premium".to_string()),
                description: None,
                quota: Some(nuo_protocol::ProviderQuotaData::Periodic(
                    nuo_protocol::PeriodicQuota {
                        buckets: vec![nuo_protocol::QuotaWindowBucket {
                            label: "Gemini Pro Agent".to_string(),
                            used_fraction: 0.72,
                            used_amount: None,
                            total_limit: None,
                            unit: None,
                            window: Some(nuo_protocol::QuotaWindowKind::Daily),
                            reset_at_ms: None,
                            reset_time_str: Some("in 3h 25m".to_string()),
                            group: None,
                        }],
                    },
                )),
                primary_balance: None,
                metrics: Vec::new(),
                model_rules: Vec::new(),
                updated_at_ms: None,
            },
        )),
        budget: Default::default(),
    };

    terminal.draw(|f| {
        let mut lm = nuo_view::model::layout::LayoutMap::new();
        let mut scroll = 0;
        let selection = nuo_view::model::selection::SelectionState::None;
        draw_connections_modal(
            f,
            &mut lm,
            crate::shell::overlays::provider::connections::ConnectionsModalProps {
                providers: &[],
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
                connection_usages: None,
            },
            &theme,
            &selection,
        );
    });

    let text = buffer_text(&terminal);
    assert!(text.contains("72% used"));
    assert!(!text.contains("remaining"));
    assert!(text.contains(" |  resets in 3h 25m"));
}

#[test]
fn redundant_plan_labels_are_identified_and_suppressed_in_header() {
    use crate::shell::overlays::provider::connections::is_redundant_plan_label;

    assert!(is_redundant_plan_label(
        "Antigravity Quota",
        "Google Antigravity",
        "google-antigravity",
        "Google Antigravity"
    ));
    assert!(is_redundant_plan_label(
        "antigravity quota active",
        "Google Antigravity",
        "google-antigravity",
        "Google Antigravity"
    ));
    assert!(is_redundant_plan_label(
        "Google Antigravity",
        "Google Antigravity",
        "google-antigravity",
        "Google Antigravity"
    ));
    assert!(is_redundant_plan_label(
        "google-antigravity",
        "Google Antigravity",
        "google-antigravity",
        "Google Antigravity"
    ));
    assert!(is_redundant_plan_label(
        "Command Code",
        "Command Code",
        "commandcode-goat",
        "CommandCode GOAT"
    ));
    assert!(is_redundant_plan_label(
        "Command Code",
        "Default",
        "commandcode-goat",
        "CommandCode GOAT"
    ));
    assert!(is_redundant_plan_label(
        "Quota",
        "Google Antigravity",
        "google-antigravity",
        "Google Antigravity"
    ));
    assert!(is_redundant_plan_label(
        "  ",
        "Google Antigravity",
        "google-antigravity",
        "Google Antigravity"
    ));

    assert!(!is_redundant_plan_label(
        "Google One AI Premium",
        "Google Antigravity",
        "google-antigravity",
        "Google Antigravity"
    ));
    assert!(!is_redundant_plan_label(
        "Pay-as-you-go",
        "DeepSeek",
        "deepseek",
        "DeepSeek"
    ));
    assert!(!is_redundant_plan_label(
        "Pro",
        "Anthropic",
        "anthropic",
        "Anthropic"
    ));
}

#[test]
fn model_detail_renders_pricing_and_off_peak_schedule_for_deepseek() {
    let theme = Theme::default();
    let rm = nuo_view::providers::RankedModel {
        section: nuo_view::providers::ModelSection::All,
        provider_id: "deepseek".to_string(),
        model: "deepseek-v4.1-flash".to_string(),
        name: Some("DeepSeek V4.1 Flash".to_string()),
        provider_label: "DeepSeek".to_string(),
        effort: Some("high".to_string()),
        thinking: Some(true),
        effort_levels: vec!["low".to_string(), "high".to_string(), "max".to_string()],
        favorite: false,
        usable: true,
        status_label: None,
        resets_at_ms: None,
        vision: None,
        context_window: 1_000_000,
        reason: None,
        availability_overridden: false,
        availability_stale: false,
        last_used_ms: None,
        match_id: None,
        match_name: None,
        match_connection: None,
        relevance_score: 0,
    };
    let rows = crate::shell::overlays::provider::models::model_detail_body(&rm, 80, &theme);
    let rendered = rows
        .into_iter()
        .map(|r| {
            let mut s = String::new();
            if let Some(p) = r.prefix {
                s.push_str(&p.text);
            }
            for seg in r.segments {
                s.push_str(&seg.text);
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("Pricing"), "must render Pricing header");
    assert!(
        rendered.contains("Standard Rates") && rendered.contains("$0.30 in | $1.20 out"),
        "must render standard flash rates: {rendered}"
    );
    assert!(
        rendered.contains("Prompt Cache Hit") && rendered.contains("$0.006 / 1M"),
        "must render cache hit rates: {rendered}"
    );
    assert!(
        rendered.contains("Off-Peak Rates") && rendered.contains("50% off ($0.15 in | $0.60 out)"),
        "must render off-peak rates: {rendered}"
    );
    assert!(
        rendered.contains("Off-Peak Window") && rendered.contains("UTC+8"),
        "must explain off-peak schedule: {rendered}"
    );
    assert!(
        rendered.contains("Current State"),
        "must report live rate window state: {rendered}"
    );
    // Verify minimum gap between label and value (never zero spaces!)
    assert!(
        rendered.contains("Prompt Cache Hit  $0.006 / 1M"),
        "Prompt Cache Hit must have at least two spaces of separation before rate: {rendered}"
    );
    assert!(
        rendered.contains("Reasoning Effort  high"),
        "Reasoning Effort must have at least two spaces of separation before effort level: {rendered}"
    );
}

#[test]
fn connection_detail_renders_active_model_pricing_and_rates() {
    let theme = Theme::default();
    let detail = nuo_protocol::ConnectionDetail {
        name: "deepseek-prod".to_string(),
        provider: "deepseek".to_string(),
        provider_label: "DeepSeek".to_string(),
        protocol: "Responses".to_string(),
        base_url: "https://api.deepseek.com/v1".to_string(),
        auth_type: "api_key".to_string(),
        budget: nuo_protocol::ConnectionBudget::Remote,
        api_key_source: "env:DEEPSEEK_API_KEY".to_string(),
        api_key_masked: Some("sk-••••1234".to_string()),
        active_model: Some("deepseek-v4.1-flash".to_string()),
        active_model_effort: Some("high".to_string()),
        active_model_thinking: Some(true),
        headers: Default::default(),
        user_agent: "nuo/0.0.41".to_string(),
        models: vec!["deepseek-v4.1-flash".to_string()],
        usage: nuo_protocol::ConnectionUsageState::Available(Box::new(
            nuo_protocol::ProviderUsage {
                plan: Some("Pay-as-you-go".to_string()),
                quota: Some(nuo_protocol::ProviderQuotaData::Balance(
                    nuo_protocol::BalanceQuota {
                        currency: "CNY".to_string(),
                        total_balance: Some(100.5),
                        cash_balance: Some(100.5),
                        voucher_balance: Some(0.0),
                        credit_limit: None,
                        consumed_amount: None,
                        display_primary: Some("¥100.50".to_string()),
                    },
                )),
                primary_balance: Some("¥100.50".to_string()),
                metrics: Vec::new(),
                model_rules: Vec::new(),
                description: None,
                updated_at_ms: None,
            },
        )),
        ..Default::default()
    };

    let rows =
        crate::shell::overlays::provider::connections::connection_detail_body(&detail, 0, &theme);
    let rendered = rows
        .into_iter()
        .map(|r| {
            let mut s = String::new();
            if let Some(p) = r.prefix {
                s.push_str(&p.text);
            }
            for seg in r.segments {
                s.push_str(&seg.text);
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        rendered.contains("Model Pricing & Rates"),
        "must render Model Pricing header: {rendered}"
    );
    assert!(
        rendered.contains("deepseek-v4.1-flash: $0.30 in | $1.20 out"),
        "must render active model rates: {rendered}"
    );
    assert!(
        rendered.contains("Off-Peak Rates") && rendered.contains("50% off"),
        "must render off-peak rates: {rendered}"
    );
    assert!(
        rendered.contains("Off-Peak Window"),
        "must render off-peak window: {rendered}"
    );
    assert!(
        rendered.contains("Total Balance") && rendered.contains("¥100.50 CNY"),
        "must keep total balance rendered alongside pricing: {rendered}"
    );
}

#[test]
fn connection_detail_omits_pricing_for_subscription_plans() {
    let theme = Theme::default();
    let detail = nuo_protocol::ConnectionDetail {
        name: "antigravity-main".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://daily-cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth".to_string(),
        active_model: Some("gemini-3.8-flash".to_string()),
        models: vec!["gemini-3.8-flash".to_string()],
        usage: nuo_protocol::ConnectionUsageState::Available(Box::new(
            nuo_protocol::ProviderUsage {
                plan: Some("Antigravity Plan".to_string()),
                quota: Some(nuo_protocol::ProviderQuotaData::Periodic(
                    nuo_protocol::PeriodicQuota {
                        buckets: vec![nuo_protocol::QuotaWindowBucket {
                            window: Some(nuo_protocol::QuotaWindowKind::Daily),
                            label: "gemini-3.8-flash".to_string(),
                            group: None,
                            used_fraction: 0.15,
                            used_amount: None,
                            total_limit: None,
                            unit: None,
                            reset_at_ms: None,
                            reset_time_str: None,
                        }],
                    },
                )),
                primary_balance: None,
                metrics: Vec::new(),
                model_rules: Vec::new(),
                description: None,
                updated_at_ms: None,
            },
        )),
        ..Default::default()
    };

    let rows =
        crate::shell::overlays::provider::connections::connection_detail_body(&detail, 0, &theme);
    let rendered = rows
        .into_iter()
        .map(|r| {
            let mut s = String::new();
            if let Some(p) = r.prefix {
                s.push_str(&p.text);
            }
            for seg in r.segments {
                s.push_str(&seg.text);
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !rendered.contains("Model Pricing & Rates"),
        "subscription plan must never render commercial API model pricing: {rendered}"
    );
    assert!(
        rendered.contains("Provider Usage & Quota"),
        "must render upstream periodic quota section: {rendered}"
    );
    assert!(
        rendered.contains("gemini-3.8-flash"),
        "must render periodic bucket: {rendered}"
    );
}

#[test]
fn periodic_quota_label_normalization_and_deduplication() {
    use crate::shell::overlays::provider::connections::normalize_quota_bucket_label;
    use nuo_protocol::QuotaWindowKind;

    // "Five Hour Limit Remaining" normalized to "5-Hour Limit" without redundant window tag
    assert_eq!(
        normalize_quota_bucket_label(
            "Five Hour Limit Remaining",
            Some("Gemini Models"),
            Some(QuotaWindowKind::Rolling5Hour)
        ),
        "5-Hour Limit"
    );

    // "Weekly Limit Remaining" normalized to "Weekly Limit" without redundant window tag
    assert_eq!(
        normalize_quota_bucket_label(
            "Weekly Limit Remaining",
            Some("Gemini Models"),
            Some(QuotaWindowKind::Weekly)
        ),
        "Weekly Limit"
    );

    // Group prefix stripped: "Gemini 4 Argon Weekly Limit" with group "Gemini 4 Argon" -> "Weekly Limit"
    assert_eq!(
        normalize_quota_bucket_label(
            "Gemini 4 Argon Weekly Limit",
            Some("Gemini 4 Argon"),
            Some(QuotaWindowKind::Weekly)
        ),
        "Weekly Limit"
    );

    // Non-redundant window label is preserved: "Gemini Pro Agent" with window Daily -> "Gemini Pro Agent | Daily"
    assert_eq!(
        normalize_quota_bucket_label("Gemini Pro Agent", None, Some(QuotaWindowKind::Daily)),
        "Gemini Pro Agent | Daily"
    );

    // Window already named in label: "5h Window" with window Rolling5Hour does not duplicate
    assert_eq!(
        normalize_quota_bucket_label("5h Window", None, Some(QuotaWindowKind::Rolling5Hour)),
        "5h Window"
    );
}

#[test]
fn reset_countdown_formatting_omits_redundant_clock_and_parentheses() {
    use crate::shell::overlays::provider::connections::format_reset_countdown;

    // Reset countdown from timestamp in future: "resets in Xh Ym", NEVER "(HH:MM)"
    let future_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 4 * 3600 * 1000
        + 52 * 60 * 1000;
    let cd = format_reset_countdown(Some(future_ms), None).unwrap();
    assert!(cd.starts_with("resets in 4h 5"), "got: {cd}");
    assert!(
        !cd.contains('('),
        "must not contain redundant clock parentheses: {cd}"
    );
    assert!(
        !cd.contains(')'),
        "must not contain redundant clock parentheses: {cd}"
    );

    // Reset countdown from duration string: "resets in 4h 52m", never "resets: in 4h 52m"
    assert_eq!(
        format_reset_countdown(None, Some("4h 52m")).as_deref(),
        Some("resets in 4h 52m")
    );
    assert_eq!(
        format_reset_countdown(None, Some("in 4h 52m")).as_deref(),
        Some("resets in 4h 52m")
    );
}

#[test]
fn periodic_quota_buckets_redesign_layout_order_and_styling() {
    use nuo_protocol::{
        PeriodicQuota, ProviderQuotaData, ProviderUsage, QuotaWindowBucket, QuotaWindowKind,
    };

    let theme = Theme::default();
    let detail = nuo_protocol::ConnectionDetail {
        name: "Google Antigravity".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth".to_string(),
        usage: nuo_protocol::ConnectionUsageState::Available(Box::new(ProviderUsage {
            plan: None,
            description: None,
            quota: Some(ProviderQuotaData::Periodic(PeriodicQuota {
                buckets: vec![
                    // Gemini Models: API returned Weekly first, then 5h
                    QuotaWindowBucket {
                        window: Some(QuotaWindowKind::Weekly),
                        label: "Weekly Limit Remaining".to_string(),
                        group: Some("Gemini Models".to_string()),
                        used_fraction: 0.08,
                        reset_time_str: Some("142h 4m".to_string()),
                        ..Default::default()
                    },
                    QuotaWindowBucket {
                        window: Some(QuotaWindowKind::Rolling5Hour),
                        label: "Five Hour Limit Remaining".to_string(),
                        group: Some("Gemini Models".to_string()),
                        used_fraction: 0.02,
                        reset_time_str: Some("4h 54m".to_string()),
                        ..Default::default()
                    },
                    // Claude and GPT models: API returned Weekly first, then 5h
                    QuotaWindowBucket {
                        window: Some(QuotaWindowKind::Weekly),
                        label: "Weekly Limit Remaining".to_string(),
                        group: Some("Claude and GPT models".to_string()),
                        used_fraction: 0.10,
                        reset_time_str: Some("90h 2m".to_string()),
                        ..Default::default()
                    },
                    QuotaWindowBucket {
                        window: Some(QuotaWindowKind::Rolling5Hour),
                        label: "Five Hour Limit Remaining".to_string(),
                        group: Some("Claude and GPT models".to_string()),
                        used_fraction: 0.0,
                        reset_time_str: Some("4h 59m".to_string()),
                        ..Default::default()
                    },
                    // Gemini 4 Argon: single bucket repeating group name
                    QuotaWindowBucket {
                        window: Some(QuotaWindowKind::Weekly),
                        label: "Gemini 4 Argon Weekly Limit".to_string(),
                        group: Some("Gemini 4 Argon".to_string()),
                        used_fraction: 0.97,
                        reset_time_str: Some("36h 45m".to_string()),
                        ..Default::default()
                    },
                ],
            })),
            primary_balance: None,
            metrics: Vec::new(),
            model_rules: Vec::new(),
            updated_at_ms: None,
        })),
        ..Default::default()
    };

    let rows =
        crate::shell::overlays::provider::connections::connection_detail_body(&detail, 0, &theme);
    let rendered_lines: Vec<String> = rows
        .into_iter()
        .map(|r| {
            let mut s = String::new();
            if let Some(p) = r.prefix {
                s.push_str(&p.text);
            }
            for seg in r.segments {
                s.push_str(&seg.text);
            }
            s
        })
        .collect();
    let text = rendered_lines.join("\n");

    // 1. Obscure symbol `▸` is NEVER rendered
    assert!(
        !text.contains('▸'),
        "must not contain obscure arrow symbol: {text}"
    );

    // 2. 5h limit is displayed before Weekly limit within each group
    let gemini_5h = text
        .find("5-Hour Limit  |  resets in 4h 54m")
        .expect("Gemini 5h line");
    let gemini_weekly = text
        .find("Weekly Limit  |  resets in 142h 4m")
        .expect("Gemini weekly line");
    assert!(
        gemini_5h < gemini_weekly,
        "5-Hour limit must appear before Weekly limit in Gemini Models"
    );

    let claude_5h = text
        .find("5-Hour Limit  |  resets in 4h 59m")
        .expect("Claude 5h line");
    let claude_weekly = text
        .find("Weekly Limit  |  resets in 90h 2m")
        .expect("Claude weekly line");
    assert!(
        claude_5h < claude_weekly,
        "5-Hour limit must appear before Weekly limit in Claude models"
    );

    // 3. Tautology is removed: no "Five Hour Limit Remaining | 5h Window", no "Weekly Limit Remaining | Weekly"
    assert!(!text.contains("Five Hour Limit Remaining"));
    assert!(!text.contains("5h Window"));
    assert!(!text.contains("Weekly Limit Remaining"));
    assert!(!text.contains("Gemini 4 Argon Weekly Limit | Weekly"));

    // 4. Gemini 4 Argon bucket label has group prefix stripped
    assert!(text.contains("Gemini 4 Argon\nWeekly Limit  |  resets in 36h 45m"));

    // 5. Title and reset countdown merged to one line without redundant clock in parentheses
    let p_quota = rendered_lines
        .iter()
        .position(|l| l.contains("Provider Usage & Quota"))
        .unwrap();
    let quota_slice = &rendered_lines[p_quota..];
    let quota_text = quota_slice.join("\n");
    assert!(
        !quota_text.contains('('),
        "quota text must not contain redundant clock parentheses: {quota_text}"
    );
    assert!(
        !quota_text.contains(')'),
        "quota text must not contain redundant clock parentheses: {quota_text}"
    );

    // 6. Groups are contiguous without blank spacer lines between them
    let empty_in_quota = quota_slice.iter().take(12).any(|l| l.trim().is_empty());
    assert!(
        !empty_in_quota,
        "quota groups must be contiguous without artificial blank lines: {text}"
    );
}

/// A connection row as the Connections list receives it.
fn ranked_connection(id: &str, label: &str, provider: &str) -> nuo_view::providers::RankedProvider {
    nuo_view::providers::RankedProvider {
        id: id.to_string(),
        name: label.to_string(),
        model: "gpt-5.5".to_string(),
        models: vec!["gpt-5.5".to_string()],
        builtin: false,
        provider: provider.to_string(),
        label: label.to_string(),
        m: None,
    }
}

#[test]
fn connections_long_name_never_starves_the_type_or_the_balance_badge() {
    // The same defect as the Models list, in the sibling surface: the name used
    // to be funded by halving the body width, which ignored both the provider
    // *type* column beside it and the balance badge pinned to the right edge.
    let theme = Theme::default();
    let providers = vec![
        ranked_connection(
            "relay-1",
            "DeepSeek Enterprise Relay (staging, eu-west-1, prepaid)",
            "openai",
        ),
        ranked_connection("direct", "Anthropic", "anthropic"),
    ];
    let mut usages = std::collections::HashMap::new();
    usages.insert(
        "relay-1".to_string(),
        nuo_protocol::ConnectionUsageState::Available(Box::new(nuo_protocol::ProviderUsage {
            primary_balance: Some("$12.40".to_string()),
            ..Default::default()
        })),
    );

    for body_width in [50usize, 66, 90] {
        let lines = crate::shell::overlays::provider::connections::provider_list_body(
            &providers,
            Some(&usages),
            0,
            &theme,
            body_width,
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let relay = &texts[0];
        let anthropic = &texts[1];

        // 1. The balance badge survives at every width — it is reserved, not
        //    placed, so no left-hand column can push it away.
        assert!(
            relay.contains("$12.40"),
            "body {body_width}: the balance badge is never starved: {relay:?}"
        );

        // 2. The provider type is the row's *optional* field: it must be the first
        //    thing to go when the body cannot hold it — never the name, never the
        //    badge. Whether it survives is not a guessed width threshold; it is
        //    whatever the solver decided, so the assertion asks the solver rather
        //    than hardcoding "66 columns is probably enough".
        let type_specs = [
            ColumnSpec::new(
                "DeepSeek Enterprise Relay (staging, eu-west-1, prepaid)".width(),
                12,
                46,
            ),
            ColumnSpec::new("OpenAI Platform".width(), "OpenAI Platform".width(), 20)
                .droppable()
                .priority(2),
        ];
        let solved = Grid::new(body_width)
            .gutter(GUTTER)
            .gap(GROUP_GAP)
            .max_gap(MAX_JUSTIFIED_GAP)
            .reserve_tail("$12.40".width() + GROUP_GAP)
            .justify(true)
            .layout(&type_specs);
        let type_funded = column(&solved.columns, 1).is_some();
        let has_type = relay.contains("OpenAI") || relay.contains("openai");
        assert_eq!(
            type_funded, has_type,
            "body {body_width}: the type column appears exactly when the solver funds it: {relay:?}"
        );
        assert!(
            relay.contains("DeepSeek"),
            "body {body_width}: the name is never the casualty: {relay:?}"
        );
        // The type column is the *last* occurrence: a connection may well be
        // named after its own provider, so the first hit would be the name.
        let type_col = |text: &str, needle: &str| {
            text.rfind(needle)
                .map(|b| unicode_width::UnicodeWidthStr::width(&text[..b]))
        };
        // When the type column is present in both rows it must be at the same
        // column — a conditional assertion that can silently skip is not an
        // assertion. The rows differ in name length, so a per-row column would
        // show up here immediately.
        let a = type_col(relay, "OpenAI").or_else(|| type_col(relay, "openai"));
        let b = type_col(anthropic, "Anthropic").or_else(|| type_col(anthropic, "anthropic"));
        assert_eq!(
            a.is_some(),
            b.is_some(),
            "body {body_width}: the type column is all-or-nothing across rows: {texts:?}"
        );
        if let Some(a) = a {
            assert_eq!(
                a,
                b.expect("checked above"),
                "body {body_width}: type column aligns: {texts:?}"
            );
        }

        // 3. The rows never overflow the body they were given.
        for text in &texts {
            assert!(
                unicode_width::UnicodeWidthStr::width(text.as_str()) <= body_width,
                "body {body_width}: row overflows: {text:?}"
            );
        }
    }
}

#[test]
fn connections_never_overflow_when_the_balance_badge_yields() {
    // The badge yields to the connection name on a body too narrow for both. What
    // must never happen is the other outcome: the solver reclaiming the badge's
    // columns while the renderer keeps painting the badge, which puts the row past
    // its body.
    let theme = Theme::default();
    let providers = vec![
        ranked_connection("relay-1", "DeepSeek Enterprise Relay", "openai"),
        ranked_connection("direct", "Anthropic", "anthropic"),
    ];
    let mut usages = std::collections::HashMap::new();
    usages.insert(
        "relay-1".to_string(),
        nuo_protocol::ConnectionUsageState::Available(Box::new(nuo_protocol::ProviderUsage {
            primary_balance: Some("$12.40".to_string()),
            ..Default::default()
        })),
    );

    for body_width in 12..=70 {
        let lines = crate::shell::overlays::provider::connections::provider_list_body(
            &providers,
            Some(&usages),
            0,
            &theme,
            body_width,
        );
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                unicode_width::UnicodeWidthStr::width(text.as_str()) <= body_width,
                "body {body_width}: the row overflows: {text:?}"
            );
        }
        // The identity survives even where the badge cannot.
        let first: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            first.contains("Deep") || first.contains("Dee"),
            "body {body_width}: the name is never the casualty: {first:?}"
        );
    }
}
