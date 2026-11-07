//! Pure Declarative ModelRow component (ADR-0073 / ADR-0146 / ADR-0152).
//!
//! The offline inspection surface for the universal multi-column row: one
//! budget, one direction, semantic overflow, and focus reveal. It exists so the
//! column contract can be *seen* at every width without launching the product
//! and opening a modal — and so a regression in the solver shows up as a story
//! that stops reading as a table, not as a bug report from a narrow terminal.

use super::{
    Component,
    story::{Story, StoryEventOutcome, StorybookComponent},
};
use nuo_ui_primitives::components::columns::{ColumnSpec, Grid, MAX_JUSTIFIED_GAP, column};
use nuo_ui_primitives::components::overflow::{
    MarqueeCadence, Overflow, TextCell, WidthAware, marquee_offset, window_max_offset,
};
use nuo_ui_primitives::components::row::{GROUP_GAP, GUTTER, ListRow, RowGroup};
use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Line, Rect, Style};
use unicode_width::UnicodeWidthStr;

/// One field of a demonstrated row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DemoField {
    pub text: String,
    /// The strategy this field degrades with — the point of the story is that
    /// the choice follows the field's semantics, not the caller's convenience.
    pub overflow: Overflow,
    pub floor: usize,
    pub max_share: usize,
    pub droppable: bool,
}

/// Presentation properties for the column-budget demonstration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRowCardProps {
    pub title: String,
    /// The rows to demonstrate, each a set of fields in display order.
    pub rows: Vec<Vec<DemoField>>,
    /// The body width to solve for. The story suite varies this so the
    /// degradation ladder (truncate → drop → reveal) is visible.
    pub body_width: usize,
    /// The row that owns focus when the story opens. Which row is focused
    /// *afterwards* is interaction state, not presentation data — see
    /// [`ModelRowCardState::focused`].
    pub focused: usize,
}

/// Volatile interactive state: which row owns focus, and the reveal clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelRowCardState {
    /// Milliseconds since the story was instantiated, advanced by `tick`.
    pub elapsed_ms: u64,
    /// The row that owns focus and therefore may reveal. `None` means "use the
    /// story's opening focus", so a fresh state and the authored props agree.
    pub focused: Option<usize>,
}

impl ModelRowCardState {
    /// The focused row, falling back to the story's opening focus.
    fn focus(&self, props: &ModelRowCardProps) -> usize {
        self.focused.unwrap_or(props.focused)
    }
}

/// Messages the story accepts (arrow keys move focus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRowCardMsg {
    FocusNext,
    FocusPrev,
}

pub struct ModelRowCard;

impl ModelRowCard {
    /// The columns a props set resolves to, exposed so the story's own test can
    /// assert the budget rather than only the painted glyphs.
    fn solve(props: &ModelRowCardProps) -> Vec<nuo_ui_primitives::components::columns::Column> {
        let field_count = props.rows.iter().map(Vec::len).max().unwrap_or(0);
        let specs: Vec<ColumnSpec> = (0..field_count)
            .map(|i| {
                let fields: Vec<&DemoField> = props.rows.iter().filter_map(|r| r.get(i)).collect();
                // Demand is the widest *rendered* width in the column, measured
                // through the same overflow implementation the row will paint
                // with — never a byte count, which would over-budget a CJK or
                // multi-byte field.
                let demand = fields.iter().map(|f| f.text.width()).max().unwrap_or(0);
                let Some(first) = fields.first() else {
                    return ColumnSpec::new(0, 0, 0);
                };
                // A droppable field gives way *first* (`priority 2`); an
                // essential one is the last to shrink (`priority 0`).
                let spec = ColumnSpec::new(demand, first.floor, first.max_share)
                    .priority(if first.droppable { 2 } else { 0 });
                if first.droppable {
                    spec.droppable()
                } else {
                    spec
                }
            })
            .collect();
        Grid::new(props.body_width)
            .gutter(GUTTER)
            .gap(GROUP_GAP)
            .max_gap(MAX_JUSTIFIED_GAP)
            .justify(true)
            .solve(&specs)
    }
}

