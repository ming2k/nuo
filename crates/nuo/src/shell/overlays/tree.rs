//! Interactive Thread Tree (DAG) visualization and navigation overlay.

use nuo_protocol::{ThreadEntry, ThreadEntryKind, ThreadTree};
use nuotc::{
    Frame, Rect, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{placeholder, relative_time_at, truncate_ellipsis};
use crate::shell::render::Theme;
use nuo_ui_primitives::components::list::{
    SelectableListPage, draw_selectable_list_page, row_style,
};
use nuo_ui_primitives::components::modal::{ModalHeader, modal_body_width};
use nuo_ui_primitives::primitives::{ContentModalSpec, FooterHint, keyvocab};

/// A flattened display row for the visual tree representation.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TreeDisplayRow {
    pub entry_id: String,
    pub parent_id: Option<String>,
    pub depth: usize,
    pub is_active: bool,
    pub is_last_child: bool,
    pub label: String,
    pub detail: String,
    pub timestamp: u64,
    pub kind_badge: &'static str,
}

/// Flatten a ThreadTree into display rows in depth-first hierarchical order.
pub fn flatten_tree(tree: &ThreadTree) -> Vec<TreeDisplayRow> {
    if tree.entries.is_empty() {
        return Vec::new();
    }

    let active_id = tree.active_leaf_id.as_deref().unwrap_or("");
    let mut rows = Vec::new();

    // Find root nodes (entries with no parent)
    let mut roots: Vec<&ThreadEntry> = tree
        .entries
        .values()
        .filter(|e| e.parent_id.is_none())
        .collect();
    roots.sort_by_key(|e| e.timestamp);

    for root in roots {
        traverse_node(tree, root, 0, false, active_id, &mut rows);
    }

    rows
}

fn traverse_node(
    tree: &ThreadTree,
    node: &ThreadEntry,
    depth: usize,
    is_last: bool,
    active_id: &str,
    rows: &mut Vec<TreeDisplayRow>,
) {
    let (kind_badge, label, detail) = match &node.kind {
        ThreadEntryKind::Message { message } => {
            let badge = match message.role {
                nuo_tool::Role::User => "USER",
                nuo_tool::Role::Assistant => "ASST",
                nuo_tool::Role::Tool => "TOOL",
                nuo_tool::Role::System => "SYS",
            };
            let preview = message.content.lines().next().unwrap_or("").to_string();
            (badge, preview, format!("{} chars", message.content.len()))
        }
        ThreadEntryKind::Compaction {
            summary,
            tokens_before,
            ..
        } => (
            "COMPACT",
            summary.lines().next().unwrap_or("Compaction").to_string(),
            format!("Tokens before: {}", tokens_before),
        ),
        ThreadEntryKind::BranchSummary {
            summary, from_id, ..
        } => (
            "BRANCH_SUM",
            format!("From {}", &from_id[..6.min(from_id.len())]),
            summary.lines().next().unwrap_or("").to_string(),
        ),
        ThreadEntryKind::Custom {
            custom_type,
            content,
            ..
        } => ("CUSTOM", custom_type.clone(), content.clone()),
    };

    rows.push(TreeDisplayRow {
        entry_id: node.id.clone(),
        parent_id: node.parent_id.clone(),
        depth,
        is_active: node.id == active_id,
        is_last_child: is_last,
        label,
        detail,
        timestamp: node.timestamp,
        kind_badge,
    });

    let mut children = tree.children_of(&node.id);
    children.sort_by_key(|e| e.timestamp);
    let count = children.len();
    for (i, child) in children.into_iter().enumerate() {
        traverse_node(tree, child, depth + 1, i + 1 == count, active_id, rows);
    }
}

