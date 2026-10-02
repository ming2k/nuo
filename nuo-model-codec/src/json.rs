//! Small JSON framing helpers shared by provider adapters and agent text-tool-call parsing.

/// Given the byte index of an opening `{` in `text`, return the byte index of
/// the matching closing `}` at the same nesting depth.
///
/// String literals and escapes are respected, so braces inside strings do not
/// affect nesting. Returns `None` when `start` is not an opening brace or the
/// object never balances.
pub fn find_balanced_object(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return None;
    }

    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'{' {
            depth += 1;
        } else if byte == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn balances_nested_objects_and_quoted_braces() {
        let text = r#"prefix {"nested":{"text":"} and \"{\""}} suffix"#;
        let start = text.find('{').unwrap();
        let end = find_balanced_object(text, start).unwrap();
        assert_eq!(&text[start..=end], r#"{"nested":{"text":"} and \"{\""}}"#);
    }

    #[test]
    fn rejects_non_object_starts_and_unbalanced_input() {
        assert_eq!(find_balanced_object("[]", 0), None);
        assert_eq!(find_balanced_object("{\"x\":{", 0), None);
    }
}
