//! Contextual In-Dialog Key Reference Overlay.
//!
//! Opened with `?` inside any dialog (when no text field claims input).
//! Conforms to the active dialog's design, hierarchy, and layout:
//! - Geometry matches the parent dialog (FixedModalSpec or ContentModalSpec).
//! - Header uses breadcrumb hierarchy: `{Dialog} › Keys`.
//! - Body lists registry-derived verbs scoped to the dialog (`Scope::Dialog(dialog)`),
//!   followed by common navigation verbs.
//! - Footer provides standard navigation hints (`↑↓ scroll`, `?/Esc close`).

use nuotc::{
    Frame, Modifier, Rect, Span, {Line, Style},
};

use crate::components::footer::{FooterHint, render_modal_footer};
use crate::components::keycap::keycap_style;
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::elevation::{modal_frame, modal_header_parts};
use crate::keymap::{AppContext, Availability, COMMAND_REGISTRY, Scope};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    ContentModalSpec, FixedModalSpec, breadcrumb_parts, content_modal_area, modal_area,
    modal_chrome_rows,
};
use crate::render::Theme;
use crate::surfaces::DialogKind;

/// Compute the modal rect matching the parent dialog's layout specification.
fn dialog_keys_area(frame: &Frame, parent: DialogKind, desired_content_rows: u16) -> Rect {
    match parent {
        DialogKind::Sessions => modal_area(frame, FixedModalSpec::SESSIONS),
        DialogKind::Models | DialogKind::Connections | DialogKind::Switcher => {
            modal_area(frame, FixedModalSpec::PROVIDER)
        }
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
        DialogKind::UsageStats => {
            let spec = ContentModalSpec::USAGE_STATS;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::Telemetry => {
            let spec = ContentModalSpec::TELEMETRY;
            let desired = desired_content_rows + modal_chrome_rows(spec.modal_spec());
            content_modal_area(frame, spec, desired)
        }
        DialogKind::Asides => {
            let spec = ContentModalSpec::BTW;
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
pub fn draw_dialog_keys(
    frame: &mut Frame,
    parent: DialogKind,
    scroll: &mut usize,
    ctx: &AppContext,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let key_fmt = |k: &str| Span::styled(format!("{:<16}", k), keycap_style(theme));
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

    // 1. Scoped Commands from Registry
    let scoped_cmds: Vec<_> = COMMAND_REGISTRY
        .iter()
        .filter(|c| c.scope == Scope::Dialog(parent))
        .collect();

    if !scoped_cmds.is_empty() {
        rows.push(SelectableRow::from_line(Line::from(section_fmt(&format!(
            "{} Actions",
            parent.label()
        )))));
        for cmd in scoped_cmds {
            let key_str = if !cmd.bindings.is_empty() {
                cmd.bindings[0].display()
            } else {
                cmd.hint
            };
            let desc = match (cmd.availability)(ctx) {
                Availability::Available => cmd.description.to_string(),
                Availability::Unavailable(reason) => format!("{} ({})", cmd.description, reason),
            };
            rows.push(SelectableRow::from_line(row_fmt(key_str, &desc)));
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
        "? / Esc",
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
                FooterHint::always("?/Esc", "back"),
            ],
            theme,
        );
    }

    area
}
