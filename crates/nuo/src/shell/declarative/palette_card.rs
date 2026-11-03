//! Pure Declarative Color Palette Specimen Component (ADR-0072 / ADR-0073 / ADR-0147).
//!
//! Provides isolated inspection for the design system color tokens and contrast ratios,
//! ensuring complete parity between user-facing appearance settings and developer harnesses.

use super::{
    Component,
    story::{Story, StorybookComponent},
};
use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};

/// Presentation properties for the color palette specimen inspector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteCardProps {
    pub scheme_name: String,
}

/// Volatile interactive state for palette inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteCardState {
    pub active_tab: usize,
}

pub struct PaletteCard;

impl Component for PaletteCard {
    type Props = PaletteCardProps;
    type State = PaletteCardState;
    type Msg = ();
    type Cmd = ();

    fn render(
        props: &Self::Props,
        _state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        if area.width < 30 || area.height < 10 {
            return;
        }

        let specimen = theme.palette_specimen(&props.scheme_name);
        let mut lines = Vec::new();

        // Title Header
        lines.push(Line::from(vec![
            Span::styled(
                format!("── Design System Color Tokens: {} ", props.scheme_name),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if specimen.is_dark {
                    "(Dark Mode)"
                } else {
                    "(Light Mode)"
                },
                Style::default().fg(theme.muted()),
            ),
            Span::styled(" ──", Style::default().fg(theme.dim())),
        ]));
        lines.push(Line::raw(""));

        // Section 1: Surface Staircase
        lines.push(Line::from(Span::styled(
            "Spatial Surfaces (ADR-0053):",
            Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
        )));
        for s in &specimen.surfaces {
            lines.push(Line::from(vec![
                Span::styled("  █ ", Style::default().fg(s.color)),
                Span::styled(
                    format!("{:<14}", s.name),
                    Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:>4.1}:1  ", s.contrast_ratio),
                    Style::default().fg(theme.muted()),
                ),
                Span::styled(s.description, Style::default().fg(theme.text_muted)),
            ]));
        }
        lines.push(Line::raw(""));

        // Section 2: Semantic Signals
        lines.push(Line::from(Span::styled(
            "Semantic Signals & Vitality (ADR-0110):",
            Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
        )));
        for s in &specimen.signals {
            lines.push(Line::from(vec![
                Span::styled("  █ ", Style::default().fg(s.color)),
                Span::styled(
                    format!("{:<14}", s.name),
                    Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:>4.1}:1  ", s.contrast_ratio),
                    Style::default().fg(theme.muted()),
                ),
                Span::styled(s.description, Style::default().fg(theme.text_muted)),
            ]));
        }
        lines.push(Line::raw(""));

        // Section 3: Text Ladder
        lines.push(Line::from(Span::styled(
            "Text Hierarchy Ladder:",
            Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
        )));
        for s in &specimen.text_ladder {
            lines.push(Line::from(vec![
                Span::styled("  █ ", Style::default().fg(s.color)),
                Span::styled(
                    format!("{:<14}", s.name),
                    Style::default().fg(s.color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:>4.1}:1  ", s.contrast_ratio),
                    Style::default().fg(theme.muted()),
                ),
                Span::styled(s.description, Style::default().fg(theme.text_muted)),
            ]));
        }

        frame.render_widget(&mut Paragraph::new(lines), area);
    }

    fn update(
        _props: &Self::Props,
        _state: &mut Self::State,
        _msg: Self::Msg,
    ) -> Option<Self::Cmd> {
        None
    }
}

impl StorybookComponent for PaletteCard {
    fn category() -> &'static str {
        "System"
    }

    fn name() -> &'static str {
        "ColorPalette"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        vec![
            Story::new(
                "Zen Palette Specimen",
                "Quiet dark charcoal design tokens with sage green accents",
                PaletteCardProps {
                    scheme_name: "Zen".to_string(),
                },
                PaletteCardState::default(),
            ),
            Story::new(
                "Nord Palette Specimen",
                "Cool arctic blues and soft contrast tokens",
                PaletteCardProps {
                    scheme_name: "Nord".to_string(),
                },
                PaletteCardState::default(),
            ),
            Story::new(
                "Paper Palette Specimen",
                "Warm bright light surface for high-illumination terminals",
                PaletteCardProps {
                    scheme_name: "Paper".to_string(),
                },
                PaletteCardState::default(),
            ),
            Story::new(
                "Catppuccin Palette Specimen",
                "Warm mocha background with lavender identity accents",
                PaletteCardProps {
                    scheme_name: "Catppuccin".to_string(),
                },
                PaletteCardState::default(),
            ),
        ]
    }
}
