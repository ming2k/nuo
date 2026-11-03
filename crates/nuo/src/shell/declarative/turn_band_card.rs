//! Pure Declarative TurnBand Component (ADR-0072 / ADR-0073 / ADR-0112 / ADR-0137 / ADR-0146).
//!
//! Provides isolated inspection for transcript turn boundaries (`› turn N  model [effort]  HH:MM`),
//! validating two-tone metadata strip rendering and spatial R2 column separation.

use super::{
    Component,
    story::{Story, StoryEventOutcome, StorybookComponent},
};
use nuo_ui_primitives::components::meta_strip::{MetaStrip, MetaTone};
use nuo_ui_primitives::design::{AI_OUTPUT_LEAD_GLYPH, JOIN_ENUMERATE_COLS};
use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Line, Paragraph, Rect, Span, Style};

/// Presentation properties for a transcript turn band header and turn body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnBandCardProps {
    pub round: u32,
    pub turn: u64,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub timestamp_label: Option<String>,
    pub mock_body: Vec<String>,
}

/// Volatile interactive state for turn band inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnBandCardState {
    pub elapsed_secs: u64,
}

pub struct TurnBandCard;

impl Component for TurnBandCard {
    type Props = TurnBandCardProps;
    type State = TurnBandCardState;
    type Msg = ();
    type Cmd = ();

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        if area.width < 20 || area.height < 4 {
            return;
        }

        let header_rect = Rect::new(area.x, area.y, area.width, 1);
        let body_rect = Rect::new(
            area.x,
            area.y.saturating_add(2),
            area.width,
            area.height.saturating_sub(2),
        );

        // 1. Draw TurnBand metadata header strip
        let lead = format!("{AI_OUTPUT_LEAD_GLYPH} ");
        let mut strip = MetaStrip::new()
            .separator(" ".repeat(JOIN_ENUMERATE_COLS))
            .lead(lead, MetaTone::Accent)
            .anchor(format!("turn {}", props.turn));

        let model_info = match (&props.model, &props.effort) {
            (Some(m), Some(e)) => Some(format!("{m} {e}")),
            (Some(m), None) => Some(m.clone()),
            (None, Some(e)) => Some(e.clone()),
            (None, None) => None,
        };

        if let Some(info) = model_info {
            strip = strip.chip(info, MetaTone::Muted);
        }

        if let Some(time) = &props.timestamp_label {
            strip = strip.trailing_detail(time.clone());
        } else if state.elapsed_secs > 0 {
            strip = strip.trailing_detail(format!("+{}s", state.elapsed_secs));
        }

        strip.render(frame, header_rect, theme);

        // 2. Draw mock turn content beneath header
        let lines: Vec<Line<'static>> = props
            .mock_body
            .iter()
            .map(|s| {
                Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(theme.dim_fg)),
                    Span::styled(s.clone(), Style::default().fg(theme.text)),
                ])
            })
            .collect();

        frame.render_widget(&mut Paragraph::new(lines), body_rect);
    }

    fn update(
        _props: &Self::Props,
        _state: &mut Self::State,
        _msg: Self::Msg,
    ) -> Option<Self::Cmd> {
        None
    }
}

impl StorybookComponent for TurnBandCard {
    fn category() -> &'static str {
        "Transcript"
    }

    fn name() -> &'static str {
        "TurnBand"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        vec![
            Story::new(
                "Standard Model Turn (Sonnet)",
                "Standard ReAct turn header showing model identity, turn counter, and send timestamp",
                TurnBandCardProps {
                    round: 1,
                    turn: 2,
                    model: Some("claude-3-7-sonnet".to_string()),
                    effort: None,
                    timestamp_label: Some("14:32:05".to_string()),
                    mock_body: vec![
                        "Analyzing workspace dependency topology...".to_string(),
                        "Found 12 crates requiring migration to two-tier presentation.".to_string(),
                    ],
                },
                TurnBandCardState::default(),
            ),
            Story::new(
                "Deep Reasoning Channel (GLM-5.3 xhigh)",
                "Turn header combining model identity with explicit cognitive reasoning effort badge",
                TurnBandCardProps {
                    round: 2,
                    turn: 4,
                    model: Some("glm-5.3".to_string()),
                    effort: Some("xhigh".to_string()),
                    timestamp_label: Some("14:35:12".to_string()),
                    mock_body: vec![
                        "Thinking trace: Verifying invariant [INV-TUI-TIER-02] compliance..."
                            .to_string(),
                        "Execution plan confirmed: no duplicate presenter logic detected."
                            .to_string(),
                    ],
                },
                TurnBandCardState::default(),
            ),
            Story::new(
                "Multi-Agent Delegated Turn (Subagent)",
                "Turn boundary generated during subagent aside execution with elapsed counter",
                TurnBandCardProps {
                    round: 3,
                    turn: 7,
                    model: Some("gpt-4o-mini".to_string()),
                    effort: None,
                    timestamp_label: None,
                    mock_body: vec![
                        "[subagent:code_reviewer] Running test suite against git staging tree..."
                            .to_string(),
                        "All unit tests and golden snapshots verified successfully.".to_string(),
                    ],
                },
                TurnBandCardState { elapsed_secs: 5 },
            ),
        ]
    }

    fn tick(_props: &mut Self::Props, state: &mut Self::State) -> StoryEventOutcome {
        state.elapsed_secs = state.elapsed_secs.saturating_add(1);
        StoryEventOutcome::Consumed
    }
}