impl Component for ModelRowCard {
    type Props = ModelRowCardProps;
    type State = ModelRowCardState;
    type Msg = ModelRowCardMsg;
    type Cmd = ();

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        if area.width < 24 || area.height < 4 {
            return;
        }
        // The story renders at the width it *declares*, bounded by the canvas.
        // A story suite whose every rung renders at the canvas width would show
        // the same layout four times: the whole point of the ladder is that the
        // 40-column rung looks materially narrower than the 100-column one.
        let width = props.body_width.min(area.width as usize);
        let props = ModelRowCardProps {
            body_width: width,
            ..props.clone()
        };
        let columns = Self::solve(&props);
        let focused = state.focus(&props);
        let style = nuo_ui_primitives::components::options::choice_style(
            nuo_ui_primitives::components::options::ChoiceTone::Filled,
            false,
            theme,
        );

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(nuotc::Span::styled(
            format!(" {}", props.title),
            Style::default().fg(theme.muted()),
        )));
        lines.push(Line::from(nuotc::Span::styled(
            format!(
                " body {}  cols {}  focused {}",
                props.body_width,
                columns.len(),
                focused
            ),
            Style::default().fg(theme.dim()),
        )));

        for (row_index, fields) in props.rows.iter().enumerate() {
            let is_focused = row_index == focused;
            let row_style = if is_focused {
                nuo_ui_primitives::components::options::choice_style(
                    nuo_ui_primitives::components::options::ChoiceTone::Filled,
                    true,
                    theme,
                )
            } else {
                style
            };
            let mut groups: Vec<RowGroup> = Vec::new();
            for (field_index, field) in fields.iter().enumerate() {
                let Some(col) = column(&columns, field_index) else {
                    continue;
                };
                // Reveal belongs to the focused row; every other row truncates
                // by its field's own semantics.
                let overflow = if is_focused
                    && nuo_ui_primitives::components::overflow::fit(
                        &field.text,
                        col.width,
                        field.overflow,
                    )
                    .truncated()
                {
                    Overflow::Window {
                        offset: marquee_offset(
                            window_max_offset(field.text.width(), col.width, 1),
                            state.elapsed_ms,
                            MarqueeCadence::default(),
                        ),
                    }
                } else {
                    field.overflow
                };
                let cell = TextCell::new(
                    field.text.clone(),
                    overflow,
                    Style::default().bg(row_style.bg).fg(row_style.fg),
                )
                .ellipsis(theme.glyphs.ellipsis);
                groups.push(RowGroup::column(col.start).spans(cell.render_width(col.width)));
            }
            let mut row = ListRow::new(row_style, width);
            for group in groups {
                row = row.group(group);
            }
            lines.push(row.finish());
        }

        let mut para = nuotc::Paragraph::new(lines);
        frame.render_widget(
            &mut para,
            Rect {
                width: area.width,
                ..area
            },
        );
    }

    fn update(props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        let count = props.rows.len();
        if count == 0 {
            return None;
        }
        let current = state.focus(props).min(count - 1);
        // Wrap, so ↑ on the first row lands on the last: the reveal follows
        // focus, and a cursor that dead-ends cannot demonstrate that.
        let next = match msg {
            ModelRowCardMsg::FocusNext => (current + 1) % count,
            ModelRowCardMsg::FocusPrev => (current + count - 1) % count,
        };
        state.focused = Some(next);
        None
    }
}

