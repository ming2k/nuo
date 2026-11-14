//! Google Google — response parsing.
//!
//! Pure functions that turn Google's JSON response shape into the harness's
//! domain types. Google reports usage under `usageMetadata`
//! (`promptTokenCount` / `candidatesTokenCount` / `totalTokenCount`), and
//! content lives in `candidates[0].content.parts[]` as either `text` or
//! `functionCall`.
use nuo_provider::capability::ProviderStreamEvent;
use nuo_tool::ToolCall;
use nuo_tool::{Message, Role, TokenUsage};
use serde_json::{Map, Value};

/// Per-`functionCall` thought signatures, keyed by both call id and call name.
pub const THOUGHT_SIGNATURES_META_KEY: &str = "gemini_thought_signatures";
/// A thought signature minted on the answer's **visible text** part, replayable
/// on that same part because the part's bytes round-trip verbatim.
pub const TEXT_THOUGHT_SIGNATURE_META_KEY: &str = "gemini_text_thought_signature";
/// A **turn-level** thought signature — minted on the terminal zero-length text
/// part (or on one delta of a text part that the harness merges). It is not
/// replayable on a text part, but it is a valid signature for this turn's
/// function-call group, which is the shape the native client sends.
pub const TERMINAL_THOUGHT_SIGNATURE_META_KEY: &str = "gemini_terminal_thought_signature";
/// The wire identity that minted the signatures on this turn (`model` on the
/// Antigravity envelope; the model id on a native Google route). A signature is
/// only valid on the route that minted it, so the request builder compares this
/// against the route it is about to address.
pub const THOUGHT_SIGNATURE_ORIGIN_META_KEY: &str = "gemini_thought_signature_origin";

#[derive(Debug, Clone, Default)]
pub(crate) struct StreamPayload {
    pub events: Vec<ProviderStreamEvent>,
    pub thought_signatures: Vec<(String, String)>,
    /// A signature seen on a **non-empty, non-thought text part** of this
    /// payload. Whether it is replayable is a whole-stream question — see
    /// [`TextSignatureSites`].
    pub text_thought_signature: Option<String>,
    /// A signature on a zero-length text part, or on the candidate itself: the
    /// turn-level signature that may sign the turn's `functionCall` group.
    pub terminal_thought_signature: Option<String>,
    /// Visible (non-thought) text parts in this payload.
    pub text_deltas: usize,
    /// How many of those carried a signature.
    pub signed_text_deltas: usize,
}

/// Accumulates where a stream's thought signatures were minted and decides what
/// may be replayed.
///
/// A Google thought signature validates against the exact bytes of the part
/// that minted it, so replaying it somewhere else is answered
/// `400 Corrupted thought signature.` — a *terminal* failure for the session,
/// not a degraded answer. The harness merges stream deltas into one answer
/// string, which means a signature minted on one delta is only replayable when
/// that delta *was* the whole answer. Everything else has exactly one
/// replayable home: the turn's leading `functionCall`, which is the shape the
/// native client itself sends.
///
/// Counting across the whole stream (rather than per payload) is the point:
/// per-payload resolution cannot tell a one-delta answer from the first delta of
/// a forty-delta answer.
#[derive(Debug, Default)]
pub(crate) struct TextSignatureSites {
    text_deltas: usize,
    signed_text_deltas: usize,
    /// The last signature minted on a non-empty text part.
    text_signature: Option<String>,
    /// The turn-level signature (terminal frame or candidate-level).
    pub turn_signature: Option<String>,
    /// The call ids this stream announced, in arrival order.
    pub call_keys: Vec<String>,
}

impl TextSignatureSites {
    /// Fold one parsed payload into the stream-level accounting.
    pub fn observe(&mut self, payload: &StreamPayload) {
        self.text_deltas += payload.text_deltas;
        self.signed_text_deltas += payload.signed_text_deltas;
        if let Some(signature) = payload.text_thought_signature.as_ref() {
            self.text_signature = Some(signature.clone());
        }
        if let Some(signature) = payload.terminal_thought_signature.as_ref() {
            self.turn_signature = Some(signature.clone());
        }
        for event in &payload.events {
            if let ProviderStreamEvent::ToolCallDelta {
                id: Some(id), name, ..
            } = event
            {
                if !id.is_empty() {
                    self.call_keys.push(id.clone());
                }
                if let Some(name) = name
                    && !name.is_empty()
                {
                    self.call_keys.push(name.clone());
                }
            }
        }
    }

