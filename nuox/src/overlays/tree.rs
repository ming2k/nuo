//! Interactive Session Tree (DAG) visualization and navigation overlay.

use nuo_contracts::{SessionEntry, SessionEntryKind, SessionTree};
use nuotc::{
    Frame, Rect, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{placeholder, relative_time_at, truncate_ellipsis};
use crate::components::list::{SelectableListPage, draw_selectable_list_page, row_style};
use crate::components::modal::{ModalHeader, modal_body_width};
use crate::primitives::{ContentModalSpec, FooterHint, keyvocab};
use crate::render::Theme;

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

/// Flatten a SessionTree into display rows in depth-first hierarchical order.
pub fn flatten_tree(tree: &SessionTree) -> Vec<TreeDisplayRow> {
    if tree.entries.is_empty() {
        return Vec::new();
    }

    let active_id = tree.active_leaf_id.as_deref().unwrap_or("");
    let mut rows = Vec::new();

    // Find root nodes (entries with no parent)
    let mut roots: Vec<&SessionEntry> = tree
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
    tree: &SessionTree,
    node: &SessionEntry,
    depth: usize,
    is_last: bool,
    active_id: &str,
    rows: &mut Vec<TreeDisplayRow>,
) {
    let (kind_badge, label, detail) = match &node.kind {
        SessionEntryKind::Message { message } => {
            let badge = match message.role {
                nuo_contracts::Role::User => "USER",
                nuo_contracts::Role::Assistant => "ASST",
                nuo_contracts::Role::Tool => "TOOL",
                nuo_contracts::Role::System => "SYS",
            };
            let preview = message.content.lines().next().unwrap_or("").to_string();
            (badge, preview, format!("{} chars", message.content.len()))
        }
        SessionEntryKind::Compaction {
            summary,
            tokens_before,
            ..
        } => (
            "COMPACT",
            summary.lines().next().unwrap_or("Compaction").to_string(),
            format!("Tokens before: {}", tokens_before),
        ),
        SessionEntryKind::BranchSummary {
            summary, from_id, ..
        } => (
            "BRANCH_SUM",
            format!("From {}", &from_id[..6.min(from_id.len())]),
            summary.lines().next().unwrap_or("").to_string(),
        ),
        SessionEntryKind::Custom {
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
    tree: &SessionTree,
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
            "No conversation tree nodes available yet.",
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

            let mut prefix = String::new();
            for _ in 0..row.depth {
                prefix.push_str("  │");
            }
            if row.depth > 0 {
                if row.is_last_child {
                    prefix.push_str("  └─ ");
                } else {
                    prefix.push_str("  ├─ ");
                }
            }

            let marker = if row.is_active { "● " } else { "○ " };
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

            body.push(Line::from(vec![
                Span::styled(" ".repeat(GUTTER_W), Style::default().bg(style.bg)),
                Span::styled(prefix, Style::default().bg(style.bg).fg(style.dim)),
                Span::styled(marker, Style::default().bg(style.bg).fg(glyph_color)),
                Span::styled(badge, Style::default().bg(style.bg).fg(theme.brand())),
                Span::styled(
                    format!("{:<w$}  ", label, w = label_budget),
                    Style::default().bg(style.bg).fg(style.fg),
                ),
                Span::styled(rel_time, Style::default().bg(style.bg).fg(style.dim)),
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
                FooterHint::key_primary(crate::keymap::Key::ENTER, "checkout branch"),
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
            ],
            empty_footer_hints: &[FooterHint::key_always(crate::keymap::Key::ESC, "close")],
            extra_footer_hints: &[],
        },
        theme,
    )
}
