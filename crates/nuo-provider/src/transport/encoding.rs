//! Universal URL component and `application/x-www-form-urlencoded` encoding.

/// Percent-encode a single URL component (path segment, query parameter, form value).
pub fn encode_component(input: &str) -> String {
    percent_encode(input)
}

/// Percent-encode an ASCII/UTF-8 string per WHATWG URL specification.
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Encode key-value pairs into `application/x-www-form-urlencoded` format.
pub fn form_encode(fields: &[(String, String)]) -> String {
    let mut out = String::new();
    for (index, (name, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&percent_encode(name));
        out.push('=');
        out.push_str(&percent_encode(value));
    }
    out
}

/// Serialize borrowed string pairs into `application/x-www-form-urlencoded`.
pub fn percent_encode_form_pairs(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding_matches_standard() {
        assert_eq!(
            form_encode(&[
                ("grant_type".into(), "authorization_code".into()),
                ("code".into(), "a b+c/d".into()),
            ]),
            "grant_type=authorization_code&code=a+b%2Bc%2Fd"
        );
    }

    #[test]
    fn borrowed_pairs_match_owned() {
        let pairs = [("k", "a b/c"), ("x", "plain")];
        assert_eq!(percent_encode_form_pairs(&pairs), "k=a+b%2Fc&x=plain");
    }
}
