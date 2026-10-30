//! The Connections modal (provider instance management) and connection inspection details.
use nuotc::{
    Frame, {Line, Span}, {Modifier, Style},
};

use super::super::common::truncate_ellipsis;
use super::common::{
    draw_picker_search_row, match_set, place_picker_search_cursor, render_pricing_detail_rows,
    search_empty_body, split_search_body,
};
use crate::shell::render::Theme;
use nuo_ui_primitives::components::detail::{
    DetailLayout, format_detail_label, render_progress_bar_spans,
};
use nuo_ui_primitives::components::options::{ChoiceTone, choice_style};
use nuo_ui_primitives::components::row::{GUTTER, ListRow, RowGroup, RowStyledAtom};
use nuo_ui_primitives::primitives::{
    BodyRenderOptions, FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN,
    breadcrumb_parts, keyvocab, modal_area, modal_frame, modal_header, modal_header_parts,
    render_body, render_centered_body, render_modal_footer, render_modal_footer_with_extra,
};
use nuo_view::components::selectable_body::{RowSegment, SelectableRow, render_selectable_body};
use nuo_view::model::layout::LayoutMap;
use nuo_view::model::selection::SelectionState;
use nuo_view::providers::RankedProvider;

/// Properties for rendering the Connections modal.
pub struct ConnectionsModalProps<'a> {
    pub providers: &'a [RankedProvider],
    pub current_provider: &'a str,
    pub modal_index: usize,
    pub query: &'a str,
    pub cursor_position: usize,
    pub scroll: &'a mut usize,
    pub follow_selection: bool,
    pub search: bool,
    /// Whether this surface currently owns the terminal cursor
    /// (`App::caret_owner == CaretOwner::Overlay`). The picker
    /// never places the physical cursor on its own authority — the frame-level
    /// arbiter decides that, and this flag is its verdict threaded down.
    pub show_caret: bool,
    pub connection_info_detail: bool,
    pub connection_detail: Option<&'a nuo_wire::ConnectionDetail>,
    pub connection_info_scroll: &'a mut usize,
    pub spinner_phase: usize,
    pub connection_info_standalone: bool,
    pub refreshing: bool,
    /// Ambient quota balances keyed by connection id (ADR-0036).
    pub connection_usages:
        Option<&'a std::collections::HashMap<String, nuo_wire::ConnectionUsageState>>,
}

