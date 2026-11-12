//! Web search and article content renderers.
use nuotc::{Line, Modifier, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::disclosure::renderers::base::{RenderCtx, nonempty_wrapped};
use nuo_view::model::layout::LinkHit;
use nuo_view::model::selection::SelectionState;
use nuo_view::text_layout::{block_selection_range, padded_tail, wrap_text};

/// Render an interactive web search result stream with cards, domain pills, and clickable URLs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_web_search_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    arguments: &str,
    structured: Option<&nuo_protocol::ToolOutput>,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let _sel_range = block_selection_range(selection, mi, block_idx);

    let (query, _provider, hits, truncated) = match structured {
        Some(nuo_protocol::ToolOutput::WebSearch {
            query,
            provider,
            results,
            truncated,
        }) => (query.clone(), provider.clone(), results.clone(), *truncated),
        _ => parse_fallback_web_search(output, arguments),
    };

    if hits.is_empty() {
        let text = if query.is_empty() {
            "No web search results found.".to_string()
        } else {
            format!("No web search results found for '{}'.", query)
        };
        let wrapped = nonempty_wrapped(wrap_text(&text, inner_w.max(1)));
        for wl in &wrapped {
            let mut spans = vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(
                    wl.text.clone(),
                    Style::default().bg(code_bg).fg(ctx.theme.muted()),
                ),
            ];
            let used = indent + wl.text.width();
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint(Line::from(spans));
        }
        return;
    }

    let index_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.brand())
        .add_modifier(Modifier::BOLD);
    let title_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.heading())
        .add_modifier(Modifier::BOLD);
    let domain_style = Style::default().bg(code_bg).fg(ctx.theme.muted());
    let url_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.info())
        .add_modifier(Modifier::UNDERLINE);
    let snippet_style = Style::default().bg(code_bg).fg(ctx.theme.code_text());

    for (idx, hit) in hits.iter().enumerate() {
        if idx > 0 {
            let line = Line::from(vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(padded_tail(ctx.full_width, indent), pad),
            ]);
            ctx.paint(line);
        }

        // Line 1: [idx + 1] Title
        let prefix = format!("[{}] ", idx + 1);
        let title_avail_w = inner_w.saturating_sub(prefix.width()).max(1);
        let wrapped_title = nonempty_wrapped(wrap_text(&hit.title, title_avail_w));
        for (t_idx, wl) in wrapped_title.iter().enumerate() {
            let mut spans = vec![Span::styled(" ".repeat(indent), pad)];
            let used = if t_idx == 0 {
                spans.push(Span::styled(prefix.clone(), index_style));
                spans.push(Span::styled(wl.text.clone(), title_style));
                indent + prefix.width() + wl.text.width()
            } else {
                let sub_indent = prefix.width();
                spans.push(Span::styled(" ".repeat(sub_indent), pad));
                spans.push(Span::styled(wl.text.clone(), title_style));
                indent + sub_indent + wl.text.width()
            };
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint(Line::from(spans));
        }

        // Line 2: 🌐 domain | url
        if !hit.url.is_empty() {
            let globe = "  🌐 ";
            let domain_pill = if hit.domain.is_empty() {
                String::new()
            } else {
                format!("{} | ", hit.domain)
            };
            let mut spans = vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(globe, domain_style),
                Span::styled(domain_pill.clone(), domain_style),
                Span::styled(hit.url.clone(), url_style),
            ];
            let used = indent + globe.width() + domain_pill.width() + hit.url.width();
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            if let Some(rect) = ctx.paint(Line::from(spans)) {
                let col_start = indent + globe.width() + domain_pill.width();
                let url_w = hit.url.width();
                let max_w = (ctx.area.width as usize).saturating_sub(col_start);
                ctx.layout_map.push_link_hit(LinkHit {
                    message_idx: mi,
                    block_idx,
                    range: (0, 0),
                    url: hit.url.clone(),
                    rect: Rect::new(
                        rect.x + (col_start as u16),
                        rect.y,
                        (url_w.min(max_w) as u16).max(1),
                        1,
                    ),
                });
            }
        }

        // Line 3: Snippet
        if !hit.snippet.is_empty() {
            let snip_indent = indent + 4;
            let snip_w = inner_w.saturating_sub(4).max(1);
            let wrapped_snip = nonempty_wrapped(wrap_text(&hit.snippet, snip_w));
            for wl in &wrapped_snip {
                let mut spans = vec![
                    Span::styled(" ".repeat(snip_indent), pad),
                    Span::styled(wl.text.clone(), snippet_style),
                ];
                let used = snip_indent + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        }
    }

    if truncated {
        let note = "[... more search results omitted to fit context budget ...]";
        let mut spans = vec![
            Span::styled(" ".repeat(indent + 2), pad),
            Span::styled(note, Style::default().bg(code_bg).fg(ctx.theme.warn())),
        ];
        let used = indent + 2 + note.len();
        spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
        ctx.paint(Line::from(spans));
    }
}

