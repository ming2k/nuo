//! Web settings: one compiled provider for search and one for page reading.

use nuo_wire::{
    WebCredentialRequirement, WebCredentialStatus, WebProviderAxis, WebSearchConfigView,
};
use nuotc::{Frame, Line, Modifier, Rect, Span, Style};

use super::{ScrollableRects, SettingsProps, render_scrollable_indexed};

pub fn build_websearch_provider_dropdown(
    current: &str,
    ws: Option<&WebSearchConfigView>,
) -> crate::components::dropdown::DropdownState<String> {
    build_provider_dropdown(current, ws, WebProviderAxis::Search)
}

pub fn build_websearch_reader_dropdown(
    current: &str,
    ws: Option<&WebSearchConfigView>,
) -> crate::components::dropdown::DropdownState<String> {
    build_provider_dropdown(current, ws, WebProviderAxis::Reader)
}

fn build_provider_dropdown(
    current: &str,
    ws: Option<&WebSearchConfigView>,
    axis: WebProviderAxis,
) -> crate::components::dropdown::DropdownState<String> {
    use crate::components::dropdown::{DropdownIndicator, DropdownItem, DropdownState};

    let mut items: Vec<_> = ws
        .into_iter()
        .flat_map(|view| view.capabilities.iter())
        .filter(|capability| capability.axis == axis)
        .map(|capability| {
            let item = DropdownItem::new(
                capability.id.clone(),
                capability.display_name.clone(),
                capability.id.clone(),
            )
            .with_description(capability.description.clone());
            let active_id = ws.map(|view| match axis {
                WebProviderAxis::Search => view.provider.id(),
                WebProviderAxis::Reader => view.reader.id(),
            });
            if active_id != Some(capability.id.as_str()) {
                // The view intentionally exposes readiness only for the active
                // provider. Do not fabricate a status for dormant choices.
                return item;
            }
            let readiness = match axis {
                WebProviderAxis::Search => ws.map(|view| view.search_credential),
                WebProviderAxis::Reader => ws.map(|view| view.reader_credential),
            };
            let missing = readiness == Some(WebCredentialStatus::RequiredMissing)
                || (capability.id == "searxng"
                    && ws.and_then(|view| view.searxng_url.as_deref()).is_none());
            item.with_indicator(if missing {
                DropdownIndicator::Warning
            } else {
                DropdownIndicator::Ready
            })
        })
        .collect();
    items.push(
        DropdownItem::new("disabled", "Disabled", "disabled".to_string())
            .with_description(match axis {
                WebProviderAxis::Search => "Disable web search",
                WebProviderAxis::Reader => "Disable page reading",
            })
            .with_indicator(DropdownIndicator::Inactive),
    );

    let title = match axis {
        WebProviderAxis::Search => "Select Search Provider",
        WebProviderAxis::Reader => "Select Reader Provider",
    };
    let context = match axis {
        WebProviderAxis::Search => "websearch_provider",
        WebProviderAxis::Reader => "websearch_reader",
    };
    let mut state = DropdownState::new(Some(title), items).with_context(context);
    state.select_by_id(current);
    state
}

pub fn search_item_count(ws: Option<&WebSearchConfigView>) -> usize {
    2 + usize::from(setup_capability(ws, WebProviderAxis::Search).is_some())
}

pub fn reader_item_count(ws: Option<&WebSearchConfigView>) -> usize {
    2 + usize::from(setup_capability(ws, WebProviderAxis::Reader).is_some())
}

fn setup_capability(
    ws: Option<&WebSearchConfigView>,
    axis: WebProviderAxis,
) -> Option<&nuo_wire::WebProviderCapability> {
    let ws = ws?;
    let id = match axis {
        WebProviderAxis::Search => ws.provider.id(),
        WebProviderAxis::Reader => ws.reader.id(),
    };
    ws.capabilities.iter().find(|capability| {
        capability.axis == axis
            && capability.id == id
            && (capability.credential != WebCredentialRequirement::None
                || capability.endpoint == nuo_wire::WebEndpointRequirement::UserSupplied)
    })
}

pub(super) fn draw_search_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut SettingsProps<'_>,
    focused: bool,
) -> ScrollableRects {
    draw_web_detail(frame, body, props, focused, WebProviderAxis::Search)
}

pub(super) fn draw_reader_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut SettingsProps<'_>,
    focused: bool,
) -> ScrollableRects {
    draw_web_detail(frame, body, props, focused, WebProviderAxis::Reader)
}