/// Draw the **Connections** modal — the provider-instance management surface (`/connections`).
pub fn draw_connections_modal(
    frame: &mut Frame,
    layout_map: &mut LayoutMap,
    props: ConnectionsModalProps<'_>,
    theme: &Theme,
    selection: &SelectionState,
) -> nuotc::Rect {
    let ConnectionsModalProps {
        providers,
        current_provider,
        modal_index,
        query,
        cursor_position,
        scroll,
        follow_selection,
        search,
        show_caret,
        connection_info_detail,
        connection_detail,
        connection_info_scroll,
        spinner_phase,
        connection_info_standalone,
        refreshing,
        connection_usages,
    } = props;
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme, true, true);

    let header_rect = f.header;

    // `a add` opens the preset chooser and `Enter details` drills into connection info/usage.
    let refresh_label = if refreshing {
        "refreshing…"
    } else {
        "refresh"
    };
    let browse_hints: [FooterHint; 8] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::primary(crate::shell::keymap::Key::ENTER.display(), "details"),
        FooterHint::secondary("a", "preset"),
        FooterHint::secondary("c", "custom"),
        FooterHint::secondary("e", "edit"),
        FooterHint::secondary("r", refresh_label),
        FooterHint::always(crate::shell::keymap::Key::ESC.display(), "close"),
    ];
    let browse_extra: [FooterHintWithBand; 1] = [FooterHintWithBand {
        key: "D",
        label: "delete",
        rank: 70,
    }];
    let search_hints: [FooterHint; 3] = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::always(crate::shell::keymap::Key::ESC.display(), "clear search"),
    ];
    let (hints, extra): (&[FooterHint], &[FooterHintWithBand]) = if search {
        (&search_hints, &[])
    } else {
        (&browse_hints, &browse_extra)
    };

    if connection_info_detail {
        if connection_info_standalone {
            modal_header(frame, f.header, "Connection Details", theme);
        } else {
            let child_title = connection_detail
                .map(|d| d.name.as_str())
                .unwrap_or("Details");
            let header = breadcrumb_parts("Connections", child_title);
            modal_header_parts(frame, f.header, &header, theme);
        }
        let mut detail_footer: Vec<FooterHint> = if connection_info_standalone {
            vec![
                FooterHint::always(crate::shell::keymap::Key::ESC.display(), "close"),
                FooterHint::secondary("r", "refresh"),
                FooterHint::secondary("e", "edit"),
            ]
        } else {
            vec![
                FooterHint::always(crate::shell::keymap::Key::ESC.display(), "list"),
                FooterHint::secondary("r", "refresh"),
                FooterHint::secondary("e", "edit"),
            ]
        };
        if let Some(detail) = connection_detail
            && !detail.models.is_empty()
        {
            detail_footer.push(FooterHint::primary("m", "browse models"));
        }
        let rows = match connection_detail {
            None => {
                let spin = theme.glyphs.spinner_frame(spinner_phase);
                vec![
                    SelectableRow::empty(),
                    SelectableRow::styled(
                        "Loading connection details and provider usage…",
                        Style::default().fg(theme.muted()),
                    )
                    .with_prefix(RowSegment::styled(
                        format!("{spin} "),
                        Style::default().fg(theme.primary),
                    )),
                ]
            }
            Some(detail) => connection_detail_body(detail, spinner_phase, theme),
        };
        render_selectable_body(
            frame,
            f.body,
            &rows,
            connection_info_scroll,
            None,
            theme,
            selection,
            layout_map,
        );
        if let Some(fo) = f.footer {
            render_modal_footer(frame, fo, &detail_footer, theme);
        }
        return area;
    }

    if refreshing {
        let spin = theme.glyphs.spinner_frame(spinner_phase);
        let header = [
            nuo_ui_primitives::elevation::HeaderPart::title("Connections"),
            nuo_ui_primitives::elevation::HeaderPart::Text {
                text: "  ",
                accent: false,
            },
            nuo_ui_primitives::elevation::HeaderPart::Text {
                text: spin,
                accent: true,
            },
            nuo_ui_primitives::elevation::HeaderPart::Text {
                text: " refreshing…",
                accent: false,
            },
        ];
        modal_header_parts(frame, header_rect, &header, theme);
    } else {
        modal_header(frame, header_rect, "Connections", theme);
    }

    let (search_rect, body_rect) = split_search_body(f.body, search);
    if let Some(search_rect) = search_rect {
        draw_picker_search_row(frame, search_rect, query, cursor_position, theme);
    }

    if providers.is_empty() && !search {
        let body = connections_empty_body(theme);
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, hints, extra, theme);
        }
        return area;
    }

    if providers.is_empty() && search {
        let body = search_empty_body(theme);
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, hints, extra, theme);
        }
        if show_caret && let Some(sr) = search_rect {
            place_picker_search_cursor(frame, sr, query, cursor_position);
        }
        return area;
    }

    let body = provider_list_body(
        providers,
        connection_usages,
        current_provider,
        modal_index,
        theme,
        body_rect.width as usize,
    );
    let follow = if follow_selection {
        Some(modal_index)
    } else {
        None
    };
    render_body(
        frame,
        body_rect,
        body,
        scroll,
        BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
        theme,
    );

    if let Some(fo) = f.footer {
        render_modal_footer_with_extra(frame, fo, hints, extra, theme);
    }

    if show_caret
        && search
        && let Some(sr) = search_rect
    {
        place_picker_search_cursor(frame, sr, query, cursor_position);
    }
    area
}