    /// Resolve the replayable **text-part** signature, if any.
    ///
    /// `Some` only when the merged answer is byte-identical to the single signed
    /// delta — i.e. exactly one non-empty visible text part arrived and it
    /// carried the token. The presence of a *separate* turn-level signature does
    /// **not** disqualify it: this surface sends the terminal zero-length
    /// signature frame on essentially every answer, so gating on its absence
    /// would deny the text signature every time and needlessly demote a valid
    /// token to turn level (replaying a turn token on text is the very thing this
    /// fix exists to prevent).
    pub fn resolve(&self) -> ResolvedSignatureSites {
        let text_signature = (self.text_deltas == 1 && self.signed_text_deltas == 1)
            .then(|| self.text_signature.clone())
            .flatten();
        let turn_signature = self.turn_signature.clone().or_else(|| {
            // A signature minted on a delta that got merged away still signs
            // this turn; keeping it is what preserves native tool replay.
            if text_signature.is_none() {
                self.text_signature.clone()
            } else {
                None
            }
        });
        ResolvedSignatureSites {
            text_signature,
            turn_signature,
            call_keys: self.call_keys.clone(),
        }
    }
}

/// The replayable signature set of one completed turn.
#[derive(Debug, Default)]
pub(crate) struct ResolvedSignatureSites {
    pub text_signature: Option<String>,
    pub turn_signature: Option<String>,
    pub call_keys: Vec<String>,
}

/// Parse Google's `usageMetadata` into a [`TokenUsage`]. Returns `None` when
/// the object is absent or has no numeric fields. Google's implicit context
/// caching discount surfaces as `cachedContentTokenCount`; it is surfaced in
/// [`TokenUsage::cache_read_input_tokens`] so the token-source report shows the
/// hit rate. Google exposes no separate cache-write counter. Its hidden
/// reasoning budget surfaces as `thoughtsTokenCount` and is carried in
/// [`TokenUsage::reasoning_tokens`] (already included in
/// `candidatesTokenCount`, never an addition to it).
pub(crate) fn usage(usage: &Value) -> Option<TokenUsage> {
    let prompt = usage["promptTokenCount"].as_i64();
    let completion = usage["candidatesTokenCount"].as_i64();
    let total = usage["totalTokenCount"].as_i64();
    let reasoning = usage["thoughtsTokenCount"].as_i64();
    // Route cache-read accounting through the shared helper so the cache
    // policy is enforced in one place. Google hides the discount in
    // `cachedContentTokenCount`, which the helper reads.
    let cache = nuo_provider::read_prompt_cache_usage(usage);
    match (prompt, completion, total) {
        (Some(p), Some(c), _) => Some(TokenUsage {
            prompt_tokens: p,
            completion_tokens: c,
            total_tokens: total.unwrap_or(p + c),
            cache_creation_input_tokens: cache.write_tokens,
            cache_read_input_tokens: cache.read_tokens,
            cache_miss_input_tokens: cache.miss_tokens.unwrap_or(0),
            reasoning_tokens: reasoning.unwrap_or(0),
        }),
        _ => total.map(|t| TokenUsage {
            prompt_tokens: prompt.unwrap_or(0),
            completion_tokens: completion.unwrap_or(0),
            total_tokens: t,
            cache_creation_input_tokens: cache.write_tokens,
            cache_read_input_tokens: cache.read_tokens,
            cache_miss_input_tokens: cache.miss_tokens.unwrap_or(0),
            reasoning_tokens: reasoning.unwrap_or(0),
        }),
    }
}

