//! Contextual In-Dialog Key Reference Overlay.
//!
//! Opened with `?` inside any dialog (when no text field claims input).
//! Conforms to the active dialog's design, hierarchy, and layout:
//! - Geometry matches the parent dialog (FixedModalSpec or ContentModalSpec).
//! - Header uses breadcrumb hierarchy: `{Dialog} › Keys`.
//! - Body lists the dialog's verbs from `palette::verbs` — the same table the
//!   command palette runs them from — followed by common navigation verbs.
//! - Footer provides standard navigation hints (`↑↓ scroll`, `?/Esc close`).

use graticule::{
    Frame, Modifier, Rect, Span, {Line, Style},
};

use crate::shell::keymap::Availability;
use crate::shell::palette::PaletteContext;
use crate::shell::render::Theme;
use crate::shell::surfaces::DialogKind;
use nuo_ui_primitives::components::footer::{FooterHint, render_modal_footer};
use nuo_ui_primitives::elevation::{modal_frame, modal_header_parts};
use nuo_ui_primitives::primitives::{
    ContentModalSpec, FixedModalSpec, breadcrumb_parts, content_modal_area, modal_area,
    modal_chrome_rows,
};
use nuo_view::components::selectable_body::{SelectableRow, render_selectable_body};
use nuo_view::model::layout::LayoutMap;
use nuo_view::model::selection::SelectionState;

/// Compute the modal rect matching the parent dialog's layout specification.
fn dialog_keys_area(frame: &Frame, parent: DialogKind, desired_content_rows: u16) -> Rect {
    match parent {
        DialogKind::Threads => modal_area(frame, FixedModalSpec::SESSIONS),
        DialogKind::Models => modal_area(frame, FixedModalSpec::MODELS),
        DialogKind::Connections
        | DialogKind::Palette
        | DialogKind::PortShare
        | DialogKind::Help
        | DialogKind::Notices => modal_area(frame, FixedModalSpec::PROVIDER),
        DialogKind::Tools => {
            let spec = ContentModalSpec::TOOLS;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::Mcp => {
            let spec = ContentModalSpec::MCP;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::Skills => {
            let spec = ContentModalSpec::SKILLS;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::Permissions => {
            let spec = ContentModalSpec::PERMISSIONS;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::UsageStats | DialogKind::Quotas => {
            let spec = ContentModalSpec::USAGE_STATS;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::ThreadStats => {
            let spec = ContentModalSpec::THREAD_STATS;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::SessionTrace => {
            let spec = ContentModalSpec::SESSION_TRACE;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::ContextPlan => {
            let spec = ContentModalSpec::CONTEXT_PLAN;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::Queue => {
            let spec = ContentModalSpec::QUEUE;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::SessionTree | DialogKind::HistorySearch => {
            let spec = ContentModalSpec::CUSTOM_PROVIDER;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
    }
}

/// Draw the localized dialog key reference overlay for `parent`.
pub(crate) fn draw_dialog_keys(
    frame: &mut Frame,
    parent: DialogKind,
    scroll: &mut usize,
    ctx: &PaletteContext,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let key_fmt = |k: &str| {
        Span::styled(
            format!("{:<16}", k),
            Style::default()
                .fg(theme.keycap_accent())
                .add_modifier(Modifier::BOLD),
        )
    };
    let desc_fmt = |d: &str| Span::styled(d.to_string(), theme.keycap_label_style());
    let section_fmt = |title: &str| {
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        )
    };
    let row_fmt = |k: &str, d: &str| Line::from(vec![key_fmt(k), desc_fmt(d)]);

    let mut rows: Vec<SelectableRow> = Vec::new();

    // 1. The dialog's own verbs.
    let verbs = crate::shell::palette::verbs::dialog_verbs(parent);
    if !verbs.is_empty() {
        rows.push(SelectableRow::from_line(Line::from(section_fmt(&format!(
            "{} Actions",
            parent.label()
        )))));
        for verb in verbs {
            let key = verb
                .effective_keycap(ctx)
                .map(|key| key.display())
                .unwrap_or("");
            let desc = match verb.requires.check(ctx) {
                Availability::Available => verb.title.to_string(),
                Availability::Unavailable(reason) => format!("{} ({reason})", verb.title),
            };
            rows.push(SelectableRow::from_line(row_fmt(key, &desc)));
        }
        rows.push(SelectableRow::from_line(Line::from("")));
    }

    // 2. Navigation & Common Dialog Keys
    rows.push(SelectableRow::from_line(Line::from(section_fmt(
        "Navigation",
    ))));
    rows.push(SelectableRow::from_line(row_fmt(
        "↑ / ↓",
        "Navigate items / scroll list",
    )));
    rows.push(SelectableRow::from_line(row_fmt(
        "Enter",
        "Select / inspect item",
    )));
    rows.push(SelectableRow::from_line(row_fmt(
        "F1 / Esc",
        "Back to dialog",
    )));

    let area = dialog_keys_area(frame, parent, rows.len() as u16);
    let f = modal_frame(frame, area, theme, true, true);

    let breadcrumbs = breadcrumb_parts(parent.label(), "Keys");
    modal_header_parts(frame, f.header, &breadcrumbs, theme);

    render_selectable_body(
        frame, f.body, &rows, scroll, None, theme, selection, layout_map,
    );

    if let Some(footer) = f.footer {
        render_modal_footer(
            frame,
            footer,
            &[
                FooterHint::navigation("↑↓", "scroll"),
                FooterHint::always("F1/Esc", "back"),
            ],
            theme,
        );
    }

    area
}
