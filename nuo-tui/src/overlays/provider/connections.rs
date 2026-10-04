//! The Connections modal (provider instance management) and connection inspection details.

use nuotc::{
    Frame, {Line, Span}, {Modifier, Style},
};

use super::super::common::truncate_ellipsis;
use super::common::{
    draw_picker_search_row, match_set, place_picker_search_cursor, search_empty_body,
    split_search_body,
};
use crate::components::options::{ChoiceTone, choice_style};
use crate::components::row::{GUTTER, ListRow, RowGroup, RowStyledAtom};
use crate::components::selectable_body::{RowSegment, SelectableRow, render_selectable_body};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    BodyRenderOptions, FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN,
    breadcrumb_parts, keyvocab, modal_area, modal_frame, modal_header, modal_header_parts,
    render_body, render_centered_body, render_modal_footer, render_modal_footer_with_extra,
};
use crate::providers::RankedProvider;
use crate::render::Theme;

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
    /// (`App::caret_owner() == CaretOwner::Overlay`, ADR-0205). The picker
    /// never places the physical cursor on its own authority — the frame-level
    /// arbiter decides that, and this flag is its verdict threaded down.
    pub show_caret: bool,
    pub connection_info_detail: bool,
    pub connection_detail: Option<&'a nuo_wire::ConnectionDetail>,
    pub connection_info_scroll: &'a mut usize,
    pub spinner_phase: usize,
    pub connection_info_standalone: bool,
    pub refreshing: bool,
    pub connection_models_expanded: bool,
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
        connection_models_expanded,
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
        FooterHint::key_primary(crate::keymap::Key::ENTER, "details"),
        FooterHint::secondary("a", "preset"),
        FooterHint::secondary("c", "custom"),
        FooterHint::secondary("e", "edit"),
        FooterHint::secondary("r", refresh_label),
        FooterHint::key_always(crate::keymap::Key::ESC, "close"),
    ];
    let browse_extra: [FooterHintWithBand; 1] = [FooterHintWithBand {
        key: "D",
        label: "delete",
        rank: 70,
    }];
    let search_hints: [FooterHint; 3] = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::key_always(crate::keymap::Key::ESC, "clear search"),
    ];
    let (hints, extra): (&[FooterHint], &[FooterHintWithBand]) = if search {
        (&search_hints, &[])
    } else {
        (&browse_hints, &browse_extra)
    };

    if connection_info_detail {
        if connection_info_standalone {
            let conn_title = connection_detail
                .map(|d| format!("Connection Details [{}]", d.name))
                .unwrap_or_else(|| "Connection Details".to_string());
            modal_header(frame, f.header, &conn_title, theme);
        } else {
            let conn_title = connection_detail
                .map(|d| format!("Details [{}]", d.name))
                .unwrap_or_else(|| "Details".to_string());
            let header = breadcrumb_parts("Connections", &conn_title);
            modal_header_parts(frame, f.header, &header, theme);
        }
        let mut detail_footer: Vec<FooterHint> = if connection_info_standalone {
            vec![
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
                FooterHint::secondary("r", "refresh"),
                FooterHint::secondary("e", "edit"),
            ]
        } else {
            vec![
                FooterHint::key_always(crate::keymap::Key::ESC, "list"),
                FooterHint::secondary("r", "refresh"),
                FooterHint::secondary("e", "edit"),
            ]
        };
        if let Some(detail) = connection_detail
            && detail.models.len() > 1
        {
            detail_footer.push(FooterHint::secondary(
                "Enter",
                if connection_models_expanded {
                    "collapse models"
                } else {
                    "expand models"
                },
            ));
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
            Some(detail) => {
                connection_detail_body(detail, connection_models_expanded, spinner_phase, theme)
            }
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
            crate::elevation::HeaderPart::title("Connections"),
            crate::elevation::HeaderPart::Text {
                text: "  ",
                accent: false,
            },
            crate::elevation::HeaderPart::Text {
                text: spin,
                accent: true,
            },
            crate::elevation::HeaderPart::Text {
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

/// Build the **Connections** provider list body via the shared [`crate::components::row::ListRow`]
/// standard.
pub(crate) fn provider_list_body(
    providers: &[RankedProvider],
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

        if let Some(label) = crate::providers::provider_type_label(&rp.provider) {
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

/// Format a reset time into human-friendly countdown / clock (e.g. "resets in 2h 15m (14:30)").
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
            use chrono::{Local, TimeZone};
            let clock = Local
                .timestamp_millis_opt(reset_ms as i64)
                .single()
                .map(|dt| dt.format("%H:%M").to_string())
                .unwrap_or_default();
            if clock.is_empty() {
                return Some(format!("resets in {time_str}"));
            } else {
                return Some(format!("resets in {time_str} ({clock})"));
            }
        } else {
            return Some("resets soon".to_string());
        }
    }
    if let Some(raw) = reset_time_str
        && !raw.trim().is_empty()
    {
        return Some(format!("resets: {raw}"));
    }
    None
}

/// Render a terminal progress bar (e.g. `[████████░░░░░░]`).
pub(crate) fn render_progress_bar_spans(
    used_fraction: f32,
    bar_width: usize,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let clamped = used_fraction.clamp(0.0, 1.0);
    let filled_count = (clamped * bar_width as f32).round() as usize;
    let empty_count = bar_width.saturating_sub(filled_count);

    let color = if clamped >= 0.90 {
        theme.err()
    } else if clamped >= 0.70 {
        theme.warn()
    } else {
        theme.ok()
    };

    let filled_str = "█".repeat(filled_count);
    let empty_str = "░".repeat(empty_count);

    vec![
        Span::styled("[", Style::default().fg(theme.dim())),
        Span::styled(filled_str, Style::default().fg(color)),
        Span::styled(empty_str, Style::default().fg(theme.dim())),
        Span::styled("]", Style::default().fg(theme.dim())),
    ]
}

/// Render the detail body rows for one connection (configuration + caller identity + models + provider usage).
pub(crate) fn connection_detail_body(
    detail: &nuo_wire::ConnectionDetail,
    models_expanded: bool,
    spinner_phase: usize,
    theme: &Theme,
) -> Vec<SelectableRow> {
    let label = Style::default().fg(theme.dim());
    let value = Style::default().fg(theme.fg());
    let header_style = Style::default()
        .fg(theme.primary)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(theme.muted());
    let highlight = Style::default().fg(theme.primary);
    let warning = Style::default().fg(theme.warning);

    let kv = |k: &str, v: &str| {
        SelectableRow::styled(v.to_string(), value)
            .with_prefix(RowSegment::styled(format!("{k:<16}"), label))
    };

    let provider_display = if !detail.provider_label.is_empty() {
        detail.provider_label.as_str()
    } else {
        nuo_wire::model_providers::model_provider_label(&detail.provider)
    };

    let mut rows: Vec<SelectableRow> = vec![
        SelectableRow::styled("Configuration", header_style),
        kv("Name", &detail.name),
        kv("Provider", provider_display),
        kv("Protocol", &detail.protocol),
        kv("Base URL", &detail.base_url),
        kv("Auth Type", &detail.auth_type),
    ];
    if let Some(masked) = &detail.api_key_masked {
        rows.push(
            SelectableRow::from_segments(vec![
                RowSegment::styled(masked.clone(), value),
                RowSegment::styled(format!(" ({})", detail.api_key_source), muted),
            ])
            .with_prefix(RowSegment::styled(format!("{:<16}", "API Key"), label)),
        );
    } else {
        rows.push(kv("Credential", &detail.api_key_source));
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
        rows.push(
            SelectableRow::from_segments(segments).with_prefix(RowSegment::styled(
                format!("{:<16}", "Default Active"),
                label,
            )),
        );
    }

    // Client Profile
    rows.push(SelectableRow::empty());
    rows.push(SelectableRow::styled("Client Profile", header_style));
    rows.push(kv("Preset", detail.client_identity.label()));
    rows.push(kv("User-Agent", &detail.user_agent));
    let client_headers = detail.client_identity.headers();
    if !client_headers.is_empty() {
        rows.push(SelectableRow::styled(
            format!("{:<16}", "Client Headers"),
            label,
        ));
        for (k, v) in client_headers {
            rows.push(
                SelectableRow::from_segments(vec![
                    RowSegment::styled(format!("{k}: "), label),
                    RowSegment::styled(v.to_string(), value),
                ])
                .with_prefix(RowSegment::styled("  • ", label)),
            );
        }
    }

    // Served Models
    rows.push(SelectableRow::empty());
    let models_title = if detail.models.is_empty() {
        "Served Models".to_string()
    } else {
        format!("Served Models ({})", detail.models.len())
    };
    rows.push(SelectableRow::styled(models_title, header_style));
    if detail.models.is_empty() {
        rows.push(
            SelectableRow::styled("(no models configured)", muted)
                .with_prefix(RowSegment::styled("  ", muted)),
        );
    } else if !models_expanded {
        let primary_model = detail
            .active_model
            .as_deref()
            .or_else(|| detail.models.first().map(|s| s.as_str()));
        if let Some(model) = primary_model {
            rows.push(
                SelectableRow::styled(model.to_string(), value.add_modifier(Modifier::BOLD))
                    .with_prefix(RowSegment::styled("  - ", label)),
            );
        }
        if detail.models.len() > 1 {
            rows.push(
                SelectableRow::from_segments(vec![
                    RowSegment::styled("▸ ", Style::default().fg(theme.primary)),
                    RowSegment::styled(
                        format!(
                            "show all {} models (press Enter to expand)",
                            detail.models.len()
                        ),
                        muted,
                    ),
                ])
                .with_prefix(RowSegment::styled("  ", label)),
            );
        }
    } else {
        for model in &detail.models {
            let is_active = detail.active_model.as_deref() == Some(model.as_str());
            let model_style = if is_active {
                value.add_modifier(Modifier::BOLD)
            } else {
                value
            };
            rows.push(
                SelectableRow::styled(model.clone(), model_style)
                    .with_prefix(RowSegment::styled("  - ", label)),
            );
        }
        if detail.models.len() > 1 {
            rows.push(
                SelectableRow::from_segments(vec![
                    RowSegment::styled("▾ ", Style::default().fg(theme.primary)),
                    RowSegment::styled("collapse models (press Enter)", muted),
                ])
                .with_prefix(RowSegment::styled("  ", label)),
            );
        }
    }

    // Provider Usage & Quota
    rows.push(SelectableRow::empty());
    let mut quota_header_segments =
        vec![RowSegment::styled("Provider Usage & Quota", header_style)];
    if let nuo_wire::ConnectionUsageState::Available(usage) = &detail.usage
        && let Some(plan) = &usage.plan
        && plan.len() <= 40
        && !plan.contains('\n')
    {
        quota_header_segments.push(RowSegment::styled("  ", Style::default()));
        quota_header_segments.push(RowSegment::styled(
            format!("[ {plan} ]"),
            Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
        ));
    }
    rows.push(SelectableRow::from_segments(quota_header_segments));

    match &detail.usage {
        nuo_wire::ConnectionUsageState::Available(usage) => {
            let mut rendered_quota = false;

            if let Some(quota_data) = &usage.quota {
                let mut quota_lines = Vec::new();
                match quota_data {
                    nuo_wire::ProviderQuotaData::Periodic(periodic) => {
                        rendered_quota = true;
                        render_periodic_quota_buckets(
                            &periodic.buckets,
                            &mut quota_lines,
                            value,
                            label,
                            muted,
                            highlight,
                            theme,
                        );
                    }
                    nuo_wire::ProviderQuotaData::Balance(balance) => {
                        rendered_quota = true;
                        render_balance_quota_block(
                            balance,
                            &mut quota_lines,
                            label,
                            value,
                            highlight,
                            theme,
                        );
                    }
                    nuo_wire::ProviderQuotaData::Composite {
                        balance,
                        periodic,
                        rate_limits,
                    } => {
                        rendered_quota = true;
                        if let Some(bal) = balance {
                            render_balance_quota_block(
                                bal,
                                &mut quota_lines,
                                label,
                                value,
                                highlight,
                                theme,
                            );
                        }
                        if let Some(per) = periodic {
                            render_periodic_quota_buckets(
                                &per.buckets,
                                &mut quota_lines,
                                value,
                                label,
                                muted,
                                highlight,
                                theme,
                            );
                        }
                        for rl in rate_limits {
                            quota_lines.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(format!("{:<16}", "Rate Limit"), label),
                                Span::styled(
                                    format!("{} req / {}", rl.requests, rl.interval),
                                    value,
                                ),
                            ]));
                        }
                    }
                }
                rows.extend(quota_lines.into_iter().map(SelectableRow::from_line));
            }

            if !rendered_quota {
                if let Some(bal) = &usage.primary_balance {
                    rows.push(
                        SelectableRow::styled(bal.clone(), highlight.add_modifier(Modifier::BOLD))
                            .with_prefix(RowSegment::styled(
                                format!("  {:<16}", "Primary Balance"),
                                label,
                            )),
                    );
                }
                for metric in &usage.metrics {
                    let val = match &metric.unit {
                        Some(u) => format!("{} {}", metric.value, u),
                        None => metric.value.clone(),
                    };
                    rows.push(
                        SelectableRow::styled(val, value).with_prefix(RowSegment::styled(
                            format!("  {:<16}", metric.label),
                            label,
                        )),
                    );
                }
            }

            if let Some(desc) = &usage.description {
                rows.push(SelectableRow::empty());
                rows.push(
                    SelectableRow::styled(desc.clone(), muted)
                        .with_prefix(RowSegment::styled("  ", muted)),
                );
            }
        }
        nuo_wire::ConnectionUsageState::Unsupported => {
            rows.push(
                SelectableRow::styled(
                    "Usage and quota query is not supported for this provider endpoint.",
                    muted,
                )
                .with_prefix(RowSegment::styled("  ", muted)),
            );
        }
        nuo_wire::ConnectionUsageState::Error(err) => {
            rows.push(
                SelectableRow::from_segments(vec![
                    RowSegment::styled("⚠ Usage query failed: ", warning),
                    RowSegment::styled(err.clone(), value),
                ])
                .with_prefix(RowSegment::styled("  ", muted)),
            );
        }
        nuo_wire::ConnectionUsageState::Fetching => {
            let spin = theme.glyphs.spinner_frame(spinner_phase);
            rows.push(
                SelectableRow::from_segments(vec![
                    RowSegment::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                    RowSegment::styled("Querying upstream provider quota & balance…", muted),
                ])
                .with_prefix(RowSegment::styled("  ", muted)),
            );
        }
    }

    rows
}