/// Assemble the candidate content of a non-streaming `generateContent`
/// response into one assistant [`Message`]. Text parts become content and
/// `functionCall` parts become native tool calls. Errors when the response has
/// no candidates or no `parts` array.
pub(crate) fn message(response: &Value) -> Result<Message, String> {
    let root = response.get("response").unwrap_or(response);
    let candidates = root
        .get("candidates")
        .and_then(|c| c.as_array())
        .ok_or_else(|| format!("Invalid Google response: {}", response))?;

    if candidates.is_empty() {
        return Err("Google returned no candidates".to_string());
    }

    let parts = candidates[0]["content"]
        .get("parts")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "Missing parts in Google response".to_string())?;

    let mut content_text = String::new();
    let mut reasoning_text = String::new();
    let mut tool_calls = Vec::new();
    let mut thought_signatures = Map::new();
    // Signature sites are kept apart because they are NOT interchangeable:
    // a signature validates against the exact bytes of the part that minted
    // it, so it may only be replayed there (or, for a turn-level signature, on
    // the turn's leading `functionCall` — the shape the native client sends).
    let mut text_signed_parts = 0usize;
    let mut visible_text_parts = 0usize;
    let mut text_thought_signature = None;
    let mut terminal_thought_signature = None;
    for part in parts {
        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
            if part_is_thought(part) {
                // Reasoning text is never replayed (the request builder emits
                // no `thought` parts), so a signature minted here has no
                // byte-identical replay target. Recorded as reasoning only —
                // relocating it is what produced "Corrupted thought signature".
                reasoning_text.push_str(text);
            } else {
                if text.is_empty() {
                    // A zero-length visible text part contributes no bytes to the
                    // answer, so it is not counted as a text part: counting it
                    // would make a single signed part look like two and needlessly
                    // disqualify its signature.
                    if let Some(signature) = thought_signature(part) {
                        // The documented terminal frame: a zero-length text
                        // part carrying the turn's signature.
                        terminal_thought_signature = Some(signature);
                    }
                } else {
                    visible_text_parts += 1;
                    content_text.push_str(text);
                    if let Some(signature) = thought_signature(part) {
                        text_signed_parts += 1;
                        text_thought_signature = Some(signature);
                    }
                }
            }
        }
        if let Some(call) = part.get("functionCall").and_then(function_call) {
            if let Some(signature) = thought_signature(part) {
                thought_signatures.insert(call.id.clone(), Value::String(signature.clone()));
                thought_signatures.insert(call.name.clone(), Value::String(signature));
            }
            tool_calls.push(call);
        }
    }

    // The answer text is the concatenation of every non-empty visible text part,
    // so it is byte-identical to the signed part only when that part was the only
    // one. Otherwise the signature survives as a turn-level signature instead of
    // being stamped onto text whose bytes it never signed. A separate terminal
    // signature does not disqualify the text one: the two sign different things.
    let text_thought_signature = match (visible_text_parts, text_signed_parts) {
        (1, 1) => text_thought_signature,
        (_, 0) => None,
        _ => {
            terminal_thought_signature = terminal_thought_signature.or(text_thought_signature);
            None
        }
    };

    let mut message = Message::new(Role::Assistant, content_text);
    message.tool_calls = (!tool_calls.is_empty()).then_some(tool_calls);
    if !reasoning_text.is_empty() {
        message.reasoning_content = Some(reasoning_text);
    }
    if !thought_signatures.is_empty() {
        let provider_meta = message.provider_meta.get_or_insert_with(Map::new);
        provider_meta.insert(
            THOUGHT_SIGNATURES_META_KEY.to_string(),
            Value::Object(thought_signatures),
        );
    }
    let mut provider_meta = Map::new();
    if let Some(signature) = text_thought_signature {
        provider_meta.insert(
            TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
            Value::String(signature),
        );
    }
    if let Some(signature) = terminal_thought_signature {
        provider_meta.insert(
            TERMINAL_THOUGHT_SIGNATURE_META_KEY.to_string(),
            Value::String(signature),
        );
    }
    if !provider_meta.is_empty() {
        if let Some(existing) = message.provider_meta.as_mut() {
            existing.extend(provider_meta);
        } else {
            message.provider_meta = Some(provider_meta);
        }
    }
    Ok(message)
}

/// Parse one `streamGenerateContent` SSE payload and concatenate the text from
/// `candidates[0].content.parts[].text`. Returns an empty string when the
/// payload carries no text part (e.g. a finish-reason-only chunk).
pub(crate) fn stream_text(payload: &str) -> String {
    stream_events(payload)
        .into_iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::TextDelta(text) => Some(text),
            _ => None,
        })
        .collect()
}