/// Build the **Connections** provider list body via the shared [`nuo_ui_primitives::components::row::ListRow`]
/// standard.
pub(crate) fn provider_list_body(
    providers: &[RankedProvider],
    connection_usages: Option<&std::collections::HashMap<String, nuo_wire::ConnectionUsageState>>,
    _current_provider: &str,
    modal_index: usize,
    theme: &Theme,
    body_width: usize,
) -> Vec<Line<'static>> {
    let name_budget = (body_width / 2).saturating_sub(GUTTER + 1).max(1);

    let mut body: Vec<Line<'static>> = Vec::new();
    for (sel, rp) in providers.iter().enumerate() {
        let is_selected = sel == modal_index;
        let style = choice_style(ChoiceTone::Filled, is_selected, theme);
        let matched = match_set(rp.m.as_ref());

        let name = truncate_ellipsis(&rp.label, name_budget);
        let mut identity = RowGroup::fixed();
        for (char_idx, c) in name.chars().enumerate() {
            let cs = if matched.contains(&char_idx) {
                Style::default()
                    .bg(style.bg)
                    .fg(if is_selected { style.fg } else { theme.brand() })
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .bg(style.bg)
                    .fg(style.fg)
                    .add_modifier(Modifier::BOLD)
            };
            identity = identity.styled(
                RowStyledAtom {
                    text: c.to_string(),
                    style: cs,
                },
                0,
            );
        }

        let mut row = ListRow::new(style, body_width).group(identity);

        if let Some(usages) = connection_usages
            && let Some(nuo_wire::ConnectionUsageState::Available(usage)) = usages.get(&rp.id)
            && let Some(balance) = &usage.primary_balance
        {
            row = row.group(RowGroup::trailing().styled(
                RowStyledAtom {
                    text: balance.clone(),
                    style: Style::default().fg(if is_selected { style.fg } else { theme.success }),
                },
                0,
            ));
        }

        if let Some(label) = nuo_view::providers::provider_type_label(&rp.provider) {
            row = row.group(RowGroup::midpoint().text(label, style.dim, 0));
        }

        body.push(row.finish());
    }
    body
}

/// The Connections empty-state body: shown when no provider instance exists.
pub(crate) fn connections_empty_body(theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "No connections yet",
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Press ", Style::default().fg(theme.muted())),
            Span::styled(
                "a",
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for a preset or ", Style::default().fg(theme.muted())),
            Span::styled(
                "c",
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for custom", Style::default().fg(theme.muted())),
        ]),
    ]
}

/// Format a reset time into human-friendly countdown (e.g. "resets in 2h 15m").
pub(crate) fn format_reset_countdown(
    reset_at_ms: Option<u64>,
    reset_time_str: Option<&str>,
) -> Option<String> {
    if let Some(reset_ms) = reset_at_ms {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if reset_ms > now_ms {
            let diff_secs = (reset_ms - now_ms) / 1000;
            let hours = diff_secs / 3600;
            let mins = (diff_secs % 3600) / 60;
            let secs = diff_secs % 60;
            let time_str = if hours > 0 {
                format!("{hours}h {mins}m")
            } else if mins > 0 {
                format!("{mins}m {secs}s")
            } else {
                format!("{secs}s")
            };
            return Some(format!("resets in {time_str}"));
        } else {
            return Some("resets soon".to_string());
        }
    }
    if let Some(raw) = reset_time_str
        && !raw.trim().is_empty()
    {
        let raw_stripped = raw.split('(').next().unwrap_or(raw).trim();
        let trimmed = raw_stripped.trim();
        if trimmed.starts_with("in ") {
            return Some(format!("resets {trimmed}"));
        } else if trimmed.starts_with("resets") {
            return Some(trimmed.to_string());
        } else if trimmed.contains('T') {
            return Some(format!("resets at {trimmed}"));
        } else {
            return Some(format!("resets in {trimmed}"));
        }
    }
    None
}

