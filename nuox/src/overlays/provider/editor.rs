//! Model settings editor (effort + thinking toggles), connection-template chooser, and custom provider editor.

use nuotc::{
    Alignment, Frame, {Line, Span}, {Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

use super::super::common::{caret_column, field_viewport, truncate_ellipsis};
use crate::components::options::{ChoiceTone, choice_style, push_wrapped_styled};
use crate::components::row::{GROUP_GAP, GUTTER, ListRow, RowGroup, RowStyledAtom};
use crate::primitives::{
    BodyRenderOptions, ContentModalSpec, FixedModalSpec, FooterHint, SCROLL_EDGE_MARGIN,
    breadcrumb_parts, content_modal_area, content_modal_probe, hierarchical_breadcrumb, keyvocab,
    modal_area, modal_chrome_rows, modal_frame, modal_header_parts, render_body,
    render_modal_footer,
};
use crate::providers::{ConnectionTemplate, CustomField, PROVIDER_PRESETS};
use crate::render::Theme;

// Effort selector (Faster⇄Smarter node slider)

const EFFORT_SCALE_ENDS: (&str, &str) = ("Faster", "Smarter");
const TRACK_NODE: char = '○';
const TRACK_MARKER: char = '●';
const EFFORT_LABEL_MIN_GAP: usize = 2;

fn effort_tier(wire: &str) -> Option<nuo_contracts::effort::Effort> {
    nuo_contracts::effort::Effort::parse(wire)
}

fn effort_caption(wire: &str) -> &'static str {
    effort_tier(wire).map(|e| e.description()).unwrap_or("")
}

fn slider_track_width(body_width: usize) -> usize {
    let (lo, hi) = EFFORT_SCALE_ENDS;
    body_width.saturating_sub(lo.width() + hi.width() + 2)
}

fn slider_node_columns(n: usize, track_w: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![track_w / 2];
    }
    (0..n)
        .map(|i| (i * (track_w - 1) + (n - 1) / 2) / (n - 1))
        .collect()
}

fn slider_label_layout(
    levels: &[String],
    current: &str,
    body_width: usize,
) -> Option<Vec<Option<usize>>> {
    let n = levels.len();
    if n == 0 || body_width == 0 {
        return None;
    }
    let track_w = slider_track_width(body_width);
    if track_w == 0 {
        return None;
    }
    let columns = slider_node_columns(n, track_w);
    let (lo, _) = EFFORT_SCALE_ENDS;
    let lo_pad = lo.width() + 1;
    let sel_idx = levels.iter().position(|l| l == current);
    let width = |i: usize| levels[i].as_str().width();

    let mut positions: Vec<Option<usize>> = Vec::with_capacity(n);
    let mut prev_end: Option<usize> = None;
    for (i, &col) in columns.iter().enumerate() {
        let w = width(i);
        if w > body_width {
            break;
        }
        let node_x = lo_pad + col;
        let start = node_x
            .saturating_sub(w / 2)
            .min(body_width.saturating_sub(w));
        if let Some(end) = prev_end
            && start < end + EFFORT_LABEL_MIN_GAP
        {
            break;
        }
        prev_end = Some(start + w);
        positions.push(Some(start));
    }
    if positions.len() == n {
        return Some(positions);
    }

    positions = Vec::with_capacity(n);
    prev_end = None;
    for (i, &col) in columns.iter().enumerate() {
        let w = width(i);
        let must = i == 0 || i + 1 == n || Some(i) == sel_idx;
        if w > body_width {
            positions.push(None);
            continue;
        }
        let node_x = lo_pad + col;
        let desired = node_x
            .saturating_sub(w / 2)
            .min(body_width.saturating_sub(w));
        let start = match prev_end {
            Some(end) if desired < end + EFFORT_LABEL_MIN_GAP => {
                if !must {
                    positions.push(None);
                    continue;
                }
                let pushed = end + 1;
                if pushed + w > body_width {
                    positions.push(None);
                    continue;
                }
                pushed
            }
            _ => desired,
        };
        prev_end = Some(start + w);
        positions.push(Some(start));
    }
    Some(positions)
}