/// Render a structured article reader view for fetched web pages without code gutters.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_web_article_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    arguments: &str,
    structured: Option<&nuo_protocol::ToolOutput>,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let _sel_range = block_selection_range(selection, mi, block_idx);

    let (url, title, _domain, markdown, reader, tokens, truncated) = match structured {
        Some(nuo_protocol::ToolOutput::WebArticle {
            url,
            title,
            domain,
            markdown,
            reader,
            tokens,
            truncated,
        }) => (
            url.clone(),
            title.clone(),
            domain.clone(),
            markdown.clone(),
            reader.clone(),
            *tokens,
            *truncated,
        ),
        _ => parse_fallback_web_article(output, arguments),
    };

    // Header line 1: 🔗 URL (clickable link)
    if !url.is_empty() {
        let prefix = "🔗 ";
        let mut spans = vec![
            Span::styled(" ".repeat(indent), pad),
            Span::styled(prefix, Style::default().bg(code_bg).fg(ctx.theme.muted())),
            Span::styled(
                url.clone(),
                Style::default()
                    .bg(code_bg)
                    .fg(ctx.theme.info())
                    .add_modifier(Modifier::UNDERLINE),
            ),
        ];
        let used = indent + prefix.width() + url.width();
        spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
        if let Some(rect) = ctx.paint(Line::from(spans)) {
            let col_start = indent + prefix.width();
            let url_w = url.width();
            let max_w = (ctx.area.width as usize).saturating_sub(col_start);
            ctx.layout_map.push_link_hit(LinkHit {
                message_idx: mi,
                block_idx,
                range: (0, 0),
                url: url.clone(),
                rect: Rect::new(
                    rect.x + (col_start as u16),
                    rect.y,
                    (url_w.min(max_w) as u16).max(1),
                    1,
                ),
            });
        }
    }

    // Header line 2: Security & Reader provenance banner
    {
        let shield = "🛡️  ";
        let provenance = format!(
            "Untrusted External Content | Reader: {} | ~{} tokens",
            reader, tokens
        );
        let mut spans = vec![
            Span::styled(" ".repeat(indent), pad),
            Span::styled(shield, Style::default().bg(code_bg).fg(ctx.theme.warn())),
            Span::styled(
                provenance.clone(),
                Style::default().bg(code_bg).fg(ctx.theme.muted()),
            ),
        ];
        let used = indent + shield.width() + provenance.width();
        spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
        ctx.paint(Line::from(spans));
    }

    // Blank separator row
    {
        let line = Line::from(vec![
            Span::styled(" ".repeat(indent), pad),
            Span::styled(padded_tail(ctx.full_width, indent), pad),
        ]);
        ctx.paint(line);
    }

    // Title (if available and not already first heading in markdown)
    let has_first_heading = markdown
        .lines()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| l.trim().starts_with('#'));
    if let Some(ref t) = title
        && !has_first_heading
    {
        let wrapped_title = nonempty_wrapped(wrap_text(t, inner_w.max(1)));
        for wl in &wrapped_title {
            let mut spans = vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(
                    wl.text.clone(),
                    Style::default()
                        .bg(code_bg)
                        .fg(ctx.theme.heading())
                        .add_modifier(Modifier::BOLD),
                ),
            ];
            let used = indent + wl.text.width();
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint(Line::from(spans));
        }
        let line = Line::from(vec![
            Span::styled(" ".repeat(indent), pad),
            Span::styled(padded_tail(ctx.full_width, indent), pad),
        ]);
        ctx.paint(line);
    }

    // Render Article Markdown body without code gutters
    let heading_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.heading())
        .add_modifier(Modifier::BOLD);
    let quote_style = Style::default().bg(code_bg).fg(ctx.theme.dim());
    let quote_bar_style = Style::default().bg(code_bg).fg(ctx.theme.info());
    let bullet_style = Style::default().bg(code_bg).fg(ctx.theme.brand());
    let text_style = Style::default().bg(code_bg).fg(ctx.theme.code_text());

    let mut in_code_block = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            let mut spans = vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(
                    line.to_string(),
                    Style::default().bg(code_bg).fg(ctx.theme.dim()),
                ),
            ];
            let used = indent + line.width();
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint(Line::from(spans));
            continue;
        }

        if in_code_block {
            let snip_indent = indent + 2;
            let mut spans = vec![
                Span::styled(" ".repeat(snip_indent), pad),
                Span::styled(line.to_string(), text_style),
            ];
            let used = snip_indent + line.width();
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint(Line::from(spans));
            continue;
        }

        if trimmed.is_empty() {
            let line = Line::from(vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(padded_tail(ctx.full_width, indent), pad),
            ]);
            ctx.paint(line);
            continue;
        }

        if trimmed.starts_with('#') {
            let wrapped = nonempty_wrapped(wrap_text(trimmed, inner_w.max(1)));
            for wl in &wrapped {
                let mut spans = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(wl.text.clone(), heading_style),
                ];
                let used = indent + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        } else if let Some(quote_content) = trimmed.strip_prefix('>') {
            let quote_trimmed = quote_content.trim();
            let avail_w = inner_w.saturating_sub(4).max(1);
            let wrapped = nonempty_wrapped(wrap_text(quote_trimmed, avail_w));
            for wl in &wrapped {
                // Share the transcript's blockquote gutter glyph so a quoted
                // run reads identically in an assistant turn and in a rendered
                // web-article payload.
                let mut spans = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(
                        format!("{} ", nuo_ui_primitives::design::QUOTE_GUTTER_GLYPH),
                        quote_bar_style,
                    ),
                    Span::styled(wl.text.clone(), quote_style),
                ];
                let used = indent + 2 + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let bullet_item = &trimmed[2..];
            let avail_w = inner_w.saturating_sub(3).max(1);
            let wrapped = nonempty_wrapped(wrap_text(bullet_item, avail_w));
            for (i, wl) in wrapped.iter().enumerate() {
                let mut spans = vec![Span::styled(" ".repeat(indent), pad)];
                let used = if i == 0 {
                    spans.push(Span::styled("• ", bullet_style));
                    spans.push(Span::styled(wl.text.clone(), text_style));
                    indent + 2 + wl.text.width()
                } else {
                    spans.push(Span::styled("  ", pad));
                    spans.push(Span::styled(wl.text.clone(), text_style));
                    indent + 2 + wl.text.width()
                };
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        } else {
            let wrapped = nonempty_wrapped(wrap_text(trimmed, inner_w.max(1)));
            for wl in &wrapped {
                let mut spans = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(wl.text.clone(), text_style),
                ];
                let used = indent + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        }
    }

    if truncated {
        let note = "[... article body truncated to fit context budget — request specific sections if needed ...]";
        let mut spans = vec![
            Span::styled(" ".repeat(indent + 2), pad),
            Span::styled(note, Style::default().bg(code_bg).fg(ctx.theme.warn())),
        ];
        let used = indent + 2 + note.len();
        spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
        ctx.paint(Line::from(spans));
    }
}