/// Render the detail body rows for one connection (configuration + caller identity + models + provider usage).
pub(crate) fn connection_detail_body(
    detail: &nuo_wire::ConnectionDetail,
    spinner_phase: usize,
    theme: &Theme,
) -> Vec<SelectableRow> {
    let label = Style::default().fg(theme.dim());
    let value = Style::default().fg(theme.fg());
    let muted = Style::default().fg(theme.muted());
    let highlight = Style::default().fg(theme.primary);
    let warning = Style::default().fg(theme.warning);

    let provider_display = if !detail.provider_label.is_empty() {
        detail.provider_label.as_str()
    } else {
        nuo_wire::model_providers::model_provider_label(&detail.provider)
    };

    let mut layout = DetailLayout::new(theme)
        .section("Configuration")
        .kv("Name", &detail.name)
        .kv("Provider", provider_display)
        .kv("Protocol", &detail.protocol)
        .kv("Base URL", &detail.base_url)
        .kv("Auth Type", &detail.auth_type);

    match &detail.budget {
        nuo_wire::ConnectionBudget::Remote => {
            layout = layout.kv("Budget Policy", "Remote (Upstream Managed)");
        }
        nuo_wire::ConnectionBudget::Local { limit, period, .. } => {
            let budget_str = format!("Local ({}, {} cap)", limit.format_usd(), period.label());
            layout = layout.kv_styled("Budget Policy", &budget_str, warning);
        }
    }
    if let Some(masked) = &detail.api_key_masked {
        layout = layout.kv_segments(
            "API Key",
            vec![
                RowSegment::styled(masked.clone(), value),
                RowSegment::styled(format!(" ({})", detail.api_key_source), muted),
            ],
        );
    } else {
        layout = layout.kv("Credential", &detail.api_key_source);
    }
    if let Some(active) = &detail.active_model {
        let mut segments = vec![RowSegment::styled(active.clone(), value)];
        if let Some(effort) = &detail.active_model_effort {
            segments.push(RowSegment::styled(
                format!("  ·  reasoning: {effort}"),
                muted,
            ));
        } else if detail.active_model_thinking == Some(true) {
            segments.push(RowSegment::styled("  ·  thinking: enabled", muted));
        }
        layout = layout.kv_segments("Default Active", segments);
    }

    // Transport & Headers
    layout = layout
        .section("Transport")
        .kv("User-Agent", &detail.user_agent);
    if !detail.headers.is_empty() {
        layout = layout.raw_row(SelectableRow::styled(format_detail_label("Headers"), label));
        for (k, v) in &detail.headers {
            layout = layout.raw_row(
                SelectableRow::from_segments(vec![
                    RowSegment::styled(format!("{k}: "), label),
                    RowSegment::styled(v.to_string(), value),
                ])
                .with_prefix(RowSegment::styled("  • ", label)),
            );
        }
    }

    // Served Models
    let models_title = if detail.models.is_empty() {
        "Served Models".to_string()
    } else {
        format!("Served Models ({})", detail.models.len())
    };
    layout = layout.section(&models_title);
    if detail.models.is_empty() {
        layout = layout.raw_row(SelectableRow::styled("(no models configured)", muted));
    } else {
        let active_desc = detail
            .active_model
            .as_deref()
            .unwrap_or("(none configured)");
        layout = layout
            .kv(
                "Catalog",
                format!(
                    "{} discovered  ·  default: {}",
                    detail.models.len(),
                    active_desc
                ),
            )
            .kv_segments(
                "Action",
                vec![
                    RowSegment::styled("[ m ] ", highlight.add_modifier(Modifier::BOLD)),
                    RowSegment::styled("Browse & filter in /models", muted),
                ],
            );
    }

    // Provider Usage & Quota
    layout = layout.section("Provider Usage & Quota");

    match &detail.usage {
        nuo_wire::ConnectionUsageState::Available(usage) => {
            if let Some(plan) = &usage.plan
                && plan.len() <= 40
                && !plan.contains('\n')
                && !is_redundant_plan_label(
                    plan,
                    &detail.name,
                    &detail.provider,
                    &detail.provider_label,
                )
            {
                layout = layout.kv("Plan", plan);
            }

            let mut rendered_quota = false;

            if let Some(quota_data) = &usage.quota {
                match quota_data {
                    nuo_wire::ProviderQuotaData::Periodic(periodic) => {
                        rendered_quota = true;
                        layout = render_periodic_quota_buckets(
                            &periodic.buckets,
                            layout,
                            value,
                            muted,
                            highlight,
                            theme,
                        );
                    }
                    nuo_wire::ProviderQuotaData::Balance(balance) => {
                        rendered_quota = true;
                        layout =
                            render_balance_quota_block(balance, layout, value, highlight, theme);
                    }
                    nuo_wire::ProviderQuotaData::Composite {
                        balance,
                        periodic,
                        rate_limits,
                    } => {
                        rendered_quota = true;
                        if let Some(bal) = balance {
                            layout =
                                render_balance_quota_block(bal, layout, value, highlight, theme);
                        }
                        if let Some(per) = periodic {
                            layout = render_periodic_quota_buckets(
                                &per.buckets,
                                layout,
                                value,
                                muted,
                                highlight,
                                theme,
                            );
                        }
                        for rl in rate_limits {
                            layout = layout.kv(
                                "Rate Limit",
                                format!("{} req / {}", rl.requests, rl.interval),
                            );
                        }
                    }
                }
            }

            if !rendered_quota {
                if let Some(bal) = &usage.primary_balance {
                    layout = layout.kv_styled(
                        "Primary Balance",
                        bal,
                        highlight.add_modifier(Modifier::BOLD),
                    );
                }
                for metric in &usage.metrics {
                    let val = match &metric.unit {
                        Some(u) => format!("{} {}", metric.value, u),
                        None => metric.value.clone(),
                    };
                    layout = layout.kv_styled(&metric.label, &val, value);
                }
            }

            if let Some(desc) = &usage.description {
                layout = layout
                    .empty_row()
                    .raw_row(SelectableRow::styled(desc.clone(), muted));
            }
        }
        nuo_wire::ConnectionUsageState::Unsupported => {
            layout = layout.raw_row(SelectableRow::styled(
                "Usage and quota query is not supported for this provider endpoint.",
                muted,
            ));
        }
        nuo_wire::ConnectionUsageState::Error(err) => {
            layout = layout.raw_row(SelectableRow::from_segments(vec![
                RowSegment::styled("⚠ Usage query failed: ", warning),
                RowSegment::styled(err.clone(), value),
            ]));
        }
        nuo_wire::ConnectionUsageState::Fetching => {
            let spin = theme.glyphs.spinner_frame(spinner_phase);
            layout = layout.raw_row(SelectableRow::from_segments(vec![
                RowSegment::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                RowSegment::styled("Querying upstream provider quota & balance…", muted),
            ]));
        }
    }

    let mut rows = layout.build();

    // Active Model Pricing & Rates (if recognized)
    if let Some(active) = &detail.active_model
        && let Some(pricing_rows) = render_pricing_detail_rows(
            &detail.provider,
            active,
            Some(active.as_str()),
            "Model Pricing & Rates",
            theme,
        )
    {
        rows.extend(pricing_rows);
    }

    rows
}

