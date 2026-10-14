use crate::message::Message;

/// Fast, zero-network token estimator calibrated for modern LLM tokenizers (BPE / tiktoken).
pub struct TokenCounter;

impl TokenCounter {
    /// Estimates token count for a raw string.
    /// Accounts for CJK characters (~1.5 chars/token) and ASCII/Latin words (~3.5-4 chars/token).
    pub fn estimate_str(s: &str) -> usize {
        if s.is_empty() {
            return 0;
        }

        let mut cjk_chars = 0;
        let mut ascii_chars = 0;
        let mut other_chars = 0;

        for ch in s.chars() {
            let u = ch as u32;
            // CJK Unified Ideographs block
            if (0x4E00..=0x9FFF).contains(&u) || (0x3400..=0x4DBF).contains(&u) {
                cjk_chars += 1;
            } else if ch.is_ascii() {
                ascii_chars += 1;
            } else {
                other_chars += 1;
            }
        }

        let cjk_tokens = (cjk_chars as f64 * 0.75).ceil() as usize;
        let ascii_tokens = (ascii_chars as f64 / 3.8).ceil() as usize;
        let other_tokens = (other_chars as f64 * 0.8).ceil() as usize;

        cjk_tokens + ascii_tokens + other_tokens
    }

    /// Estimates token usage of a single message.
    pub fn estimate_message(msg: &Message) -> usize {
        let mut tokens = 4; // Envelope overhead per message
        tokens += Self::estimate_str(&msg.content);

        for tc in &msg.tool_calls {
            tokens += 6; // Tool call envelope
            tokens += Self::estimate_str(&tc.name);
            tokens += Self::estimate_str(&tc.arguments.to_string());
        }

        for tr in &msg.tool_results {
            tokens += 6; // Tool result envelope
            tokens += Self::estimate_str(&tr.name);
            tokens += Self::estimate_str(&tr.output);
        }

        tokens
    }

    /// Estimates total tokens for a slice of messages.
    pub fn estimate_messages(messages: &[Message]) -> usize {
        messages.iter().map(Self::estimate_message).sum::<usize>() + 2 // Conversation priming tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_costs_nothing() {
        assert_eq!(TokenCounter::estimate_str(""), 0);
    }

    #[test]
    fn latin_text_is_estimated_within_a_reasonable_band() {
        let text = "Hello world! This is a test message to count tokens.";
        let count = TokenCounter::estimate_str(text);
        assert!((10..=20).contains(&count), "unexpected estimate: {count}");
    }

    #[test]
    fn cjk_text_is_estimated_within_a_reasonable_band() {
        // The estimator must not treat CJK as ASCII: 15 CJK characters cost far
        // more than 15 Latin characters would.
        let cjk = "你好，世界！这是一个测试用例。";
        let count = TokenCounter::estimate_str(cjk);
        assert!((8..=16).contains(&count), "unexpected estimate: {count}");

        let same_length_latin = "a".repeat(cjk.chars().count());
        assert!(
            count > TokenCounter::estimate_str(&same_length_latin),
            "CJK must cost more per character than ASCII"
        );
    }

    #[test]
    fn message_estimation_includes_tool_payloads() {
        let plain = Message::assistant("hello");
        let with_tool = Message::assistant_with_tools(
            "hello",
            vec![crate::message::ToolCall {
                id: "1".into(),
                name: "search".into(),
                arguments: serde_json::json!({"query": "rust agent protocols"}),
            }],
        );
        assert!(
            TokenCounter::estimate_message(&with_tool) > TokenCounter::estimate_message(&plain),
            "tool-call payload must add to the estimate"
        );
    }
}