fn draw_web_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut SettingsProps<'_>,
    focused: bool,
    axis: WebProviderAxis,
) -> ScrollableRects {
    let Some(ws) = props.websearch else {
        return render_scrollable_indexed(
            frame,
            body,
            vec![Line::from(Span::styled(
                "Loading web configuration…",
                Style::default().fg(props.theme.muted()),
            ))],
            props.detail_scroll,
            None,
            &[],
            props.theme,
        );
    };

    let (provider_id, credential) = match axis {
        WebProviderAxis::Search => (ws.provider.id(), ws.search_credential),
        WebProviderAxis::Reader => (ws.reader.id(), ws.reader_credential),
    };
    let capability = ws
        .capabilities
        .iter()
        .find(|capability| capability.axis == axis && capability.id == provider_id);
    let display_name = capability
        .map(|capability| capability.display_name.as_str())
        .unwrap_or("Disabled");
    let ready = provider_id != "disabled"
        && credential != WebCredentialStatus::RequiredMissing
        && !(provider_id == "searxng" && ws.searxng_url.is_none());

    let mut lines = vec![Line::from(vec![
        Span::styled(
            "PROVIDER",
            Style::default()
                .fg(props.theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            match axis {
                WebProviderAxis::Search => "Used by search_web",
                WebProviderAxis::Reader => "Used by read_url",
            },
            Style::default().fg(props.theme.dim()),
        ),
    ])];
    let mut selected_line = None;
    let mut selectable: Vec<(usize, usize)> = Vec::new();
    push_row(
        &mut lines,
        &mut selectable,
        &mut selected_line,
        0,
        props.detail_index,
        focused,
        "Provider",
        display_name,
        if provider_id == "disabled" {
            "Disabled"
        } else if ready {
            "Active"
        } else {
            "Needs setup"
        },
        "Enter to select a provider",
        props,
    );
    push_row(
        &mut lines,
        &mut selectable,
        &mut selected_line,
        1,
        props.detail_index,
        focused,
        "Timeout",
        &format!("{} seconds", ws.timeout_secs),
        "Shared",
        "Enter to increase by 5 seconds",
        props,
    );

    if let Some(capability) = setup_capability(Some(ws), axis) {
        if capability.endpoint == nuo_wire::WebEndpointRequirement::UserSupplied {
            push_row(
                &mut lines,
                &mut selectable,
                &mut selected_line,
                2,
                props.detail_index,
                focused,
                "Endpoint",
                ws.searxng_url.as_deref().unwrap_or("Not configured"),
                if ws.searxng_url.is_some() {
                    "Ready"
                } else {
                    "Required"
                },
                "Enter to set the provider endpoint",
                props,
            );
        } else {
            push_row(
                &mut lines,
                &mut selectable,
                &mut selected_line,
                2,
                props.detail_index,
                focused,
                "API token",
                credential_label(credential),
                match capability.credential {
                    WebCredentialRequirement::Optional => "Optional",
                    WebCredentialRequirement::Required => "Required",
                    WebCredentialRequirement::None => "",
                },
                "Enter to set or clear the token",
                props,
            );
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "NETWORK",
            Style::default()
                .fg(props.theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("Direct connection", Style::default().fg(props.theme.dim())),
    ]));

    render_scrollable_indexed(
        frame,
        body,
        lines,
        props.detail_scroll,
        selected_line,
        &selectable,
        props.theme,
    )
}

fn credential_label(status: WebCredentialStatus) -> &'static str {
    match status {
        WebCredentialStatus::NotRequired => "Not required",
        WebCredentialStatus::Environment => "Provided by environment",
        WebCredentialStatus::Stored => "Stored securely",
        WebCredentialStatus::OptionalMissing => "Not configured",
        WebCredentialStatus::RequiredMissing => "Missing",
    }
}

#[allow(clippy::too_many_arguments)]
fn push_row(
    lines: &mut Vec<Line<'static>>,
    selectable: &mut Vec<(usize, usize)>,
    selected_line: &mut Option<usize>,
    index: usize,
    selected_index: usize,
    focused: bool,
    label: &str,
    value: &str,
    badge: &str,
    help: &str,
    props: &SettingsProps<'_>,
) {
    let selected = index == selected_index;
    if selected {
        *selected_line = Some(lines.len());
    }
    selectable.push((index, lines.len()));

    let banded = props.hover_index == Some(index) || (selected && focused);
    let band = if banded {
        props.theme.row_hover_band(&[])
    } else {
        nuotc::Color::Reset
    };

    lines.push(
        Line::from(vec![
            Span::raw("    "),
            Span::styled(
                format!("{label:<18}"),
                Style::default()
                    .fg(props.theme.band_text(
                        band,
                        if selected && focused {
                            props.theme.brand()
                        } else {
                            props.theme.fg()
                        },
                    ))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                value.to_string(),
                Style::default().fg(props.theme.band_text(band, props.theme.fg())),
            ),
            Span::raw("  "),
            Span::styled(
                badge.to_string(),
                Style::default().fg(props.theme.band_text(
                    band,
                    if badge == "Active" || badge == "Ready" {
                        props.theme.ok()
                    } else {
                        props.theme.dim()
                    },
                )),
            ),
        ])
        .style(Style::default().bg(band)),
    );
    if selected && focused {
        lines.push(Line::from(vec![
            Span::raw("       "),
            Span::styled(help.to_string(), Style::default().fg(props.theme.muted())),
        ]));
    }
}