impl StorybookComponent for ModelRowCard {
    fn category() -> &'static str {
        "Overlays"
    }

    fn name() -> &'static str {
        "ModelRow"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        let name = |text: &str| DemoField {
            text: text.to_string(),
            overflow: Overflow::End,
            floor: 16,
            max_share: 34,
            droppable: false,
        };
        let id = |text: &str| DemoField {
            text: text.to_string(),
            overflow: Overflow::Middle,
            floor: 12,
            max_share: 18,
            droppable: true,
        };
        let connection = |text: &str| DemoField {
            text: text.to_string(),
            overflow: Overflow::End,
            floor: 10,
            max_share: 24,
            droppable: false,
        };
        let rows = || {
            vec![
                vec![
                    name("DeepSeek V4.1 Flash Fast (latest)"),
                    id("deepseek-flash-20250219"),
                    connection("cc-main"),
                ],
                vec![
                    name("Gemini 3.8 Flash"),
                    id("gemini-3.8-flash"),
                    connection("ga-main"),
                ],
                vec![
                    name("GPT-5.4"),
                    id("gpt-5.4"),
                    connection("openai-platform-direct"),
                ],
            ]
        };

        vec![
            Story::new(
                "Comfortable Width",
                "All three fields survive and nothing is dropped; the caps absorb the surplus, so only the wire id's tail is elided",
                ModelRowCardProps {
                    title: "COMFORTABLE".to_string(),
                    rows: rows(),
                    body_width: 100,
                    focused: 0,
                },
                ModelRowCardState::default(),
            ),
            Story::new(
                "Contended Width",
                "The share caps engage; the name truncates from its tail while the wire id keeps both ends",
                ModelRowCardProps {
                    title: "CONTENDED".to_string(),
                    rows: rows(),
                    body_width: 64,
                    focused: 1,
                },
                ModelRowCardState::default(),
            ),
            Story::new(
                "Narrow: the optional column drops",
                "The wire id vanishes whole rather than rendering a fragment; the connection keeps its floor",
                ModelRowCardProps {
                    title: "NARROW".to_string(),
                    rows: rows(),
                    body_width: 40,
                    focused: 0,
                },
                ModelRowCardState::default(),
            ),
            Story::new(
                "Focused Row Reveals",
                "The focused row slides its overflowing cells instead of truncating them; other rows stay still",
                ModelRowCardProps {
                    title: "REVEAL".to_string(),
                    rows: rows(),
                    body_width: 56,
                    focused: 0,
                },
                ModelRowCardState {
                    elapsed_ms: 1_500,
                    ..ModelRowCardState::default()
                },
            ),
        ]
    }

    fn map_key_to_msg(
        key: &super::key::Key,
        _props: &Self::Props,
        _state: &Self::State,
    ) -> Option<Self::Msg> {
        match key {
            super::key::Key::Down | super::key::Key::Char('j') => Some(ModelRowCardMsg::FocusNext),
            super::key::Key::Up | super::key::Key::Char('k') => Some(ModelRowCardMsg::FocusPrev),
            _ => None,
        }
    }

    fn tick(_props: &mut Self::Props, state: &mut Self::State) -> StoryEventOutcome {
        // The reveal's clock advances only while the story is on screen, and it
        // is the only thing that moves: this is what `reveal_active` gates in
        // the real product.
        state.elapsed_ms = state.elapsed_ms.saturating_add(100);
        StoryEventOutcome::Consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::declarative::story::instantiate_story;
    use crate::shell::declarative::testing::render_story_styled_snapshot;

    fn props_at(width: usize) -> ModelRowCardProps {
        ModelRowCard::stories()
            .into_iter()
            .map(|s| s.props)
            .find(|p| p.body_width == width)
            .unwrap_or_else(|| ModelRowCardProps {
                title: "T".to_string(),
                rows: vec![vec![
                    DemoField {
                        text: "DeepSeek V4.1 Flash Fast (latest)".to_string(),
                        overflow: Overflow::End,
                        floor: 16,
                        max_share: 34,
                        droppable: false,
                    },
                    DemoField {
                        text: "deepseek-flash-20250219".to_string(),
                        overflow: Overflow::Middle,
                        floor: 12,
                        max_share: 18,
                        droppable: true,
                    },
                    DemoField {
                        text: "cc-main".to_string(),
                        overflow: Overflow::End,
                        floor: 10,
                        max_share: 24,
                        droppable: false,
                    },
                ]],
                body_width: width,
                focused: 0,
            })
    }

    #[test]
    fn the_story_budget_never_overflows_its_body() {
        for width in 20..=140 {
            let columns = ModelRowCard::solve(&props_at(width));
            for c in &columns {
                assert!(
                    c.end() <= width,
                    "width {width}: column {} ends at {}",
                    c.index,
                    c.end()
                );
            }
        }
    }

    #[test]
    fn the_story_declares_the_canonical_degradation_ladder() {
        let stories = ModelRowCard::stories();
        assert_eq!(stories.len(), 4);
        // The ladder: comfortable → contended → narrow (a column drops) →
        // reveal. Each rung is a distinct claim, not a re-render.
        let widths: Vec<usize> = stories.iter().map(|s| s.props.body_width).collect();
        assert_eq!(widths, vec![100, 64, 40, 56]);
    }

    #[test]
    fn arrow_keys_move_focus_and_therefore_which_row_reveals() {
        // Focus is state, not props: the workbench owns an immutable story and a
        // mutable state, so a key that only *looked* wired would leave the story
        // showing one row revealing forever.
        let story = ModelRowCard::stories().pop().expect("reveal story");
        let mut instance = instantiate_story::<ModelRowCard>(story);
        let opening = render_story_styled_snapshot(instance.as_ref(), 80, 12);
        assert_eq!(
            instance.handle_key(&crate::shell::declarative::key::Key::Down),
            StoryEventOutcome::Consumed
        );
        let moved = render_story_styled_snapshot(instance.as_ref(), 80, 12);
        assert_ne!(opening, moved, "moving focus must repaint a different row");
    }

    #[test]
    fn the_reveal_story_advances_its_clock_and_nothing_else() {
        let story = ModelRowCard::stories().pop().expect("story");
        let before = story.initial_state.clone();
        let mut props = story.props.clone();
        let mut state = story.initial_state.clone();
        assert_eq!(
            ModelRowCard::tick(&mut props, &mut state),
            StoryEventOutcome::Consumed
        );
        assert!(state.elapsed_ms > before.elapsed_ms);
        assert_eq!(
            props.body_width, story.props.body_width,
            "props are untouched"
        );
    }

    #[test]
    fn the_comfortable_story_keeps_every_field_alive() {
        // The rung's claim, asserted rather than described: nothing is dropped,
        // so the reader can compare it against the narrow rung where the
        // optional column is gone.
        let columns = ModelRowCard::solve(&props_at(100));
        assert_eq!(columns.len(), 3, "{columns:?}");
        for index in [0, 1, 2] {
            assert!(column(&columns, index).is_some(), "field {index} survives");
        }
    }

    #[test]
    fn a_narrow_story_drops_the_optional_column_instead_of_shredding_it() {
        let columns = ModelRowCard::solve(&props_at(40));
        assert!(
            column(&columns, 1).is_none(),
            "the optional wire id drops at 40 columns: {columns:?}"
        );
        assert!(column(&columns, 0).is_some(), "the name survives");
        assert!(column(&columns, 2).is_some(), "the connection survives");
    }
}
