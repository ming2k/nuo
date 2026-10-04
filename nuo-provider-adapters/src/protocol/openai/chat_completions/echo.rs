//! OpenAI-compatible — tool-call "echo" suppression.
//!
//! Models such as GLM/Qwen return native `tool_calls` *and* mirror the call as
//! text in `delta.content`, wrapped in sentinel tokens. That mirror is not
//! assistant prose. [`ToolCallEchoFilter`] suppresses it before it ever becomes
//! a [`ProviderStreamEvent::TextDelta`], so the UI never flickers and the
//! harness needs no after-the-fact retraction.
//!
//! This is a stateful-but-pure component: it accumulates deltas across a stream
//! and resolves them at the end against whether native tool calls arrived. It
//! performs no I/O.

use nuo_model_codec::ProviderStreamEvent;
use serde_json::Value;

/// Sentinel tokens that wrap a tool call when a model mirrors it as text
/// content alongside native `tool_calls` (ChatML/Hermes/Qwen style), e.g.
/// `{"tool":"bash",...}<|tool_calls_section_end|>`.
const TOOL_CALL_SENTINELS: &[&str] = &[
    "<|tool_calls_section_begin|>",
    "<|tool_calls_section_end|>",
    "<|tool_calls_begin|>",
    "<|tool_calls_end|>",
    "<|tool_call_begin|>",
    "<|tool_call_end|>",
    "<|tool's_call_begin|>",
    "<|tool's_call_end|>",
    "<tool_call>",
    "</tool_call>",
];

/// Maximum bytes of `{`-prefixed content to buffer while deciding whether it is
/// a tool-call echo. Real tool calls are far smaller; exceeding this flushes
/// the buffer as ordinary text so a large legitimate JSON response is not held
/// back indefinitely.
const MAX_ECHO_BUFFER: usize = 8192;

/// Streaming filter that strips tool-call "echo" text from a content channel.
///
/// Content is treated as an echo when it contains a sentinel token, or when it
/// is nothing but JSON object(s) carrying a `tool`/`name` key (with optional
/// surrounding whitespace). Everything else passes through unchanged; sentinel
/// tokens split across deltas are still recognised.
pub struct ToolCallEchoFilter {
    /// Unclassified text: may still be the prefix of a sentinel token or an
    /// incomplete JSON object.
    pending: String,
    /// Text classified as a tool-call echo, held until the stream ends. Whether
    /// it is dropped depends on whether native `tool_calls` also arrived: with
    /// them it is a redundant mirror (drop); without them it is a real
    /// text-emitted tool call the harness must still parse (emit).
    held: String,
    /// In hold mode: every subsequent delta appends to `held`.
    echo: bool,
    /// Set when the stream produced at least one native `ToolCallDelta` — the
    /// decision input for [`ToolCallEchoFilter::finish`].
    had_native_tool_calls: bool,
    /// Diagnostics accumulated across the stream: chars fed vs emitted (their
    /// difference is what the filter suppressed), plus reasoning/tool-call
    /// traffic. Logged once at stream end so an "empty assistant response" can
    /// be traced to its cause.
    pub fed_chars: usize,
    pub emitted_chars: usize,
    pub reasoning_chars: usize,
    pub tool_call_deltas: usize,
}

impl Default for ToolCallEchoFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolCallEchoFilter {
    pub fn new() -> Self {
        Self {
            pending: String::new(),
            held: String::new(),
            echo: false,
            had_native_tool_calls: false,
            fed_chars: 0,
            emitted_chars: 0,
            reasoning_chars: 0,
            tool_call_deltas: 0,
        }
    }

