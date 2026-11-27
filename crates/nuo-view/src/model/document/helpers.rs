//! Formatting, duration, milestone extraction, and argument helpers.

/// Strip control characters (except \n, \t) to prevent Ratatui from rendering
/// them as block characters (█).
pub(crate) fn sanitize_text(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(|c: char| c.is_control() && c != '\n' && c != '\t') {
        std::borrow::Cow::Owned(
            text.replace(|c: char| c.is_control() && c != '\n' && c != '\t', ""),
        )
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Parse a JSON arguments string into ordered `(key, display_value)` pairs
/// suitable for compact rendering in the tool step body.
///
/// String values are shown unquoted; other JSON types keep their native
/// representation. Incomplete or malformed JSON never falls back to raw syntax dumping (ADR-0026, ADR-0046).
pub fn parse_arguments_kv(arguments: &str) -> Vec<(String, String)> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if let Ok(serde_json::Value::Object(object)) =
        serde_json::from_str::<serde_json::Value>(trimmed)
    {
        return object
            .into_iter()
            .map(|(key, val)| {
                let display = match val {
                    serde_json::Value::String(s) => s,
                    _ => val.to_string(),
                };
                (key, display)
            })
            .collect();
    }
    // Resilient streaming projection (ADR-0046): extract only closed, valid key-value pairs.
    // Unclosed, broken syntax is never leaked as a raw string into the view layer.
    let streaming = crate::tools::parse_streaming_arguments(trimmed);
    streaming
        .into_iter()
        .map(|(key, val)| {
            let display = match val {
                serde_json::Value::String(s) => s,
                _ => val.to_string(),
            };
            (key, display)
        })
        .collect()
}

pub(crate) fn duration_text(duration_ms: Option<u64>) -> String {
    match duration_ms {
        None => "...".to_string(),
        Some(ms) if ms < 1000 => format!("{}ms", ms),
        Some(ms) if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        Some(ms) => {
            let total_secs = ms / 1000;
            let h = total_secs / 3600;
            let m = (total_secs % 3600) / 60;
            let s = total_secs % 60;
            if h > 0 {
                format!("{}h {}m", h, m)
            } else {
                format!("{}m {}s", m, s)
            }
        }
    }
}

/// Clamp `value` to `max_chars` characters, keeping its head.
///
/// Routed through the substrate's overflow vocabulary so a document label and a
/// modal label degrade identically (ADR-0152 `[INV-TEXT-OVERFLOW-01]`). The
/// budget is in characters here, which for the ASCII labels this clamps is the
/// same as columns.
pub(crate) fn truncate(value: &str, max_chars: usize) -> String {
    nuo_ui_primitives::components::overflow::fit(
        value,
        max_chars,
        nuo_ui_primitives::components::overflow::Overflow::End,
    )
    .text
}

/// Extract the latest active milestone or heading from reasoning content.
pub fn extract_active_milestone(text: &str) -> Option<String> {
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("**") {
            let title = if let Some(end) = rest.find("**") {
                &rest[..end]
            } else {
                rest
            };
            let clean = title.trim().trim_matches(':').trim();
            if !clean.is_empty() {
                return Some(clean.to_string());
            }
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            let header = rest.trim_start_matches('#').trim();
            if !header.is_empty() {
                return Some(header.to_string());
            }
        }
    }
    None
}

/// Canonical normalization for thinking milestone topics.
///
/// Strips redundant action participles (e.g. "Deconstructing", "Analyzing",
/// "Evaluating") and normalizes casing while preserving acronyms and mixed-case
/// identifiers (API, SQL, OAuth, macOS). No article is injected: the milestone
/// heading is already the topic, so forcing `the` in front of it produces odd
/// output for code-like headings (`the derive(Clone, copy, debug, PartialEq,
/// eq)`). The summary's verb ("Thinking through" / "Thought through") supplies
/// the grammatical frame.
pub fn normalize_thinking_topic(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches(':').trim();
    if trimmed.is_empty() {
        return "the solution".to_string();
    }

    // Strip trailing ellipsis or punctuation
    let topic = trimmed.trim_end_matches(['.', '…', ':']).trim();

    // Redundant participle prefixes that models prepend to milestone headings
    const REDUNDANT_PREFIXES: &[&str] = &[
        "deconstructing ",
        "analyzing ",
        "evaluating ",
        "inspecting ",
        "reviewing ",
        "mapping out ",
        "breaking down ",
        "thinking about ",
        "thinking through ",
        "working on ",
        "looking into ",
        "investigating ",
        "exploring ",
        "examining ",
        "considering ",
        "understanding ",
        "identifying ",
        "determining ",
        "formulating ",
        "planning ",
        "executing ",
        "validating ",
        "verifying ",
        "checking ",
    ];

    let mut stripped = topic;
    for prefix in REDUNDANT_PREFIXES {
        // `get` returns None when the prefix length lands mid-UTF-8-char;
        // byte-length slicing (`stripped[..prefix.len()]`) would panic there.
        let head = match stripped.get(..prefix.len()) {
            Some(head) => head,
            None => continue,
        };
        if stripped.len() > prefix.len() && head.eq_ignore_ascii_case(prefix) {
            let candidate = stripped[prefix.len()..].trim();
            if !candidate.is_empty() {
                stripped = candidate;
                break;
            }
        }
    }

    let words: Vec<&str> = stripped.split_whitespace().collect();
    if words.is_empty() {
        return "the solution".to_string();
    }

    let mut normalized_words = Vec::with_capacity(words.len());
    for (i, word) in words.iter().enumerate() {
        let is_acronym = word.len() > 1
            && word.chars().any(|c| c.is_alphabetic())
            && word.chars().all(|c| c.is_uppercase() || !c.is_alphabetic());
        let has_internal_caps = word.chars().skip(1).any(|c| c.is_uppercase());

        if is_acronym || has_internal_caps {
            normalized_words.push(word.to_string());
        } else if i == 0 {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                let lower = first.to_lowercase().to_string() + chars.as_str();
                normalized_words.push(lower);
            }
        } else {
            let is_capitalized = word.chars().next().is_some_and(|c| c.is_uppercase());
            let is_all_caps = word.chars().all(|c| c.is_uppercase() || !c.is_alphabetic());
            if is_capitalized && !is_all_caps {
                normalized_words.push(word.to_lowercase());
            } else {
                normalized_words.push(word.to_string());
            }
        }
    }

    // No article is injected here: the milestone heading is already the topic,
    // and prefixing `the` reads oddly for code-like identifiers
    // (`the derive(Clone, Copy, Debug)`). The summary's verb ("Thinking
    // through" / "Thought through") supplies the grammatical frame.
    normalized_words.join(" ")
}

/// Count distinct milestone/heading sections in reasoning content.
pub fn count_milestones(text: &str) -> usize {
    let mut count = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("**") && trimmed.len() > 4 {
            let rest = &trimmed[2..];
            if rest.contains("**") {
                count += 1;
            }
        } else if trimmed.starts_with('#') {
            let header = trimmed.trim_start_matches('#').trim();
            if !header.is_empty() {
                count += 1;
            }
        }
    }
    count
}