/// Parse one `streamGenerateContent` SSE payload into normalized provider
/// events. Google may stream text, usage metadata, and whole `functionCall`
/// parts. Function calls are emitted as one delta per part because the REST
/// stream does not use OpenAI-style argument fragments.
pub(crate) fn stream_events(payload: &str) -> Vec<ProviderStreamEvent> {
    stream_payload(payload).events
}

/// Parse one `streamGenerateContent` SSE payload into normalized provider
/// events plus any Google thought signatures attached to function-call parts.
pub(crate) fn stream_payload(payload: &str) -> StreamPayload {
    // Single-parse entry point: callers holding an already-parsed
    // payload should prefer [`stream_payload_value`] to avoid re-parsing.
    match serde_json::from_str(payload) {
        Ok(value) => stream_payload_value(&value),
        Err(_) => StreamPayload::default(),
    }
}

/// Assemble the stream payload from an already-parsed JSON value — the
/// single-parse path used by the SSE executor.
///
/// Reports where this payload's signatures were minted; the
/// replayability decision belongs to [`TextSignatureSites`], which sees the whole
/// stream.
pub(crate) fn stream_payload_value(value: &Value) -> StreamPayload {
    let root = value.get("response").unwrap_or(value);
    let mut events = Vec::new();
    let mut thought_signatures = Vec::new();
    let mut text_deltas = 0usize;
    let mut signed_text_deltas = 0usize;
    let mut text_thought_signature = None;
    let mut terminal_thought_signature = None;
    if let Some(usage) = usage(&root["usageMetadata"]) {
        events.push(ProviderStreamEvent::Usage(usage));
    }
    if let Some(parts) = root
        .get("candidates")
        .and_then(|candidates| candidates.as_array())
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate["content"]["parts"].as_array())
    {
        let mut call_index = 0usize;
        for part in parts {
            if let Some(text) = part.get("text").and_then(|text| text.as_str()) {
                if let Some(signature) = thought_signature(part) {
                    if text.is_empty() {
                        // The documented terminal frame: a zero-length text
                        // part carrying this turn's signature. It signs the
                        // turn, not the answer text, so it is NOT replayable
                        // on a text part — but it IS the signature the turn's
                        // leading `functionCall` may carry.
                        terminal_thought_signature = Some(signature);
                    } else if !part_is_thought(part) {
                        signed_text_deltas += 1;
                        text_thought_signature = Some(signature);
                    }
                }
                if !text.is_empty() {
                    // Route reasoning summaries to ReasoningDelta so the harness
                    // surfaces them as a thinking trace, distinct from the answer.
                    if part_is_thought(part) {
                        events.push(ProviderStreamEvent::ReasoningDelta(text.to_string()));
                    } else {
                        text_deltas += 1;
                        events.push(ProviderStreamEvent::TextDelta(text.to_string()));
                    }
                }
            }
            if let Some(call) = part.get("functionCall").and_then(function_call) {
                if let Some(signature) = thought_signature(part) {
                    thought_signatures.push((call.id.clone(), signature));
                }
                events.push(ProviderStreamEvent::ToolCallDelta {
                    index: call_index,
                    id: Some(call.id),
                    name: Some(call.name),
                    arguments: call.arguments,
                });
                call_index += 1;
            }
        }
    }
    if terminal_thought_signature.is_none()
        && let Some(candidate) = root
            .get("candidates")
            .and_then(|candidates| candidates.as_array())
            .and_then(|candidates| candidates.first())
        && let Some(signature) = thought_signature(candidate)
    {
        terminal_thought_signature = Some(signature);
    }
    StreamPayload {
        events,
        thought_signatures,
        text_thought_signature,
        terminal_thought_signature,
        text_deltas,
        signed_text_deltas,
    }
}

fn function_call(value: &Value) -> Option<ToolCall> {
    let name = value.get("name")?.as_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));
    let arguments = value
        .get("args")
        .filter(|args| !args.is_null())
        .map(Value::to_string)
        .unwrap_or_else(|| "{}".to_string());
    Some(ToolCall {
        id,
        name,
        arguments,
    })
}