    /// Observe a stream event, updating bookkeeping and returning any
    /// [`ProviderStreamEvent::TextDelta`] safe to emit now. Text deltas are fed
    /// through `feed`; reasoning/tool-call/usage deltas pass
    /// through untouched (the latter two flip the "had native tool calls" flag
    /// so a trailing echo is dropped at [`finish`](Self::finish)).
    pub fn observe(&mut self, event: ProviderStreamEvent) -> Vec<ProviderStreamEvent> {
        match event {
            ProviderStreamEvent::ModelCatalogEtag(etag) => {
                vec![ProviderStreamEvent::ModelCatalogEtag(etag)]
            }
            ProviderStreamEvent::TextDelta(text) => {
                let emitted = self.feed(&text);
                if emitted.is_empty() {
                    Vec::new()
                } else {
                    vec![ProviderStreamEvent::TextDelta(emitted)]
                }
            }
            ProviderStreamEvent::ReasoningDelta(delta) => {
                self.reasoning_chars += delta.len();
                vec![ProviderStreamEvent::ReasoningDelta(delta)]
            }
            ProviderStreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => {
                self.tool_call_deltas += 1;
                self.had_native_tool_calls = true;
                vec![ProviderStreamEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments,
                }]
            }
            ProviderStreamEvent::Usage(usage) => {
                // Metadata, not content: pass through untouched.
                vec![ProviderStreamEvent::Usage(usage)]
            }
            ProviderStreamEvent::Completed(meta) => {
                vec![ProviderStreamEvent::Completed(meta)]
            }
        }
    }

    /// Feed a content delta; returns the text safe to emit now. Tool-call-shaped
    /// content is *held* (not dropped) until [`finish`](Self::finish) resolves
    /// it against whether native tool calls arrived.
    fn feed(&mut self, delta: &str) -> String {
        self.fed_chars += delta.len();
        if self.echo {
            self.held.push_str(delta);
            return String::new();
        }
        self.pending.push_str(delta);

        // A sentinel token anywhere means the content is a tool-call section —
        // hold it for the stream-end decision.
        if TOOL_CALL_SENTINELS
            .iter()
            .any(|token| self.pending.contains(token))
        {
            self.enter_hold();
            return String::new();
        }

        let trimmed = self.pending.trim_start();
        if trimmed.starts_with('{') {
            let brace = self.pending.len() - trimmed.len();
            return self.classify_json_prefix(brace);
        }

        // Ordinary prose: emit everything that cannot be the start of a
        // sentinel token, holding a short ASCII tail back so a sentinel split
        // across two deltas is still recognised on the next call.
        let emit = prose_emit_len(&self.pending);
        if emit > 0 {
            let out = self.pending[..emit].to_string();
            self.pending = self.pending[emit..].to_string();
            self.emitted_chars += out.len();
            return out;
        }
        String::new()
    }

    /// Resolve for the non-streaming `chat` path: filter the assembled content
    /// given whether native tool calls arrived. Echo text is dropped only when
    /// native tool calls were also produced.
    pub fn filter_content(content: &str, had_native_tool_calls: bool) -> String {
        let mut filter = Self::new();
        let mut out = filter.feed(content);
        filter.had_native_tool_calls = had_native_tool_calls;
        out.push_str(&filter.finish());
        out
    }

    /// Move `pending` into `held` and enter hold mode.
    fn enter_hold(&mut self) {
        self.held.push_str(&self.pending);
        self.pending.clear();
        self.echo = true;
    }

    /// Flush whatever remains once the stream ends. Held echo text is dropped
    /// only when native tool calls were also produced (it was a redundant
    /// mirror); otherwise it is emitted so the harness can parse a text
    /// tool-call fallback. Returns the text to emit, if any.
    pub fn finish(&mut self) -> String {
        if self.echo {
            if self.had_native_tool_calls {
                self.held.clear();
                return String::new();
            }
            let out = std::mem::take(&mut self.held);
            self.emitted_chars += out.len();
            return out;
        }
        let out = std::mem::take(&mut self.pending);
        self.emitted_chars += out.len();
        out
    }

    /// `self.pending[brace..]` begins with `{`. If the object is complete,
    /// classify it; otherwise keep buffering (or flush if it has grown too
    /// large to plausibly be a tool call).
    fn classify_json_prefix(&mut self, brace: usize) -> String {
        match crate::json::find_balanced_object(&self.pending, brace) {
            Some(end) => {
                let candidate = &self.pending[brace..=end];
                let is_tool_call = serde_json::from_str::<Value>(candidate)
                    .map(|value| {
                        value
                            .get("tool")
                            .or_else(|| value.get("name"))
                            .and_then(|node| node.as_str())
                            .is_some()
                    })
                    .unwrap_or(false);
                if is_tool_call {
                    // Hold everything; the stream-end decision resolves mirror
                    // vs real text tool call.
                    self.enter_hold();
                    String::new()
                } else {
                    // Valid JSON but not a tool call — ordinary content.
                    let out = std::mem::take(&mut self.pending);
                    self.emitted_chars += out.len();
                    out
                }
            }
            None => {
                if self.pending.len() > MAX_ECHO_BUFFER {
                    let out = std::mem::take(&mut self.pending);
                    self.emitted_chars += out.len();
                    out
                } else {
                    String::new()
                }
            }
        }
    }
}

