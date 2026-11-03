//! Pure Declarative Model Reasoning Trace Component (ADR-0072 / ADR-0115 / ADR-0142 / ADR-0148).
//!
//! Provides isolated inspection for model thinking traces, multi-paragraph deductive chains,
//! streaming spinners, and collapsible summary headers.

use super::{
    Component,
    story::{Story, StoryEventOutcome, StorybookComponent},
};
use crate::shell::declarative::key::Key;
use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};

/// Presentation properties for the cognitive reasoning trace card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReasoningCardProps {
    pub summary_label: String,
    pub duration_secs: Option<u64>,
    pub is_streaming: bool,
    pub paragraphs: Vec<String>,
}

/// Volatile interactive state for reasoning trace inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReasoningCardState {
    pub expanded: bool,
    pub scroll: usize,
    pub elapsed_secs: u64,
    pub spinner_frame: usize,
}

/// Interactive messages handled by reasoning trace story inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningCardMsg {
    ToggleExpand,
    ScrollUp(usize),
    ScrollDown(usize),
}

pub struct ReasoningCard;

impl Component for ReasoningCard {
    type Props = ReasoningCardProps;
    type State = ReasoningCardState;
    type Msg = ReasoningCardMsg;
    type Cmd = ();

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        if area.width < 20 || area.height < 3 {
            return;
        }

        let glyphs = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let mut header_spans = Vec::new();

        // 1. Disclosure Marker
        let marker = if state.expanded { "▾ " } else { "▸ " };
        header_spans.push(Span::styled(marker, Style::default().fg(theme.info())));

        // 2. Summary text or Streaming liveness
        if props.is_streaming {
            let spin_char = glyphs[state.spinner_frame % glyphs.len()];
            header_spans.push(Span::styled(
                format!("{spin_char} Thinking "),
                Style::default()
                    .fg(theme.running())
                    .add_modifier(Modifier::BOLD),
            ));
            header_spans.push(Span::styled(
                format!("({}s)", state.elapsed_secs),
                Style::default().fg(theme.running()),
            ));
        } else {
            header_spans.push(Span::styled(
                &props.summary_label,
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            ));
            if let Some(dur) = props.duration_secs {
                header_spans.push(Span::styled(
                    format!(" ({dur}s)"),
                    Style::default().fg(theme.muted()),
                ));
            }
        }

        let header_rect = Rect::new(area.x, area.y, area.width, 1);
        frame.render_widget(&mut Paragraph::new(Line::from(header_spans)), header_rect);

        // 3. Render expanded body paragraphs if expanded
        if state.expanded && area.height > 2 {
            let body_rect = Rect::new(
                area.x,
                area.y.saturating_add(1),
                area.width,
                area.height.saturating_sub(1),
            );

            let mut body_lines = Vec::new();
            for para in &props.paragraphs {
                let wrapped =
                    nuo_view::text_layout::wrap_text(para, area.width.saturating_sub(4) as usize);
                for seg in &wrapped {
                    body_lines.push(Line::from(vec![
                        Span::styled("  │ ", Style::default().fg(theme.quote())),
                        Span::styled(seg.text.clone(), Style::default().fg(theme.fg())),
                    ]));
                }
                body_lines.push(Line::raw(""));
            }

            let visible_lines: Vec<Line> = body_lines
                .into_iter()
                .skip(state.scroll)
                .take(body_rect.height as usize)
                .collect();

            frame.render_widget(&mut Paragraph::new(visible_lines), body_rect);
        }
    }

    fn update(_props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        match msg {
            ReasoningCardMsg::ToggleExpand => {
                state.expanded = !state.expanded;
            }
            ReasoningCardMsg::ScrollUp(delta) => {
                state.scroll = state.scroll.saturating_sub(delta);
            }
            ReasoningCardMsg::ScrollDown(delta) => {
                state.scroll = state.scroll.saturating_add(delta);
            }
        }
        None
    }
}

impl StorybookComponent for ReasoningCard {
    fn category() -> &'static str {
        "Disclosure"
    }

    fn name() -> &'static str {
        "ReasoningTrace"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        vec![
            Story::new(
                "Deep Deductive Reasoning (Exhaustive Chain-of-Thought)",
                "Multi-paragraph chain-of-thought exploring invariant proofs and trade-offs",
                ReasoningCardProps {
                    summary_label: "Thought for 14 seconds (Proof Analysis)".to_string(),
                    duration_secs: Some(14),
                    is_streaming: false,
                    paragraphs: vec![
                        "Phase 1: Evaluating the invariant constraint matrix [INV-TUI-COLOR-01]. All color literals must resolve strictly through theme tokens to avoid background bleed.".to_string(),
                        "Phase 2: Verifying contrast invariants. On the light Paper scheme, foreground luminance must satisfy WCAG 4.5:1 ratio against the cream background (#f0f0ea).".to_string(),
                        "Phase 3: Synthesizing the presentation bridge. Component::render must adapt to both headless memory test frames and connected production spatial hit registries.".to_string(),
                        "Conclusion: The architecture is complete and provably sound across all terminal capability archetypes.".to_string(),
                    ],
                },
                ReasoningCardState {
                    expanded: true,
                    scroll: 0,
                    elapsed_secs: 14,
                    spinner_frame: 0,
                },
            ),
            Story::new(
                "Streaming In-Flight Thinking Trace",
                "Model actively reasoning with live spinner and incrementing elapsed timer",
                ReasoningCardProps {
                    summary_label: "Thinking".to_string(),
                    duration_secs: None,
                    is_streaming: true,
                    paragraphs: vec![
                        "Exploring execution paths for AST module resolution...".to_string(),
                        "Validating residency bounds across crates/nuo and crates/nuo-view...".to_string(),
                    ],
                },
                ReasoningCardState {
                    expanded: true,
                    scroll: 0,
                    elapsed_secs: 3,
                    spinner_frame: 2,
                },
            ),
            Story::new(
                "Compact Collapsed Thought Summary",
                "Single-row collapsed summary line keeping transcript focus clean",
                ReasoningCardProps {
                    summary_label: "Thought for 18 seconds (2,410 tokens)".to_string(),
                    duration_secs: Some(18),
                    is_streaming: false,
                    paragraphs: vec![
                        "Detailed reasoning content stored in trace buffer.".to_string(),
                    ],
                },
                ReasoningCardState {
                    expanded: false,
                    scroll: 0,
                    elapsed_secs: 18,
                    spinner_frame: 0,
                },
            ),
        ]
    }

    fn map_key_to_msg(key: &Key, _props: &Self::Props, _state: &Self::State) -> Option<Self::Msg> {
        match key {
            Key::Char(' ') | Key::Enter => Some(ReasoningCardMsg::ToggleExpand),
            Key::Up | Key::Char('k') => Some(ReasoningCardMsg::ScrollUp(1)),
            Key::Down | Key::Char('j') => Some(ReasoningCardMsg::ScrollDown(1)),
            _ => None,
        }
    }

    fn tick(props: &mut Self::Props, state: &mut Self::State) -> StoryEventOutcome {
        if props.is_streaming {
            state.spinner_frame = (state.spinner_frame + 1) % 10;
            if state.spinner_frame == 0 {
                state.elapsed_secs = state.elapsed_secs.saturating_add(1);
            }
            StoryEventOutcome::Consumed
        } else {
            StoryEventOutcome::Ignored
        }
    }
}