pub(crate) fn render_balance_quota_block<'a>(
    balance: &nuo_wire::BalanceQuota,
    mut layout: DetailLayout<'a>,
    value: Style,
    highlight: Style,
    theme: &'a Theme,
) -> DetailLayout<'a> {
    if let (Some(consumed), Some(limit)) = (balance.consumed_amount, balance.credit_limit)
        && limit > 0.0
    {
        let frac = (consumed / limit) as f32;
        let pct = (frac * 100.0).round() as u32;
        layout = layout.raw_row(SelectableRow::styled(
            "Credit Limit Consumption",
            value.add_modifier(Modifier::BOLD),
        ));
        let mut bar_spans = render_progress_bar_spans(frac, 20, theme);
        bar_spans.push(Span::styled(
            format!("  ${:.2} / ${:.2} ({pct}% used)", consumed, limit),
            value,
        ));
        let segments = bar_spans
            .into_iter()
            .map(|s| RowSegment::styled(s.content.into_owned(), s.style))
            .collect();
        layout = layout.raw_row(
            SelectableRow::from_segments(segments)
                .with_prefix(RowSegment::styled("  ", Style::default().fg(theme.dim()))),
        );
    }

    if let Some(total) = balance.total_balance {
        let sym = if balance.currency == "CNY" {
            "¥"
        } else if balance.currency == "USD" {
            "$"
        } else {
            ""
        };
        let total_str = if !sym.is_empty() {
            format!("{sym}{:.2} {}", total, balance.currency)
        } else {
            format!("{:.2} {}", total, balance.currency)
        };
        layout = layout.kv_styled(
            "Total Balance",
            total_str,
            highlight.add_modifier(Modifier::BOLD),
        );
    } else if let Some(prim) = &balance.display_primary {
        layout = layout.kv_styled("Balance", prim, highlight.add_modifier(Modifier::BOLD));
    }

    if let Some(cash) = balance.cash_balance {
        let sym = if balance.currency == "CNY" {
            "¥"
        } else if balance.currency == "USD" {
            "$"
        } else {
            ""
        };
        layout = layout.kv_indented(2, "├─ Recharge:", format!("{sym}{:.2}", cash), value);
    }
    if let Some(voucher) = balance.voucher_balance {
        let sym = if balance.currency == "CNY" {
            "¥"
        } else if balance.currency == "USD" {
            "$"
        } else {
            ""
        };
        layout = layout.kv_indented(2, "└─ Voucher:", format!("{sym}{:.2}", voucher), value);
    }

    layout
}