/// Largest prefix length of `pending` that is safe to emit now, retaining any
/// trailing suffix that could be the start of a sentinel token.
fn prose_emit_len(pending: &str) -> usize {
    let max_sentinel = TOOL_CALL_SENTINELS
        .iter()
        .map(|token| token.len())
        .max()
        .unwrap_or(0);
    let scan_from = pending.len().saturating_sub(max_sentinel);
    let bytes = pending.as_bytes();
    let mut cursor = scan_from;
    while cursor < bytes.len() {
        if pending.is_char_boundary(cursor) {
            let suffix = &pending[cursor..];
            if TOOL_CALL_SENTINELS
                .iter()
                .any(|token| token.starts_with(suffix))
            {
                return cursor;
            }
        }
        cursor += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a sequence of content deltas through an echo filter and return
    /// `(surviving_text, echo_flag)` — mirroring how `stream_chat_events`
    /// feeds deltas and then resolves at stream end given whether native
    /// `tool_calls` also arrived.
    fn run_echo_filter(deltas: &[&str], native_tool_calls: bool) -> (String, bool) {
        let mut filter = ToolCallEchoFilter::new();
        let mut out = String::new();
        for delta in deltas {
            out.push_str(&filter.feed(delta));
        }
        filter.had_native_tool_calls = native_tool_calls;
        out.push_str(&filter.finish());
        (out, filter.echo)
    }

    #[test]
    fn drops_mirror_when_native_tool_calls_present() {
        let (out, echo) = run_echo_filter(
            &[
                "{\"tool\":\"bash\",\"arguments\":{\"command\":\"git show 493588a\"}}",
                "<|tool_calls_section_end|>",
            ],
            true,
        );
        assert!(echo, "should be classified as an echo");
        assert!(out.is_empty(), "mirror must be dropped: got {out:?}");
    }

    #[test]
    fn drops_multi_argument_tool_call_mirror() {
        let (out, echo) = run_echo_filter(
            &[
                "{\"tool\":\"edit_text\",\"arguments\":{\"path\":\"docs/adr/0001-tool-rendering-redesign.md\",\"old_string\":\"- Status: Accepted\",\"new_string\":\"- Status: Implemented\"}}",
                "<|tool_calls_section_end|>",
            ],
            true,
        );
        assert!(echo);
        assert!(out.is_empty(), "got {out:?}");
    }

    #[test]
    fn drops_bare_json_mirror_when_native_calls_present() {
        let (out, echo) = run_echo_filter(&["{\"name\":\"read_text\",\"arguments\":{}}"], true);
        assert!(echo);
        assert!(out.is_empty(), "got {out:?}");
    }

    #[test]
    fn buffers_until_json_completes_no_flicker() {
        let (out, echo) = run_echo_filter(&["{\"too", "l\":\"bash\",\"arguments\":{}}"], true);
        assert!(echo);
        assert!(out.is_empty(), "got {out:?}");
    }

    #[test]
    fn recognises_sentinel_split_across_deltas() {
        let (out, echo) = run_echo_filter(
            &[
                "<|tool_calls_secti",
                "on_end|>",
                "{\"tool\":\"bash\",\"arguments\":{}}",
            ],
            true,
        );
        assert!(echo);
        assert!(out.is_empty(), "got {out:?}");
    }

    #[test]
    fn restores_text_fallback_when_no_native_calls() {
        let (out, echo) = run_echo_filter(
            &["{\"tool\":\"bash\",\"arguments\":{\"command\":\"ls\"}}<|tool_calls_section_end|>"],
            false,
        );
        assert!(echo, "still classified as tool-call-shaped");
        assert!(
            !out.is_empty() && out.contains("\"tool\":\"bash\""),
            "text tool call must be restored when no native calls: got {out:?}"
        );
    }

    #[test]
    fn passes_through_plain_prose() {
        let (out, echo) = run_echo_filter(&["Let me read that file ", "for you."], false);
        assert!(!echo);
        assert_eq!(out, "Let me read that file for you.");
    }

    #[test]
    fn keeps_prose_with_embedded_non_tool_json() {
        let (out, echo) = run_echo_filter(&["Here is data: {\"key\":42} done"], false);
        assert!(!echo);
        assert_eq!(out, "Here is data: {\"key\":42} done");
    }

    #[test]
    fn holds_everything_once_a_tool_call_is_seen() {
        let (out, echo) = run_echo_filter(
            &["{\"tool\":\"bash\",\"arguments\":{}} now running it"],
            true,
        );
        assert!(echo);
        assert!(
            out.is_empty(),
            "held content is dropped when native calls arrive: got {out:?}"
        );

        let (out, echo) = run_echo_filter(
            &["{\"tool\":\"bash\",\"arguments\":{}} now running it"],
            false,
        );
        assert!(echo);
        assert_eq!(out, "{\"tool\":\"bash\",\"arguments\":{}} now running it");
    }
}
