//! Small JSON framing helpers shared by provider adapters and the agent's
//! text-tool-call compatibility path. Standardized on `nuo-model-codec`.

pub use nuo_model_codec::find_balanced_object;

#[cfg(test)]
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
    fn rejects_unbalanced_or_missing_brace() {
        assert_eq!(find_balanced_object("no brace", 0), None);
        assert_eq!(find_balanced_object("{unclosed", 0), None);
    }
}