/// Draw the interactive session DAG tree modal.
pub fn draw_tree_modal(
    frame: &mut Frame,
    tree: &ThreadTree,
    modal_index: usize,
    scroll: &mut usize,
    follow_selection: bool,
    theme: &Theme,
) -> Rect {
    let body_width = modal_body_width(frame, ContentModalSpec::TOOLS);
    let rows = flatten_tree(tree);

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    if rows.is_empty() {
        body.push(placeholder(
            "No thread tree nodes available yet.",
            true,
            theme.muted(),
        ));
    } else {
        const GUTTER_W: usize = 2;
        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        for (i, row) in rows.iter().enumerate() {
            let is_sel = i == modal_index;
            let style = row_style(is_sel, theme);

            if is_sel {
                selected_line = Some(body.len());
            }

            let is_ascii = theme.glyphs.corner_tl == "+";
            let mut prefix = String::new();
            for _ in 0..row.depth {
                prefix.push_str(if is_ascii { "  |" } else { "  │" });
            }
            if row.depth > 0 {
                if row.is_last_child {
                    prefix.push_str(if is_ascii { "  `- " } else { "  └─ " });
                } else {
                    prefix.push_str(if is_ascii { "  +- " } else { "  ├─ " });
                }
            }

            let marker = if !theme.elevation.is_chromatic() {
                if row.is_active { "* " } else { "  " }
            } else if row.is_active {
                "● "
            } else {
                "○ "
            };
            let glyph_color = if row.is_active {
                theme.brand()
            } else {
                style.dim
            };
            let rel_time = relative_time_at(row.timestamp, now_ts);
            let badge = format!("[{}] ", row.kind_badge);

            let label_budget = body_width
                .saturating_sub(
                    GUTTER_W
                        + prefix.width()
                        + marker.width()
                        + badge.width()
                        + rel_time.width()
                        + 16,
                )
                .max(10);
            let label = truncate_ellipsis(&row.label, label_budget);

            let mut gutter_st = Style::default().bg(style.bg);
            let mut dim_st = Style::default().bg(style.bg).fg(style.dim);
            let mut glyph_st = Style::default().bg(style.bg).fg(glyph_color);
            let mut brand_st = Style::default().bg(style.bg).fg(theme.brand());
            let mut label_st = Style::default().bg(style.bg).fg(style.fg);
            if is_sel && !theme.elevation.is_chromatic() {
                gutter_st = gutter_st.add_modifier(nuotc::Modifier::REVERSE);
                dim_st = dim_st.add_modifier(nuotc::Modifier::REVERSE);
                glyph_st = glyph_st.add_modifier(nuotc::Modifier::REVERSE | nuotc::Modifier::BOLD);
                brand_st = brand_st.add_modifier(nuotc::Modifier::REVERSE | nuotc::Modifier::BOLD);
                label_st = label_st.add_modifier(nuotc::Modifier::REVERSE | nuotc::Modifier::BOLD);
            }

            body.push(Line::from(vec![
                Span::styled(" ".repeat(GUTTER_W), gutter_st),
                Span::styled(prefix, dim_st),
                Span::styled(marker, glyph_st),
                Span::styled(badge, brand_st),
                Span::styled(format!("{:<w$}  ", label, w = label_budget), label_st),
                Span::styled(rel_time, dim_st),
            ]));
        }
    }

    let has_rows = !rows.is_empty();
    draw_selectable_list_page(
        frame,
        SelectableListPage {
            geometry: ContentModalSpec::TOOLS,
            header: ModalHeader::title("Session Tree"),
            lines: body,
            scroll,
            selected_line,
            follow_selection,
            has_items: has_rows,
            item_footer_hints: &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
                FooterHint::primary(
                    crate::shell::keymap::Key::ENTER.display(),
                    "checkout branch",
                ),
                FooterHint::always(crate::shell::keymap::Key::ESC.display(), "close"),
            ],
            empty_footer_hints: &[FooterHint::always(
                crate::shell::keymap::Key::ESC.display(),
                "close",
            )],
            extra_footer_hints: &[],
        },
        theme,
    )
}