/// Helper to rank windows by duration (shorter window first, e.g. 5h before daily before weekly).
fn quota_window_rank(bucket: &nuo_wire::QuotaWindowBucket) -> (u8, u64) {
    let rank = match bucket.window {
        Some(nuo_wire::QuotaWindowKind::Rolling5Hour) => 1,
        Some(nuo_wire::QuotaWindowKind::Daily) => 2,
        Some(nuo_wire::QuotaWindowKind::Weekly) => 3,
        Some(nuo_wire::QuotaWindowKind::Monthly) => 4,
        Some(nuo_wire::QuotaWindowKind::Custom) => 5,
        None => {
            let lower = bucket.label.to_ascii_lowercase();
            if lower.contains("5h") || lower.contains("5-hour") || lower.contains("five hour") {
                1
            } else if lower.contains("daily") || lower.contains("24h") || lower.contains("day") {
                2
            } else if lower.contains("weekly") || lower.contains("week") || lower.contains("7d") {
                3
            } else if lower.contains("monthly") || lower.contains("month") || lower.contains("30d")
            {
                4
            } else {
                6
            }
        }
    };
    (rank, bucket.reset_at_ms.unwrap_or(u64::MAX))
}

/// Normalize bucket labels to avoid tautology and redundant window tags.
pub(crate) fn normalize_quota_bucket_label(
    label: &str,
    group: Option<&str>,
    window: Option<nuo_wire::QuotaWindowKind>,
) -> String {
    let mut clean = label.trim().to_string();

    // 1. If group is specified and clean starts with group, strip group prefix
    if let Some(grp) = group {
        let grp_trim = grp.trim();
        if !grp_trim.is_empty() {
            let lower_clean = clean.to_ascii_lowercase();
            let lower_grp = grp_trim.to_ascii_lowercase();
            if lower_clean.starts_with(&lower_grp) {
                let remainder = &clean[lower_grp.len()..];
                let trimmed = remainder.trim_start_matches([' ', '-', ':', '·']);
                if !trimmed.is_empty() {
                    clean = trimmed.to_string();
                }
            }
        }
    }

    // 2. Strip redundant "Remaining" or "Limit Remaining"
    let lower = clean.to_ascii_lowercase();
    if lower.ends_with(" limit remaining") {
        clean.truncate(clean.len() - " limit remaining".len());
        clean.push_str(" Limit");
    } else if lower.ends_with(" remaining") {
        clean.truncate(clean.len() - " remaining".len());
    }

    // Convert "Five Hour" or "Five-Hour" to "5-Hour"
    if clean.starts_with("Five Hour") {
        clean = format!("5-Hour{}", &clean["Five Hour".len()..]);
    } else if clean.starts_with("Five-Hour") {
        clean = format!("5-Hour{}", &clean["Five-Hour".len()..]);
    } else if clean.eq_ignore_ascii_case("five hour") {
        clean = "5-Hour Limit".to_string();
    }

    // 3. Append window tag only if not already specified in the clean label
    let lower = clean.to_ascii_lowercase();
    let contains_window = match window {
        Some(nuo_wire::QuotaWindowKind::Rolling5Hour) => {
            lower.contains("5h")
                || lower.contains("5-hour")
                || lower.contains("5 hour")
                || lower.contains("five hour")
        }
        Some(nuo_wire::QuotaWindowKind::Daily) => {
            lower.contains("daily") || lower.contains("24h") || lower.contains("day")
        }
        Some(nuo_wire::QuotaWindowKind::Weekly) => {
            lower.contains("weekly") || lower.contains("week") || lower.contains("7d")
        }
        Some(nuo_wire::QuotaWindowKind::Monthly) => {
            lower.contains("monthly") || lower.contains("month") || lower.contains("30d")
        }
        Some(nuo_wire::QuotaWindowKind::Custom) => lower.contains("custom"),
        None => true,
    };

    if !contains_window {
        if let Some(w) = window {
            clean.push_str(&format!(" · {}", w.label()));
        }
    }

    clean
}

