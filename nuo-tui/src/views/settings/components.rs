//! Components settings panel: interactive component disclosure, expansion
//! defaults, and behavior.
//!
//! Row order is defined **once**, in [`row_for_index`], and both the renderer
//! and the activation dispatcher resolve their rows through it — so the panel
//! and the keys that change it can never disagree about what row *n* means.
//!
//! The tool-backed rows are derived from the tool registry
//! ([`crate::tools::TOOL_COMPONENTS`]), which also owns name → presenter
//! resolution and the per-tool built-in defaults. Declaring a new tool
//! component therefore makes its settings row appear automatically: this
//! module must never re-list tool names, labels, or defaults by hand
//! (ADR-0020).

use nuotc::{Frame, Line, Modifier, Rect, Span, Style};

use super::{SettingsProps, render_scrollable};
use crate::tools::{TOOL_COMPONENTS, ToolComponent};

/// The disclosure badge a row renders in its third column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BadgeStyle {
    /// Open / closed by default (`[ Expanded ]` / `[ Collapsed ]`).
    Expanded,
    /// On / off switch (`[ Enabled ]` / `[ Disabled ]`).
    Enabled,
    /// Density mode (`[ Comfortable ]` / `[ Compact ]`).
    Density,
}

/// Identity of one Components-panel row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComponentRowId {
    /// `[tui.default_expanded] thinking` — model reasoning traces.
    Reasoning,
    /// A declared tool component; every alias shares this one row. Compared by
    /// [`ToolComponent::id`] so the row identity survives an address change.
    Tool(&'static ToolComponent),
    /// Global step density (`[tui] tool_density`).
    Density,
    /// Auto-scroll on expand (`[tui] expand_auto_scroll`).
    AutoScroll,
}

