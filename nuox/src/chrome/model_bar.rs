//! The persistent single-line model bar pinned below composer input.

use nuotc::{Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::design::{MODEL_BAR_GAP_MIN, MODEL_BAR_INNER_PADDING, MODEL_BAR_MODEL_GAP};
use crate::render::Theme;

pub const CONTEXT_USAGE_WARN_THRESHOLD: f64 = 0.70;
pub const CONTEXT_USAGE_CRIT_THRESHOLD: f64 = 0.90;

/// Inputs for [`draw_model_bar`]. The split row's halves — context usage
/// on the left, model identity on the right.
pub struct ModelBarProps<'a> {
    pub current_model: &'a str,
    pub model_available: bool,
    pub provider_name: Option<&'a str>,
    pub reasoning_effort: Option<&'a str>,
    pub context_tokens: Option<usize>,
    pub context_window: usize,
    pub ignition_elapsed_ms: Option<u128>,
}

impl<'a> Default for ModelBarProps<'a> {
    fn default() -> Self {
        Self {
            current_model: "",
            model_available: true,
            provider_name: None,
            reasoning_effort: None,
            context_tokens: None,
            context_window: 0,
            ignition_elapsed_ms: None,
        }
    }
}

/// Fine-grained click targets painted inside the model bar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelBarRects {
    pub context: Option<Rect>,
    pub connection: Option<Rect>,
}

/// Format token count into SI abbreviation (e.g. `120k`, `1.2M`, `3.2B`).
pub fn format_token_count(n: usize) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

/// Context-window usage indicator: `89.2k (8%)`.
pub(crate) fn context_usage_spans(
    used: usize,
    max: usize,
    theme: &Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    let ratio = if max == 0 {
        0.0
    } else {
        ((used as f64) / (max as f64)).clamp(0.0, 1.0)
    };
    let color = if ratio < CONTEXT_USAGE_WARN_THRESHOLD {
        theme.muted()
    } else if ratio < CONTEXT_USAGE_CRIT_THRESHOLD {
        theme.warn()
    } else {
        theme.err()
    };
    let pct = (ratio * 100.0).round() as u32;

    let mut spans = Vec::with_capacity(2);
    spans.push(Span::styled(
        format_token_count(used),
        Style::default().fg(theme.muted()).bg(bg),
    ));
    spans.push(Span::styled(
        format!(" ({}%)", pct),
        Style::default().fg(color).bg(bg),
    ));
    spans
}