pub(crate) fn render_periodic_quota_buckets<'a>(
    buckets: &[nuo_wire::QuotaWindowBucket],
    mut layout: DetailLayout<'a>,
    value: Style,
    muted: Style,
    _highlight: Style,
    theme: &'a Theme,
) -> DetailLayout<'a> {
    if buckets.is_empty() {
        return layout.raw_row(SelectableRow::styled(
            "Active (no specific bucket limits reported)",
            muted,
        ));
    }

    // Preserve the first-encountered group ordering, and sort buckets within each group
    // by window duration ascending (shorter windows like 5h before weekly).
    let mut group_order: Vec<Option<String>> = Vec::new();
    for b in buckets {
        if !group_order.contains(&b.group) {
            group_order.push(b.group.clone());
        }
    }

    let mut sorted_buckets: Vec<&nuo_wire::QuotaWindowBucket> = buckets.iter().collect();
    sorted_buckets.sort_by_key(|b| {
        let grp_idx = group_order
            .iter()
            .position(|g| g == &b.group)
            .unwrap_or(usize::MAX);
        (grp_idx, quota_window_rank(b))
    });

    let mut last_group: Option<&str> = None;
    for bucket in sorted_buckets {
        let current_group = bucket.group.as_deref();
        if current_group != last_group {
            if let Some(grp) = current_group {
                layout = layout.raw_row(SelectableRow::styled(
                    grp.to_string(),
                    value.add_modifier(Modifier::BOLD),
                ));
            }
            last_group = current_group;
        }

        let bucket_label =
            normalize_quota_bucket_label(&bucket.label, current_group, bucket.window);

        let reset_str =
            format_reset_countdown(bucket.reset_at_ms, bucket.reset_time_str.as_deref());

        let title_row = if let Some(reset) = reset_str {
            SelectableRow::from_segments(vec![
                RowSegment::styled(bucket_label, value),
                RowSegment::styled(format!("  ·  {reset}"), muted),
            ])
        } else {
            SelectableRow::styled(bucket_label, value)
        };
        layout = layout.raw_row(title_row);

        let pct_used = (bucket.used_fraction * 100.0).round() as u32;
        let mut bar_spans = render_progress_bar_spans(bucket.used_fraction, 20, theme);
        bar_spans.push(Span::styled(format!("  {pct_used}% used"), value));
        let segments = bar_spans
            .into_iter()
            .map(|s| RowSegment::styled(s.content.into_owned(), s.style))
            .collect();
        let bar_row = SelectableRow::from_segments(segments);
        layout = layout.raw_row(bar_row);
    }
    layout
}

pub(crate) fn is_redundant_plan_label(
    plan: &str,
    connection_name: &str,
    provider: &str,
    provider_label: &str,
) -> bool {
    let p = plan.trim();
    if p.is_empty() {
        return true;
    }
    let lower = p.to_ascii_lowercase();
    if lower == "antigravity quota"
        || lower == "antigravity quota active"
        || lower == "quota"
        || lower == "provider quota"
        || lower == "usage & quota"
        || lower == "provider usage & quota"
        || lower == "standard"
    {
        return true;
    }

    fn normalize(s: &str) -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    }

    let norm_plan = normalize(p);
    if norm_plan.is_empty() {
        return true;
    }

    let norm_conn = normalize(connection_name);
    let norm_prov = normalize(provider);
    let norm_prov_lbl = normalize(provider_label);

    norm_plan == norm_conn
        || norm_plan == norm_prov
        || norm_plan == norm_prov_lbl
        || norm_plan.trim_end_matches("plan") == norm_conn.trim_end_matches("plan")
        || norm_plan.trim_end_matches("plan") == norm_prov.trim_end_matches("plan")
        || norm_plan.trim_end_matches("plan") == norm_prov_lbl.trim_end_matches("plan")
}