pub(crate) fn render_balance_quota_block(
    balance: &nuo_wire::BalanceQuota,
    lines: &mut Vec<Line<'static>>,
    label: Style,
    value: Style,
    highlight: Style,
    theme: &Theme,
) {
    if let (Some(consumed), Some(limit)) = (balance.consumed_amount, balance.credit_limit)
        && limit > 0.0
    {
        let frac = (consumed / limit) as f32;
        let pct = (frac * 100.0).round() as u32;
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "Credit Limit Consumption",
                value.add_modifier(Modifier::BOLD),
            ),
        ]));
        let mut bar_spans = vec![Span::raw("    ")];
        bar_spans.extend(render_progress_bar_spans(frac, 20, theme));
        bar_spans.push(Span::styled(
            format!("  ${:.2} / ${:.2} ({pct}% used)", consumed, limit),
            value,
        ));
        lines.push(Line::from(bar_spans));
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
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{:<16}", "Total Balance"), label),
            Span::styled(total_str, highlight.add_modifier(Modifier::BOLD)),
        ]));
    } else if let Some(prim) = &balance.display_primary {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{:<16}", "Balance"), label),
            Span::styled(prim.clone(), highlight.add_modifier(Modifier::BOLD)),
        ]));
    }

    if let Some(cash) = balance.cash_balance {
        let sym = if balance.currency == "CNY" {
            "¥"
        } else if balance.currency == "USD" {
            "$"
        } else {
            ""
        };
        lines.push(Line::from(vec![
            Span::raw("    ├─ Recharge:     "),
            Span::styled(format!("{sym}{:.2}", cash), value),
        ]));
    }
    if let Some(voucher) = balance.voucher_balance {
        let sym = if balance.currency == "CNY" {
            "¥"
        } else if balance.currency == "USD" {
            "$"
        } else {
            ""
        };
        lines.push(Line::from(vec![
            Span::raw("    └─ Voucher:      "),
            Span::styled(format!("{sym}{:.2}", voucher), value),
        ]));
    }
}