pub(crate) fn effort_block_rows(levels: &[String]) -> u16 {
    if levels.is_empty() { 3 } else { 6 }
}

fn effort_block_lines(
    current: &str,
    levels: &[String],
    body_width: usize,
    focused: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let caption = truncate_ellipsis(effort_caption(current), body_width);
    let caption_row = || {
        Line::from(Span::styled(
            caption.to_string(),
            Style::default().fg(theme.muted()),
        ))
        .alignment(Alignment::Center)
    };
    let selected_fg = if current == "max" {
        theme.warn()
    } else if focused {
        theme.brand()
    } else {
        theme.fg()
    };

    let Some(positions) = slider_label_layout(levels, current, body_width) else {
        return vec![
            Line::from(vec![
                Span::styled(
                    "Effort  ".to_string(),
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    current.to_string(),
                    Style::default()
                        .fg(selected_fg)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::default(),
            caption_row(),
        ];
    };

    let (lo, hi) = EFFORT_SCALE_ENDS;
    let track_w = slider_track_width(body_width);
    let columns = slider_node_columns(levels.len(), track_w);
    let sel_idx = levels.iter().position(|l| l == current).unwrap_or(0);
    let track_style = Style::default().fg(theme.muted());
    let marker_style = Style::default()
        .fg(selected_fg)
        .add_modifier(Modifier::BOLD);

    let track: Vec<char> = (0..track_w)
        .map(|col| match columns.iter().position(|&c| c == col) {
            Some(i) if i == sel_idx => TRACK_MARKER,
            Some(_) => TRACK_NODE,
            None => '─',
        })
        .collect();
    let marker_col = columns[sel_idx];
    let before: String = track[..marker_col].iter().collect();
    let after: String = track[marker_col + 1..].iter().collect();
    let track_row = Line::from(vec![
        Span::styled(format!("{lo} "), track_style),
        Span::styled(before, track_style),
        Span::styled(TRACK_MARKER.to_string(), marker_style),
        Span::styled(after, track_style),
        Span::styled(format!(" {hi}"), track_style),
    ]);

    let mut label_spans: Vec<Span<'static>> = Vec::new();
    let mut x = 0;
    for (i, level) in levels.iter().enumerate() {
        if let Some(start) = positions[i] {
            if start > x {
                label_spans.push(Span::raw(" ".repeat(start - x)));
            }
            let style = if i == sel_idx {
                Style::default()
                    .fg(selected_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                track_style
            };
            x = start + level.as_str().width();
            label_spans.push(Span::styled(level.clone(), style));
        }
    }

    vec![
        Line::from(Span::styled(
            "Effort",
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        track_row,
        Line::from(label_spans),
        Line::default(),
        caption_row(),
    ]
}

/// Draw the per-model/channel settings editor (effort ladder + thinking on/off checkbox).
#[allow(clippy::too_many_arguments)]
pub fn draw_model_editor(
    frame: &mut Frame,
    title: &str,
    input: &str,
    cursor_position: usize,
    show_key: bool,
    focused_field: u8,
    // The frame-level caret verdict (ADR-0205): `true` only while this sheet is
    // the layer that owns the physical cursor.
    show_caret: bool,
    effort: Option<&str>,
    effort_levels: &[String],
    thinking: Option<bool>,
    overrides: Option<(Option<bool>, Option<bool>)>,
    theme: &Theme,
) -> nuotc::Rect {
    let geometry = ContentModalSpec::MODEL_EDITOR;

    let body_width = content_modal_probe(frame, geometry)
        .width
        .saturating_sub(2 * crate::design::MODAL_INNER_H_PADDING) as usize;
    let effort_rows = match effort {
        Some(_) => effort_block_rows(effort_levels),
        None => 0,
    };
    let overrides_rows = overrides.map(|_| 2).unwrap_or(0);
    let body_rows = show_key as u16 + effort_rows + thinking.is_some() as u16 + overrides_rows;
    let desired = body_rows + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme, true, true);

    let child_title = format!("Edit {title}");
    let header = breadcrumb_parts("Models", &child_title);
    modal_header_parts(frame, f.header, &header, theme);

    let label_style = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);
    let mut api_key_off: usize = 0;
    let mut body: Vec<Line> = Vec::new();
    if show_key {
        let label = format!("{:<8}", "API key");
        let label_w = label.width();
        let field_w = body_width.saturating_sub(label_w);
        let key_off;
        let value_span = if input.is_empty() {
            key_off = 0;
            Span::styled("enter key…".to_string(), Style::default().fg(theme.muted()))
        } else if focused_field == 0 {
            let (off, text) = field_viewport(input, cursor_position, field_w);
            key_off = off;
            Span::styled(
                text,
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            )
        } else {
            key_off = 0;
            Span::styled(
                truncate_ellipsis(input, field_w.max(1)),
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            )
        };
        body.push(Line::from(vec![
            Span::styled(label, label_style),
            value_span,
        ]));
        api_key_off = key_off;
    }

    if let Some(effort) = effort {
        for line in effort_block_lines(effort, effort_levels, body_width, focused_field == 1, theme)
        {
            body.push(line);
        }
    }

    if let Some(on) = thinking {
        let label = format!("{:<8}", "Thinking");
        let box_style = Style::default()
            .fg(if focused_field == 2 {
                theme.brand()
            } else {
                theme.fg()
            })
            .add_modifier(Modifier::BOLD);
        let word_style = Style::default().fg(if on { theme.ok() } else { theme.muted() });
        let checkbox = if on { "[x]" } else { "[ ]" };
        let word = if on { "on" } else { "off" };
        let tail = format!("{checkbox} {word}");
        let pad = body_width.saturating_sub(label.width() + tail.width());
        body.push(Line::from(vec![
            Span::styled(label, label_style),
            Span::raw(" ".repeat(pad)),
            Span::styled(checkbox.to_string(), box_style),
            Span::styled(format!(" {word}"), word_style),
        ]));
    }

    if let Some((vision, tool)) = overrides {
        for (idx, (label, value)) in [(0u8, ("Vision", vision)), (1u8, ("Tool call", tool))] {
            let field = 3 + idx;
            let label_text = format!("{:<8}", label);
            let box_style = Style::default()
                .fg(if focused_field == field {
                    theme.brand()
                } else {
                    theme.fg()
                })
                .add_modifier(Modifier::BOLD);
            let (marker, word, word_style) = match value {
                None => ("[-]", "inherit", Style::default().fg(theme.muted())),
                Some(true) => ("[x]", "force on", Style::default().fg(theme.ok())),
                Some(false) => ("[ ]", "force off", Style::default().fg(theme.warn())),
            };
            let tail = format!("{marker} {word}");
            let pad = body_width.saturating_sub(label_text.width() + tail.width());
            body.push(Line::from(vec![
                Span::styled(label_text, label_style),
                Span::raw(" ".repeat(pad)),
                Span::styled(marker.to_string(), box_style),
                Span::styled(format!(" {word}"), word_style),
            ]));
        }
    }

    let body_rect = f.body;
    render_body(
        frame,
        body_rect,
        body,
        &mut 0,
        BodyRenderOptions::default(),
        theme,
    );

    if let Some(fo) = f.footer {
        let mut hints: Vec<FooterHint> = Vec::with_capacity(6);
        hints.push(FooterHint::key_primary(crate::keymap::Key::ENTER, "save"));
        if effort.is_some() || thinking.is_some() {
            hints.push(FooterHint::key_secondary(crate::keymap::Key::TAB, "field"));
        }
        if overrides.is_some() {
            hints.push(FooterHint::secondary(keyvocab::SPACE, "override"));
        }
        if effort.is_some() {
            hints.push(FooterHint::secondary(keyvocab::ARROWS_LR, "effort"));
            hints.push(FooterHint::secondary("1-7", "jump"));
        }
        if thinking.is_some() {
            hints.push(FooterHint::secondary(keyvocab::SPACE, "thinking"));
        }
        hints.push(FooterHint::key_always(crate::keymap::Key::ESC, "cancel"));
        render_modal_footer(frame, fo, &hints, theme);
    }

    if show_caret && show_key && focused_field == 0 && body_rect.width > 0 && body_rect.height > 0 {
        let prefix = format!("{:<8}", "API key");
        let caret_col = caret_column(input, cursor_position);
        let max_x = body_rect.right().saturating_sub(1);
        let local_caret = (caret_col as usize).saturating_sub(api_key_off);
        let mut cursor_x = body_rect
            .x
            .saturating_add(prefix.width().min(u16::MAX as usize) as u16)
            .saturating_add(local_caret.min(u16::MAX as usize) as u16);
        if cursor_x > max_x {
            cursor_x = max_x;
        }
        let cursor_y = body_rect.y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
    area
}

/// Focused one-field editor used by singleton web provider setup.
#[allow(clippy::too_many_arguments)]
pub fn draw_web_value_editor(
    frame: &mut Frame,
    title: &str,
    label: &str,
    input: &str,
    cursor_position: usize,
    secret: bool,
    // The frame-level caret verdict (ADR-0205).
    show_caret: bool,
    theme: &Theme,
) -> nuotc::Rect {
    let geometry = ContentModalSpec::MODEL_EDITOR;
    let desired = 1 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let modal = modal_frame(frame, area, theme, true, true);
    modal_header_parts(
        frame,
        modal.header,
        &breadcrumb_parts("Web tools", title),
        theme,
    );

    let label_text = format!("{label:<10}");
    let field_width = modal.body.width.saturating_sub(label_text.width() as u16) as usize;
    let (offset, visible) = field_viewport(input, cursor_position, field_width);
    let visible = if secret && !visible.is_empty() {
        "•".repeat(visible.chars().count())
    } else if visible.is_empty() {
        if secret {
            "enter token…".into()
        } else {
            "enter URL…".into()
        }
    } else {
        visible
    };
    render_body(
        frame,
        modal.body,
        vec![Line::from(vec![
            Span::styled(
                label_text.clone(),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                visible,
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            ),
        ])],
        &mut 0,
        BodyRenderOptions::default(),
        theme,
    );
    if let Some(footer) = modal.footer {
        render_modal_footer(
            frame,
            footer,
            &[
                FooterHint::key_primary(crate::keymap::Key::ENTER, "save"),
                FooterHint::key_always(crate::keymap::Key::ESC, "cancel"),
            ],
            theme,
        );
    }
    if show_caret && modal.body.width > 0 && modal.body.height > 0 {
        let local_caret = usize::from(caret_column(input, cursor_position)).saturating_sub(offset);
        let cursor_x = modal
            .body
            .x
            .saturating_add(label_text.width() as u16)
            .saturating_add(local_caret.min(field_width) as u16)
            .min(modal.body.right().saturating_sub(1));
        frame.set_cursor_position((cursor_x, modal.body.y));
    }
    area
}

pub(crate) struct PresetRow {
    pub(crate) body_width: usize,
}

impl PresetRow {
    fn title_budget(&self) -> usize {
        self.body_width.saturating_sub(GUTTER + GROUP_GAP).max(1)
    }

    pub(crate) fn build(
        &self,
        template: &ConnectionTemplate,
        focused: bool,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let style = choice_style(ChoiceTone::Filled, focused, theme);
        let title = truncate_ellipsis(template.display_title(), self.title_budget());

        let mut identity = RowGroup::fixed();
        for c in title.chars() {
            identity = identity.styled(
                RowStyledAtom {
                    text: c.to_string(),
                    style: Style::default()
                        .bg(style.bg)
                        .fg(style.fg)
                        .add_modifier(Modifier::BOLD),
                },
                0,
            );
        }

        let row = ListRow::new(style, self.body_width).group(identity);

        if !focused {
            return vec![row.finish()];
        }

        let mut lines = vec![row.finish()];
        let indent = " ".repeat(GUTTER + GROUP_GAP);
        push_wrapped_styled(
            &mut lines,
            &indent,
            &indent,
            template.description,
            Style::default().bg(theme.panel()).fg(theme.dim()),
            self.body_width,
        );
        lines
    }
}

/// Draw the template chooser as the Connections list's Add connection child page.
pub fn draw_preset_chooser(
    selected: usize,
    frame: &mut Frame,
    theme: &Theme,
    scroll: &mut usize,
) -> nuotc::Rect {
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme, true, true);

    let header = hierarchical_breadcrumb(
        &["Connections", "Add connection"],
        f.header.map(|h| h.width as usize).unwrap_or(80),
    );
    modal_header_parts(frame, f.header, &header, theme);

    let policy = PresetRow {
        body_width: f.body.width as usize,
    };

    let mut body: Vec<Line> = Vec::new();
    let mut follow: Option<usize> = None;
    for (i, template) in PROVIDER_PRESETS.iter().enumerate() {
        let focused = i == selected;
        if focused {
            follow = Some(body.len());
        }
        body.extend(policy.build(template, focused, theme));
    }

    render_body(
        frame,
        f.body,
        body,
        scroll,
        BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
        theme,
    );

    let oauth_template = PROVIDER_PRESETS
        .get(selected)
        .is_some_and(|template| template.auth.is_oauth());
    let mut hints: Vec<FooterHint> = vec![
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, "select"),
    ];
    if oauth_template {
        hints.push(FooterHint::secondary("b", "browser"));
        hints.push(FooterHint::secondary("d", "device"));
    }
    hints.push(FooterHint::key_always(crate::keymap::Key::ESC, "back"));
    if let Some(fo) = f.footer {
        render_modal_footer(frame, fo, &hints, theme);
    }
    area
}

/// Everything [`draw_custom_provider_editor`] renders, bundled so the call site stays readable.
pub struct CustomEditorProps<'a> {
    pub fields: &'a [CustomField],
    pub field: u8,
    pub editing: bool,
    pub custom: bool,
    pub title: &'a str,
    pub name_buf: &'a str,
    pub base_url_buf: &'a str,
    pub token_buf: &'a str,
    pub model_buf: &'a str,
    pub protocol_display: &'a str,
    pub identity_display: &'a str,
    pub url_hint: &'a str,
    pub input: &'a str,
    pub cursor_position: usize,
}

/// Draw the provider editor: a per-template form drawn from [`CustomEditorProps::fields`].
pub fn draw_custom_provider_editor(
    props: CustomEditorProps<'_>,
    frame: &mut Frame,
    theme: &Theme,
    scroll: &mut usize,
    // The frame-level caret verdict (ADR-0205).
    show_caret: bool,
) -> nuotc::Rect {
    let CustomEditorProps {
        fields,
        field,
        editing,
        custom,
        title,
        name_buf,
        base_url_buf,
        token_buf,
        model_buf,
        protocol_display,
        identity_display,
        url_hint,
        input,
        cursor_position,
    } = props;

    let geometry = ContentModalSpec::CUSTOM_PROVIDER;
    let desired = (fields.len() as u16) + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme, true, true);

    const LABEL_W: usize = 9;
    let body_width = f.body.width as usize;
    let label_cell_w = 3 + LABEL_W;
    let field_w = body_width.saturating_sub(label_cell_w);
    let focus_off = if input.is_empty() {
        0
    } else {
        field_viewport(input, cursor_position, field_w).0
    };
    let field_label = |label: &str, focused: bool| {
        let style = if focused {
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted())
        };
        let marker = if focused { " › " } else { "   " };
        Span::styled(format!("{marker}{label:<LABEL_W$}"), style)
    };
    let value_style = |focused: bool| {
        if focused {
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted())
        }
    };
    let placeholder = |val: String, focused: bool, hint: &str| -> Span<'static> {
        if val.is_empty() && focused {
            Span::styled(hint.to_string(), Style::default().fg(theme.muted()))
        } else {
            Span::styled(val, value_style(focused))
        }
    };
    let text_row = |focused: bool, label: &str, buf: &str, hint: &str, mask: bool| {
        let raw = if focused {
            field_viewport(input, cursor_position, field_w).1
        } else if mask {
            truncate_ellipsis(&"•".repeat(buf.chars().count()), field_w.max(1))
        } else {
            truncate_ellipsis(buf, field_w.max(1))
        };
        Line::from(vec![
            field_label(label, focused),
            placeholder(raw, focused, hint),
        ])
    };
    let choice_row = |focused: bool, label: &str, value: &str| {
        let display = if focused {
            format!("‹ {value} ›")
        } else {
            value.to_string()
        };
        Line::from(vec![
            field_label(label, focused),
            Span::styled(
                truncate_ellipsis(&display, field_w.max(1)),
                value_style(focused),
            ),
        ])
    };

    let header_width = f.header.map(|h| h.width as usize).unwrap_or(80);
    let levels: Vec<&str> = if editing {
        vec!["Connections", "Edit", title]
    } else if custom {
        vec!["Connections", "Add custom connection"]
    } else {
        vec!["Connections", "Add connection", title]
    };
    let header = hierarchical_breadcrumb(&levels, header_width);
    modal_header_parts(frame, f.header, &header, theme);

    let token_hint = if editing {
        "blank = keep existing"
    } else {
        "API key (blank for local)"
    };
    let mut body: Vec<Line> = Vec::new();
    for (idx, fld) in fields.iter().enumerate() {
        let focused = idx as u8 == field;
        body.push(match fld {
            CustomField::Name => text_row(focused, "Name", name_buf, "e.g. My Relay", false),
            CustomField::BaseUrl => text_row(focused, "Base URL", base_url_buf, url_hint, false),
            CustomField::Token => text_row(focused, "Token", token_buf, token_hint, true),
            CustomField::Model => text_row(
                focused,
                "Model",
                model_buf,
                "e.g. gpt-5, gpt-5-mini (comma-separated)",
                false,
            ),
            CustomField::Protocol => choice_row(focused, "API", protocol_display),
            CustomField::ClientIdentity => choice_row(focused, "Identity", identity_display),
        });
    }

    let body_rect = f.body;
    let follow = Some(field as usize);
    render_body(
        frame,
        body_rect,
        body,
        scroll,
        BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
        theme,
    );
    if let Some(fo) = f.footer {
        let mut hints: Vec<FooterHint> = Vec::with_capacity(5);
        hints.push(FooterHint::key_secondary(crate::keymap::Key::TAB, "field"));
        let choice_focused = matches!(
            fields.get(field as usize),
            Some(CustomField::Protocol | CustomField::ClientIdentity)
        );
        if choice_focused {
            hints.push(FooterHint::navigation(keyvocab::ARROWS_LR, "choose"));
        } else {
            hints.push(FooterHint::navigation(keyvocab::ARROWS_UD, "scroll"));
        }
        hints.push(FooterHint::key_primary(crate::keymap::Key::ENTER, "save"));
        hints.push(FooterHint::key_always(crate::keymap::Key::ESC, "cancel"));
        render_modal_footer(frame, fo, &hints, theme);
    }

    let row = field as usize;
    let text_focused = matches!(
        fields.get(field as usize),
        Some(CustomField::Name | CustomField::BaseUrl | CustomField::Token | CustomField::Model)
    );
    let visible = body_rect.height as usize;
    let in_view = (*scroll <= row) && (row < *scroll + visible);
    if show_caret && in_view && text_focused {
        let prefix_w = 3 + LABEL_W as u16;
        let caret_col = caret_column(input, cursor_position);
        let max_x = body_rect.right().saturating_sub(1);
        let local_caret = (caret_col as usize).saturating_sub(focus_off);
        let mut cursor_x = body_rect
            .x
            .saturating_add(prefix_w)
            .saturating_add(local_caret.min(u16::MAX as usize) as u16);
        if cursor_x > max_x {
            cursor_x = max_x;
        }
        let cursor_y = body_rect.y + (row - *scroll) as u16;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
    area
}
