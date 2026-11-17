//! Image tool-result card.
//!
//! The terminal has no portable inline-graphics contract (Kitty/iTerm2/Sixel
//! each need a capability probe and a separate encoder), so the transcript
//! must not pretend a base64 payload is prose. The card keeps the image
//! visible as a typed object: MIME, approximate byte size, and a truthful
//! statement that the model received the bytes while this terminal did not
//! paint them.

use ::tool::ToolOutput;
use nuotc::{Line, Modifier, Span, Style};
use view::model::selection::SelectionState;

use crate::shell::disclosure::renderers::base::RenderCtx;

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_image_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    structured: Option<&ToolOutput>,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let (mime, data) = match structured {
        Some(ToolOutput::Image { mime, data }) => (mime.as_str(), data.as_str()),
        _ => ("image", ""),
    };
    let bytes = data.len().saturating_mul(3) / 4;
    let prefix = " ".repeat(indent);
    let bg = ctx.theme.code_surface();
    let _ = selection;
    let _ = mi;
    let _ = block_idx;

    let mut header = vec![
        Span::styled(prefix.clone(), Style::default().bg(bg)),
        Span::styled(
            " image ",
            Style::default()
                .bg(bg)
                .fg(ctx.theme.info())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(mime.to_string(), Style::default().bg(bg).fg(ctx.theme.fg())),
        Span::styled(
            format!("  ·  {}", human_bytes(bytes)),
            Style::default().bg(bg).fg(ctx.theme.muted()),
        ),
    ];
    header.push(Span::styled(
        " ".repeat(inner_w.saturating_sub(
            prefix.chars().count()
                + 7
                + mime.chars().count()
                + 5
                + human_bytes(bytes).chars().count(),
        )),
        Style::default().bg(bg),
    ));
    ctx.paint(Line::from(header));

    let note = if data.is_empty() {
        "Image bytes are not present in this restored result."
    } else {
        "The model received these bytes; this terminal has no inline image protocol. \
         Copy the attachment to open it externally."
    };
    let body = vec![
        Span::styled(prefix, Style::default().bg(bg)),
        Span::styled(
            note.to_string(),
            Style::default().bg(bg).fg(ctx.theme.muted()),
        ),
    ];
    ctx.paint(Line::from(body));

    let _ = output;
}

fn human_bytes(bytes: usize) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