impl PartialEq for ToolComponent {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for ToolComponent {}

/// The behaviour rows trailing the tool component rows. Reasoning leads the
/// panel because it is the one non-tool interactive entry every transcript
/// carries.
const BEHAVIOR_ROWS: [ComponentRowId; 2] = [ComponentRowId::Density, ComponentRowId::AutoScroll];

/// Count of selectable rows in the Components panel. Derived from the tool
/// registry — never a literal (ADR-0020).
pub fn item_count() -> usize {
    1 + TOOL_COMPONENTS.len() + BEHAVIOR_ROWS.len()
}

/// Resolve a detail index to its row identity. This is the single ordering
/// definition the renderer and the dispatcher share.
pub fn row_for_index(index: usize) -> Option<ComponentRowId> {
    if index == 0 {
        return Some(ComponentRowId::Reasoning);
    }
    let tool_index = index - 1;
    if let Some(component) = TOOL_COMPONENTS.get(tool_index) {
        return Some(ComponentRowId::Tool(component));
    }
    BEHAVIOR_ROWS.get(tool_index - TOOL_COMPONENTS.len()).copied()
}

/// The row's panel copy, badge kind, and current state, resolved from live
/// config. `None` for an out-of-range index (the caller renders nothing).
fn row_view(
    id: ComponentRowId,
    tui_config: &crate::config::TuiConfig,
) -> (&'static str, &'static str, BadgeStyle, bool) {
    match id {
        ComponentRowId::Reasoning => (
            "Reasoning Traces (Thinking)",
            "Expand model reasoning traces and chain-of-thought by default",
            BadgeStyle::Expanded,
            crate::config::reasoning_default_expanded(tui_config),
        ),
        ComponentRowId::Tool(component) => (
            component.label,
            component.description,
            BadgeStyle::Expanded,
            crate::config::tool_default_expanded(tui_config, component.primary_name()),
        ),
        ComponentRowId::Density => (
            "Global Step Density",
            "Comfortable mode expands all tool steps; Compact uses per-tool defaults",
            BadgeStyle::Density,
            tui_config.tool_density,
        ),
        ComponentRowId::AutoScroll => (
            "Auto-Scroll on Expand",
            "Automatically follow new turns when expanding collapsible steps",
            BadgeStyle::Enabled,
            tui_config.expand_auto_scroll,
        ),
    }
}

pub(super) fn draw_components_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut SettingsProps<'_>,
    focused: bool,
) -> Option<Rect> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = None;

    for i in 0..item_count() {
        let Some(id) = row_for_index(i) else {
            break;
        };
        let (label, description, badge_style, is_active) = row_view(id, props.tui_config);

        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }

        let mark = if is_active { "●" } else { "○" };
        let row_style = if is_sel && focused {
            Style::default()
                .fg(props.theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(props.theme.fg())
                .add_modifier(Modifier::BOLD)
        };

        let badge = match badge_style {
            BadgeStyle::Expanded => {
                if is_active {
                    "[ Expanded ]"
                } else {
                    "[ Collapsed ]"
                }
            }
            BadgeStyle::Enabled => {
                if is_active {
                    "[ Enabled ]"
                } else {
                    "[ Disabled ]"
                }
            }
            BadgeStyle::Density => {
                if is_active {
                    "[ Comfortable ]"
                } else {
                    "[ Compact ]"
                }
            }
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("{mark} "),
                Style::default().fg(if is_active {
                    props.theme.ok()
                } else if is_sel {
                    props.theme.brand()
                } else {
                    props.theme.dim()
                }),
            ),
            Span::styled(label, row_style),
            Span::raw("  "),
            Span::styled(
                badge,
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(description, Style::default().fg(props.theme.muted())),
        ]));
        lines.push(Line::from(""));
    }

    render_scrollable(
        frame,
        body,
        lines,
        props.detail_scroll,
        selected_line,
        props.theme,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every declared tool component gets exactly one row, in registry order,
    /// and every index in range resolves — the panel and the registry cannot
    /// drift (ADR-0020).
    #[test]
    fn panel_rows_are_derived_from_the_tool_registry() {
        assert_eq!(item_count(), TOOL_COMPONENTS.len() + 3);
        assert_eq!(row_for_index(0), Some(ComponentRowId::Reasoning));
        for (offset, component) in TOOL_COMPONENTS.iter().enumerate() {
            assert_eq!(
                row_for_index(1 + offset),
                Some(ComponentRowId::Tool(component)),
                "row {} must map to component {}",
                1 + offset,
                component.id
            );
        }
        assert_eq!(
            row_for_index(1 + TOOL_COMPONENTS.len()),
            Some(ComponentRowId::Density)
        );
        assert_eq!(
            row_for_index(2 + TOOL_COMPONENTS.len()),
            Some(ComponentRowId::AutoScroll)
        );
        // Out of range resolves to nothing rather than wrapping.
        assert_eq!(row_for_index(item_count()), None);
    }

    /// Activating a component row must move that component's state whichever
    /// name a step was recorded under, and must not touch any other row.
    #[test]
    fn toggling_a_component_row_updates_every_alias_only() {
        for component in TOOL_COMPONENTS {
            let mut config = crate::config::TuiConfig::default();
            let before = crate::config::tool_default_expanded(&config, component.primary_name());
            crate::config::set_component_default_expanded(&mut config, component, !before);
            for name in component.names() {
                assert_eq!(
                    crate::config::tool_default_expanded(&config, name),
                    !before,
                    "{name} must follow the {} row",
                    component.id
                );
            }
            for other in TOOL_COMPONENTS.iter().filter(|o| o.id != component.id) {
                assert_eq!(
                    crate::config::tool_default_expanded(&config, other.primary_name()),
                    other.default_expanded(),
                    "toggling {} must not disturb {}",
                    component.id,
                    other.id
                );
            }
        }
    }

    /// A row with no config entry reports the presenter's built-in default, so
    /// the panel badge always matches what the transcript will actually do.
    #[test]
    fn row_state_falls_back_to_the_declared_default() {
        let config = crate::config::TuiConfig::default();
        for component in TOOL_COMPONENTS {
            let id = ComponentRowId::Tool(component);
            let (_, _, _, is_active) = row_view(id, &config);
            assert_eq!(is_active, component.default_expanded());
        }
    }

    /// The declared policy the panel advertises: `edit_text` / `write_file`
    /// (Diffs) and the shell family (Command) open; every other declared
    /// component — and every undeclared tool — stays closed (ADR-0020 §5).
    #[test]
    fn declared_defaults_open_only_the_action_components() {
        let config = crate::config::TuiConfig::default();
        for name in ["edit_text", "write_file", "execute_command", "run_command", "bash"] {
            assert!(
                crate::config::tool_default_expanded(&config, name),
                "{name} must open by default"
            );
        }
        for component in TOOL_COMPONENTS {
            for name in component.names() {
                let expected = matches!(component.id, "command" | "diff");
                assert_eq!(
                    crate::config::tool_default_expanded(&config, name),
                    expected,
                    "{name} ({}) disagrees with the declared policy",
                    component.id
                );
            }
        }
        for name in ["mcp__any__thing", "brand_new_tool"] {
            assert!(!crate::config::tool_default_expanded(&config, name));
        }
    }

    /// Every row the panel renders must carry enough copy to be understood
    /// without the transcript in view.
    #[test]
    fn every_row_renders_copy_and_a_badge() {
        let config = crate::config::TuiConfig::default();
        for i in 0..item_count() {
            let id = row_for_index(i).expect("index in range must resolve");
            let (label, description, _badge, _active) = row_view(id, &config);
            assert!(!label.trim().is_empty(), "row {i} has no label");
            assert!(
                !description.trim().is_empty(),
                "row {i} ({label}) has no description"
            );
        }
    }
}