/// Draw the single-line model bar pinned below the input box.
pub fn draw_model_bar(
    frame: &mut Frame,
    rect: Rect,
    props: ModelBarProps<'_>,
    theme: &Theme,
    key_overrides: &crate::keymap::GlobalOverrides,
) -> ModelBarRects {
    let ModelBarProps {
        current_model,
        model_available,
        provider_name,
        reasoning_effort,
        context_tokens,
        context_window,
        ignition_elapsed_ms,
    } = props;

    let bg = theme.surface();
    let full_w = rect.width as usize;
    let inner = MODEL_BAR_INNER_PADDING;

    let context_max = if context_window > 0 {
        context_window
    } else {
        nuo_contracts::model::resolve(current_model).context_window
    };

    let (model_label, model_style) = if current_model.is_empty() {
        (
            "(no model)".to_string(),
            Style::default().fg(theme.muted()).bg(bg),
        )
    } else if !model_available {
        let name = current_model;
        (
            format!("{name} [unavailable]"),
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD)
                .bg(bg),
        )
    } else {
        (
            current_model.to_string(),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
                .bg(bg),
        )
    };
    let model_width = model_label.width();
    let model_spans = vec![Span::styled(model_label, model_style)];

    let instance_label = provider_name
        .filter(|name| !name.is_empty())
        .map(|name| format!("@{name}"));
    let mut instance_spans: Vec<Span<'static>> = Vec::new();
    if let Some(label) = &instance_label {
        instance_spans.push(Span::styled(
            label.clone(),
            Style::default().fg(theme.muted()).bg(bg),
        ));
    }
    let instance_width = instance_spans
        .iter()
        .map(|span| span.content.width())
        .sum::<usize>();

    let mut reasoning_spans: Vec<Span<'static>> = Vec::new();
    if let Some(effort) = reasoning_effort {
        reasoning_spans.push(Span::styled(
            effort.to_string(),
            Style::default().fg(theme.muted()).bg(bg),
        ));
    }
    let reasoning_width = reasoning_spans
        .iter()
        .map(|span| span.content.width())
        .sum::<usize>();

    let mut context_spans: Vec<Span<'static>> = Vec::new();
    if context_max > 0 {
        let used = context_tokens.unwrap_or(0);
        context_spans = context_usage_spans(used, context_max, theme, bg);
    }
    let context_seg_width = context_spans
        .iter()
        .map(|span| span.content.width())
        .sum::<usize>();

    let keycap_badge =
        |text: &str| Span::styled(text.to_string(), Style::default().fg(theme.dim()).bg(bg));
    // Both keycaps render the chord the registry actually resolves (ADR-0238):
    // a retired/remapped/unbound chord must never be advertised.
    let telemetry_key = key_overrides.effective_binding(crate::keymap::CommandId::OpenTelemetry);
    let connection_key =
        key_overrides.effective_binding(crate::keymap::CommandId::OpenActiveConnectionDetail);
    let telemetry_keycap_width = telemetry_key.map_or(0, |key| key.display().width() + 1);
    let connection_keycap_width = connection_key.map_or(0, |key| key.display().width() + 1);

    let mut show_model = model_width > 0;
    let mut show_reasoning = reasoning_width > 0;
    let mut show_instance = instance_width > 0;
    let mut show_context = context_seg_width > 0;
    let mut show_telemetry_keycap = show_context && telemetry_key.is_some();
    let mut show_connection_keycap = (show_model || show_instance) && connection_key.is_some();

    let identity_width_for = |model: bool, reasoning: bool, instance: bool, show_keycap: bool| {
        let identity_count = usize::from(model) + usize::from(reasoning) + usize::from(instance);
        let mut width = usize::from(model) * model_width
            + usize::from(reasoning) * reasoning_width
            + usize::from(instance) * instance_width
            + identity_count.saturating_sub(1) * MODEL_BAR_MODEL_GAP;
        if (model || instance) && show_keycap {
            width += connection_keycap_width;
        }
        width
    };
    let gauges_width_for = |context: bool, show_keycap: bool| {
        let mut width = 0;
        if context {
            width += context_seg_width;
        }
        if context && show_keycap {
            width += telemetry_keycap_width;
        }
        width
    };

    let fits = |gauges_width: usize, identity_width: usize| {
        let middle = usize::from(gauges_width > 0 && identity_width > 0) * MODEL_BAR_GAP_MIN;
        inner + gauges_width + middle + identity_width + inner <= full_w
    };

    let mut gauges_width = gauges_width_for(show_context, show_telemetry_keycap);
    let mut identity_width = identity_width_for(
        show_model,
        show_reasoning,
        show_instance,
        show_connection_keycap,
    );

    if !fits(gauges_width, identity_width) && show_telemetry_keycap {
        show_telemetry_keycap = false;
        gauges_width = gauges_width_for(show_context, show_telemetry_keycap);
    }
    if !fits(gauges_width, identity_width) && show_connection_keycap {
        show_connection_keycap = false;
        identity_width = identity_width_for(
            show_model,
            show_reasoning,
            show_instance,
            show_connection_keycap,
        );
    }
    if !fits(gauges_width, identity_width) && show_instance {
        show_instance = false;
        identity_width = identity_width_for(
            show_model,
            show_reasoning,
            show_instance,
            show_connection_keycap,
        );
    }
    if !fits(gauges_width, identity_width) && show_reasoning {
        show_reasoning = false;
        identity_width = identity_width_for(
            show_model,
            show_reasoning,
            show_instance,
            show_connection_keycap,
        );
    }
    if !fits(gauges_width, identity_width) && show_context {
        show_context = false;
        show_telemetry_keycap = false;
        gauges_width = gauges_width_for(show_context, show_telemetry_keycap);
    }
    if !fits(gauges_width, identity_width) && show_model {
        show_model = false;
        show_connection_keycap = false;
    }

    let label_budget = identity_width_for(
        show_model,
        show_reasoning,
        show_instance,
        show_connection_keycap,
    );
    let label_spans = ignition_elapsed_ms
        .and_then(|ms| crate::effort_ignition::label_cluster(label_budget, ms, bg, theme));
    let ignition_label_active = label_spans.is_some();

    let mut left_spans: Vec<Span<'static>> = Vec::new();
    if show_context {
        left_spans.extend(context_spans);
    }
    if show_context
        && show_telemetry_keycap
        && let Some(key) = telemetry_key
    {
        left_spans.push(Span::styled(" ", Style::default().bg(bg)));
        left_spans.push(keycap_badge(key.display()));
    }

    let mut right_spans: Vec<Span<'static>> = Vec::new();
    if let Some(label) = label_spans {
        right_spans = label;
    } else {
        let identity_separator =
            || Span::styled(" ".repeat(MODEL_BAR_MODEL_GAP), Style::default().bg(bg));
        let mut identity_started = false;
        for segment in [
            show_model.then_some(model_spans),
            show_reasoning.then_some(reasoning_spans),
            show_instance.then_some(instance_spans),
        ]
        .into_iter()
        .flatten()
        {
            if identity_started {
                right_spans.push(identity_separator());
            }
            identity_started = true;
            right_spans.extend(segment);
        }
        if (show_model || show_instance)
            && show_connection_keycap
            && let Some(key) = connection_key
        {
            right_spans.push(Span::styled(" ", Style::default().bg(bg)));
            right_spans.push(keycap_badge(key.display()));
        }
    }

    let left_rendered_width: usize = left_spans.iter().map(|s| s.content.width()).sum();
    let right_rendered_width: usize = right_spans.iter().map(|s| s.content.width()).sum();
    let min_gap =
        usize::from(left_rendered_width > 0 && right_rendered_width > 0) * MODEL_BAR_GAP_MIN;
    let gap = full_w
        .saturating_sub(inner + left_rendered_width + right_rendered_width + inner)
        .max(min_gap);

    let mut spans: Vec<Span<'static>> =
        Vec::with_capacity(8 + left_spans.len() + right_spans.len());
    spans.push(Span::styled(" ".repeat(inner), Style::default().bg(bg)));
    spans.extend(left_spans);
    spans.push(Span::styled(" ".repeat(gap), Style::default().bg(bg)));
    spans.extend(right_spans);
    let used: usize = inner + left_rendered_width + gap + right_rendered_width;
    spans.push(Span::styled(
        " ".repeat(full_w.saturating_sub(used)),
        Style::default().bg(bg),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);

    let mut context_rect: Option<Rect> = None;
    let mut connection_rect: Option<Rect> = None;
    if !ignition_label_active {
        if show_context {
            let keycap_extra = if show_telemetry_keycap {
                telemetry_keycap_width
            } else {
                0
            };
            let width = (context_seg_width + keycap_extra) as u16;
            context_rect = Some(Rect::new(rect.x + inner as u16, rect.y, width, rect.height));
        }
        if right_rendered_width > 0 {
            let right_x = (inner + left_rendered_width + gap) as u16;
            connection_rect = Some(Rect::new(
                rect.x + right_x,
                rect.y,
                right_rendered_width as u16,
                rect.height,
            ));
        }
    }
    ModelBarRects {
        context: context_rect,
        connection: connection_rect,
    }
}