fn thought_signature(part: &Value) -> Option<String> {
    part.get("thoughtSignature")
        .or_else(|| part.get("thought_signature"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Whether this part is a Google reasoning *summary* — the model's thinking
/// text, surfaced only when the request set
/// `generationConfig.thinkingConfig.includeThoughts`. Such parts carry
/// `"thought": true` and MUST be routed to `ReasoningDelta` /
/// `reasoning_content`, never folded into the answer `content` (otherwise the
/// reasoning leaks into the visible reply).
fn part_is_thought(part: &Value) -> bool {
    part.get("thought")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether an upstream error rejects the `thinkingConfig` we stamped — i.e.
/// the request asked for `includeThoughts` / a `thinkingLevel` and the endpoint
/// answered that it does not accept that field **for this model**. This is the
/// "chain withheld" family of upstream behavior: a model served through a
/// legacy or filtered route may reason internally while refusing to disclose
/// the chain, and Google's error surfaces that as an `INVALID_ARGUMENT` naming
/// `thinkingConfig` (or one of its keys) / `include_thoughts` / `thought`.
///
/// Deliberately narrow: a bare 400 without a thinking-related token is some
/// other contract violation and must keep failing loudly. The match is
/// case-insensitive because relays re-case Google's field names freely, and it
/// accepts both the camelCase (`thinkingConfig`) and snake_case
/// (`thinking_config`) spellings for the same reason.
///
/// # Call this on the *upstream* message, never on the augmented one
///
/// [`clarify_error_with_shape`] appends guidance that itself mentions `thinkingConfig`,
/// `thinkingLevel` and `HTTP 400` (it has to — that is what makes a
/// content-free refusal actionable). Feed an augmented message to this
/// predicate and *every* 400 matches, so an unrelated contract violation (a bad
/// tool schema, a refused image) would trigger the thinking downgrade and latch
/// the channel into permanent chain-withholding.
///
/// [`rejects_thinking_config_error`] is the typed entry point that keeps the two
/// apart by construction: it reads the upstream text before any augmentation.
/// Prefer it over calling this directly.
pub fn rejects_thinking_config(error: &str) -> bool {
    let haystack = error.to_ascii_lowercase();
    let names_thinking = [
        "thinkingconfig",
        "thinking_config",
        "include_thoughts",
        "includethoughts",
        "thinkinglevel",
        "thinking_level",
        "thinkingbudget",
        "thinking_budget",
    ]
    .iter()
    .any(|needle| haystack.contains(needle));
    if !names_thinking {
        return false;
    }
    [
        "invalid_argument",
        "invalid argument",
        "http 400",
        "unknown name",
        "unsupported",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
}

/// The **typed** form of [`rejects_thinking_config`]: decide from the raw
/// upstream error, before this provider augments it.
///
/// This exists so the ordering cannot be got wrong at a call site. The
/// augmentation in [`clarify_error_with_shape`] deliberately mentions the
/// thinking surface (to make an otherwise content-free 400 actionable), so
/// classifying an already-augmented message would match every 400 and latch the
/// channel into chain-withholding for an unrelated fault. Taking the untouched
/// `ProviderError` makes "classify the upstream text" the only thing a caller
/// can express.
pub fn rejects_thinking_config_error(err: &nuo_provider::ProviderError) -> bool {
    rejects_thinking_config(err.message())
}

/// The salient facts of the request body this provider **actually sent**.
///
/// Google's `400 INVALID_ARGUMENT` is frequently uninformative — the canonical
/// shape is the bare `{"error":{"code":400,"message":"Request contains an
/// invalid argument.","status":"INVALID_ARGUMENT"}}`, which names no field and
/// carries no `fieldViolations`. A user then has nothing to act on: the notice
/// says "invalid argument" and stops.
///
/// The provider is the only layer that knows what it put on the wire, so it
/// captures those facts here and folds them into the error. This is *evidence*,
/// not inference: every field is read back off the body that was sent, so it
/// cannot describe a request other than the refused one (ADR-0074, ADR-0113).
///
/// Deliberately small and owned — no borrowed body, no `serde_json::Value`
/// clone of a multi-megabyte conversation. Only the shape axes that upstream
/// contract violations actually turn on are retained.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RequestShape {
    /// The wire model id in the envelope's `model` field. This can differ from
    /// the channel's model id (`gemini-3.8-flash` → `gemini-3.8-flash-tiered`),
    /// and the wire id is the one the upstream routes and rejects on.
    pub wire_model: String,
    /// The `generationConfig.thinkingConfig` object as stamped, compact JSON.
    /// `None` when the surface was omitted (a non-reasoning route, a Claude
    /// route, or a downgrade after a previous refusal).
    pub thinking_config: Option<String>,
    /// Number of `functionDeclarations` sent.
    pub tool_count: usize,
    /// Number of `contents[]` entries sent.
    pub content_count: usize,
    /// How many replayed `thoughtSignature` values the body carried, and how many
    /// of them were the documented bypass sentinel rather than a real token.
    ///
    /// This is the fact that attributes the `400 Corrupted thought signature`
    /// class of refusal, which names no field either: a signature only validates
    /// on the backend that minted it, so a replayed token from another route (or
    /// one stamped onto bytes it never signed) is the cause — and the count is
    /// what proves the request carried one at all.
    pub replayed_signatures: usize,
    pub bypass_signatures: usize,
}

impl RequestShape {
    /// Read the shape back off the body that was sent.
    pub fn of(wire_model: &str, body: &serde_json::Value) -> Self {
        let request = body.get("request").unwrap_or(body);
        let thinking_config = request
            .get("generationConfig")
            .and_then(|config| config.get("thinkingConfig"))
            .filter(|config| !config.is_null())
            .and_then(|config| serde_json::to_string(config).ok());
        let tool_count = request
            .get("tools")
            .and_then(|tools| tools.as_array())
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| tool.get("functionDeclarations"))
                    .filter_map(|decls| decls.as_array())
                    .map(Vec::len)
                    .sum()
            })
            .unwrap_or(0);
        let content_count = request
            .get("contents")
            .and_then(|contents| contents.as_array())
            .map(Vec::len)
            .unwrap_or(0);
        // Count the replayed signature tokens: the only in-band fact that can
        // attribute a `Corrupted thought signature` refusal.
        let mut replayed_signatures = 0usize;
        let mut bypass_signatures = 0usize;
        if let Some(contents) = request.get("contents").and_then(|c| c.as_array()) {
            for part in contents
                .iter()
                .filter_map(|entry| entry.get("parts"))
                .filter_map(|parts| parts.as_array())
                .flatten()
            {
                if let Some(signature) = part
                    .get("thoughtSignature")
                    .or_else(|| part.get("thought_signature"))
                    .and_then(|signature| signature.as_str())
                {
                    replayed_signatures += 1;
                    if signature == crate::request::SKIP_THOUGHT_SIGNATURE_VALIDATOR {
                        bypass_signatures += 1;
                    }
                }
            }
        }
        Self {
            wire_model: wire_model.to_string(),
            thinking_config,
            tool_count,
            content_count,
            replayed_signatures,
            bypass_signatures,
        }
    }

    /// The one-line-per-fact rendering folded into a request-refusal error.
    ///
    /// Phrased as a report of what was sent ("this request carried …"), never
    /// as a diagnosis, because the upstream did not tell us which field it
    /// objected to. A caller that guesses here would be inventing a cause; the
    /// point of this block is that the *evidence* travels with the failure so a
    /// human (or a `/debug trace` capture) can attribute it.
    ///
    /// Deliberately **brace-free**: this text is appended to a provider error
    /// that the client then normalizes, and the notice normalizer treats the
    /// first `{` as the start of a JSON error body (it pretty-prints it into the
    /// entry's detail). Emitting the raw `thinkingConfig` object here would make
    /// the normalizer re-parse our own diagnostic as if it were the upstream's
    /// payload and mangle both the title and the detail. So the fields are
    /// flattened to `key=value` pairs instead.
    pub fn describe(&self) -> String {
        let mut facts = vec![format!("model={}", self.wire_model)];
        match self.thinking_config.as_deref() {
            Some(config) => facts.push(format!("thinkingConfig=({})", flatten_json(config))),
            None => facts.push("thinkingConfig=omitted".to_string()),
        }
        facts.push(format!("tools={}", self.tool_count));
        facts.push(format!("contents={}", self.content_count));
        if self.replayed_signatures > 0 {
            let real = self.replayed_signatures - self.bypass_signatures;
            facts.push(format!(
                "replayedThoughtSignatures={real} (plus {} bypass sentinels)",
                self.bypass_signatures
            ));
        } else {
            facts.push("replayedThoughtSignatures=none".to_string());
        }
        format!(
            "The request that was refused carried: {}.\n\
             Google's message names no field, so read this list against the \
             upstream contract: a `thoughtSignature` replayed from a different \
             wire identity than the one this request addressed (a token only \
             validates on the backend that minted it — the `Corrupted thought \
             signature` case), a `thinkingConfig` the routed entity does not \
             accept (a per-level entity rejects the whole in-band object), a \
             `thinkingLevel` outside `LOW`/`MEDIUM`/`HIGH`, a model id that is \
             not the registered entity, or a tool schema outside Google's \
             OpenAPI subset. Arm `/debug trace` to capture the exact body.",
            facts.join(", ")
        )
    }
}

/// Flatten a compact JSON object into `key=value, key=value` without braces or
/// quotes, so the result can be embedded in prose the notice normalizer will
/// not mistake for a JSON payload.
fn flatten_json(config: &str) -> String {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str(config) else {
        // Not an object (never produced by the request builder, but be total):
        // strip the two characters that would trip the normalizer.
        return config.replace(['{', '}'], "").replace('"', "");
    };
    map.iter()
        .map(|(key, value)| match value {
            serde_json::Value::String(s) => format!("{key}={s}"),
            other => format!("{key}={other}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Augment a transport-layer error with model-specific guidance, folding in
/// the shape of the request that was refused. For native Google, a `404
/// NOT_FOUND` almost always means the upstream does not serve this model id —
/// not a transient fault. `429 RESOURCE_EXHAUSTED` explains quota / rate
/// limits.
///
/// The input may be a `[NUO_RETRYABLE]`-enveloped error (429/5xx from
/// [`ensure_success`](nuo_provider::transport::ensure_success)); appending to the
/// envelope verbatim would corrupt its JSON and strip the error of its
/// retryable classification downstream, so the guidance is folded **into**
/// the envelope's message and any `RetryInfo` delay Google embedded in the
/// body is promoted to `retry_after_ms` when the envelope has none.
///
/// `shape` is `Some` whenever the caller knows what it put on the wire. It is
/// only folded in for a request refusal
/// ([`nuo_provider::error::ProviderError::is_request_refusal`]) — the one class whose upstream body
/// is routinely content-free.
///
/// # Classify before you call this
///
/// The guidance appended here deliberately mentions `thinkingConfig` /
/// `thinkingLevel` / `HTTP 400` so a content-free refusal becomes actionable.
/// That means the **returned** message matches
/// [`rejects_thinking_config`]. Never decide a downgrade from it — use
/// [`rejects_thinking_config_error`] on the untouched error first.
pub fn clarify_error_with_shape(
    err: nuo_provider::ProviderError,
    model: &str,
    base_url: &str,
    shape: Option<&RequestShape>,
) -> nuo_provider::ProviderError {
    let server_delay = google_retry_after_ms(err.message());
    // A request refusal is the one class whose upstream body is routinely
    // content-free, so the sent shape is folded in as the error's detail. The
    // predicate is the shared typed one (`ProviderError::is_request_refusal`),
    // read *before* the message is rewritten, so this cannot disagree with the
    // classification that produced the error. It also covers in-band refusals
    // (a vendor reporting the refusal inside a 2xx body), which carry no status.
    let shape = shape.filter(|_| err.is_request_refusal());
    let mut err = err.map_message(|msg| {
        let msg = clarify_message(msg, model, base_url);
        match shape {
            Some(shape) => format!("{msg}\n\n{}", shape.describe()),
            None => msg,
        }
    });
    if server_delay.is_some() {
        err = err.with_retry_after_if_absent(server_delay);
    }
    err
}

/// The plain-message half of [`clarify_error_with_shape`]: append guidance only, never
/// touching any envelope structure.
fn clarify_message(err: String, model: &str, base_url: &str) -> String {
    if err.contains("HTTP 404") || err.contains("\"status\": \"NOT_FOUND\"") {
        format!(
            "{err}\n\n\
             Google returned 404 for model `{model}` at {base_url}. \
             The requested endpoint or model was not found. Check the provider's \
             endpoint and protocol dialect, then its model availability; HTTP 404 \
             alone does not establish that the model is unsupported."
        )
    } else if err.contains("HTTP 429") || err.contains("RESOURCE_EXHAUSTED") {
        let quota_reset = google_quota_reset_hint(&err);
        format!(
            "{err}\n\n\
             Google rate limit / quota exhausted (RESOURCE_EXHAUSTED).{quota_reset}"
        )
    } else {
        err
    }
}

/// Extract Google's own reset hint when the error body carries one. Google
/// attaches `details[]: [{...QuotaFailure}, {@type: RetryInfo, retryDelay:
/// "45s"}]` to a 429; a `RetryInfo` delay is authoritative, so it is quoted
/// verbatim. Legacy prose (the old fixed 45–60 minute guess) is not invented
/// when Google said nothing — an invented number reads as a promise.
fn google_quota_reset_hint(err: &str) -> String {
    if let Some(milliseconds) = google_retry_after_ms(err) {
        let seconds = milliseconds / 1000;
        let human = if seconds >= 3600 {
            format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
        } else if seconds >= 60 {
            format!("{}m {:02}s", seconds / 60, seconds % 60)
        } else {
            format!("{seconds}s")
        };
        return format!("\nGoogle reports the quota resets in ~{human} (`RetryInfo.retryDelay`).");
    }
    "\nYour Google One / Antigravity request quota is exhausted; it resets at the \
     start of the next window (5-hour or daily/weekly, whichever limit tripped)."
        .to_string()
}

/// Parse `RetryInfo`'s `retryDelay` (or a plain `retryDelay`) out of a Google
/// error body. Accepts `"45s"`, `"1.5s"`, `"2m30s"`-style durations plus a
/// bare second count; returns milliseconds.
fn google_retry_after_ms(err: &str) -> Option<u64> {
    let value = serde_json::from_str::<serde_json::Value>(
        err.split_once(": ").map(|(_, rest)| rest).unwrap_or(err),
    )
    .ok()
    .or_else(|| {
        // The body may be prefixed with provider framing; find the first
        // `{` from the first `HTTP 429`/`RESOURCE_EXHAUSTED` mention onward.
        err.find('{').and_then(|start| {
            let end = err.rfind('}')?;
            err.get(start..=end)
                .map(str::to_string)
                .and_then(|slice| serde_json::from_str::<serde_json::Value>(&slice).ok())
        })
    })?;
    let retry_delay = find_retry_delay(&value)?;
    parse_google_duration(&retry_delay)
}

/// Depth-first search for the first `retryDelay` string anywhere in the
/// payload (Google nests it under `details[]` of the error).
fn find_retry_delay(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(delay)) = map.get("retryDelay") {
                return Some(delay.clone());
            }
            for child in map.values() {
                if let Some(found) = find_retry_delay(child) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(items) => {
            for child in items {
                if let Some(found) = find_retry_delay(child) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Parse a Google protobuf duration string (`"45s"`, `"1.500s"`, `"90m"`) or
/// a bare number of seconds into milliseconds.
fn parse_google_duration(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if let Ok(seconds) = trimmed.parse::<f64>() {
        return Some((seconds.max(0.0) * 1000.0) as u64);
    }
    let mut total_seconds = 0.0f64;
    let mut matched = false;
    let mut rest = trimmed;
    while let Some(pos) = rest.find(|c: char| c.is_ascii_digit() || c == '.') {
        let digits: String = rest[pos..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let unit_start = pos + digits.len();
        let unit: String = rest[unit_start..]
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        if digits.is_empty() || unit.is_empty() {
            break;
        }
        let value: f64 = digits.parse().ok()?;
        let multiplier = match unit.as_str() {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            "ms" => 0.001,
            _ => return None,
        };
        total_seconds += value * multiplier;
        matched = true;
        rest = &rest[unit_start + unit.len()..];
    }
    matched.then(|| (total_seconds.max(0.0) * 1000.0) as u64)
}

#[cfg(test)]
mod tests;