pub(crate) fn render_periodic_quota_buckets(
    buckets: &[nuo_wire::QuotaWindowBucket],
    lines: &mut Vec<Line<'static>>,
    value: Style,
    _label: Style,
    muted: Style,
    highlight: Style,
    theme: &Theme,
) {
    if buckets.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("Active (no specific bucket limits reported)", muted),
        ]));
        return;
    }

    let mut last_group: Option<&str> = None;
    for bucket in buckets {
        let current_group = bucket.group.as_deref();
        if current_group != last_group {
            if let Some(grp) = current_group {
                if last_group.is_some() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("▸ {grp}"), highlight.add_modifier(Modifier::BOLD)),
                ]));
            }
            last_group = current_group;
        }

        let has_group = bucket.group.is_some();
        let title_indent = if has_group { "    " } else { "  " };
        let bar_indent = if has_group { "      " } else { "    " };

        let window_tag = bucket
            .window
            .map(|w| format!(" · {}", w.label()))
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::raw(title_indent),
            Span::styled(
                format!("{}{}", bucket.label, window_tag),
                value.add_modifier(Modifier::BOLD),
            ),
        ]));

        let pct_used = (bucket.used_fraction * 100.0).round() as u32;
        let mut bar_spans = vec![Span::raw(bar_indent)];
        bar_spans.extend(render_progress_bar_spans(bucket.used_fraction, 20, theme));
        bar_spans.push(Span::styled(format!("  {pct_used}% used"), value));
        lines.push(Line::from(bar_spans));

        if let Some(reset_str) =
            format_reset_countdown(bucket.reset_at_ms, bucket.reset_time_str.as_deref())
        {
            lines.push(Line::from(vec![
                Span::raw(bar_indent),
                Span::styled(reset_str, muted),
            ]));
        }
    }
}