pub(crate) fn parse_fallback_web_search(
    output: &str,
    arguments: &str,
) -> (String, String, Vec<nuo_protocol::WebSearchHit>, bool) {
    let query = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| {
            v.get("query")
                .and_then(|q| q.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    let provider = if let Some(idx) = output.find("(via ") {
        let rest = &output[idx + 5..];
        rest.split(')').next().unwrap_or("Web").to_string()
    } else {
        "Web".to_string()
    };

    let mut hits = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_url: Option<String> = None;
    let mut current_snippet = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Search results for") || trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("[... ") && trimmed.ends_with("...]") {
            continue;
        }
        let is_numbered = trimmed.chars().next().is_some_and(|c| c.is_ascii_digit())
            && trimmed.find(". ").is_some();
        if is_numbered {
            if let (Some(title), Some(url)) = (current_title.take(), current_url.take()) {
                let domain = extract_url_host(&url);
                hits.push(nuo_protocol::WebSearchHit {
                    title,
                    url,
                    domain,
                    snippet: current_snippet.join(" "),
                });
                current_snippet.clear();
            }
            let title = trimmed
                .split_once(". ")
                .map(|x| x.1)
                .unwrap_or(trimmed)
                .to_string();
            current_title = Some(title);
        } else if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
            && current_url.is_none()
        {
            current_url = Some(trimmed.to_string());
        } else if current_title.is_some() {
            current_snippet.push(trimmed);
        }
    }

    if let (Some(title), Some(url)) = (current_title, current_url) {
        let domain = extract_url_host(&url);
        hits.push(nuo_protocol::WebSearchHit {
            title,
            url,
            domain,
            snippet: current_snippet.join(" "),
        });
    }

    let truncated = output.contains("more results omitted to fit");
    (query, provider, hits, truncated)
}

pub(crate) fn parse_fallback_web_article(
    output: &str,
    arguments: &str,
) -> (String, Option<String>, String, String, String, usize, bool) {
    let url = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let domain = extract_url_host(&url);

    let mut cleaned = output;
    if let Some(idx) = cleaned.find("[BEGIN UNTRUSTED WEB CONTENT")
        && let Some(nl) = cleaned[idx..].find('\n')
    {
        cleaned = &cleaned[idx + nl + 1..];
    }
    if let Some(idx) = cleaned.rfind("[END UNTRUSTED WEB CONTENT]") {
        cleaned = &cleaned[..idx];
    }
    let trimmed = cleaned.trim();
    let tokens = nuo_protocol::tokenizer::count_tokens(trimmed);
    let truncated = output.contains("kept the first") || output.contains("truncated to fit");
    (
        url,
        None,
        domain,
        trimmed.to_string(),
        "Reader".to_string(),
        tokens,
        truncated,
    )
}

fn extract_url_host(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|x| x.1).unwrap_or(url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let host = authority.split('@').next_back().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        "web".to_string()
    } else {
        host.to_string()
    }
}
