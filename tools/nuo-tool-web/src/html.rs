/// Naive HTML → text conversion. Collapses whitespace and strips tags/scripts.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut skip = false;
    let lower = html.to_ascii_lowercase();
    let mut chars = html.char_indices().peekable();
    while let Some((byte_idx, c)) = chars.next() {
        if !in_tag && lower[byte_idx..].starts_with("<script") {
            skip = true;
        } else if skip && lower[byte_idx..].starts_with("</script") {
            skip = false;
            // jump to end of tag
            if let Some(idx) = lower[byte_idx..].find('>') {
                let next_byte = byte_idx + idx + 1;
                while chars
                    .peek()
                    .is_some_and(|(peek_byte, _)| *peek_byte < next_byte)
                {
                    chars.next();
                }
                continue;
            }
        } else if !in_tag && lower[byte_idx..].starts_with("<style") {
            skip = true;
        } else if skip && lower[byte_idx..].starts_with("</style") {
            skip = false;
            if let Some(idx) = lower[byte_idx..].find('>') {
                let next_byte = byte_idx + idx + 1;
                while chars
                    .peek()
                    .is_some_and(|(peek_byte, _)| *peek_byte < next_byte)
                {
                    chars.next();
                }
                continue;
            }
        }
        if skip {
            continue;
        }
        if c == '<' {
            in_tag = true;
        } else if c == '>' && in_tag {
            in_tag = false;
            out.push(' ');
        } else if !in_tag {
            out.push(c);
        }
    }
    // Decode a handful of common entities
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let mut collapsed = String::with_capacity(decoded.len());
    let mut prev_ws = false;
    for c in decoded.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                collapsed.push(' ');
                prev_ws = true;
            }
        } else {
            collapsed.push(c);
            prev_ws = false;
        }
    }
    collapsed.trim().to_string()
}

pub fn extract_html_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(open) = lower.find("<title") else {
        return String::new();
    };
    let Some(start_offset) = lower[open..].find('>') else {
        return String::new();
    };
    let start = open + start_offset + 1;
    let Some(end_offset) = lower[start..].find("</title>") else {
        return String::new();
    };
    html_to_text(&html[start..start + end_offset])
}
