//! Anthropic Messages — thinking-signature capture.
//!
//! A small side-channel accumulator: streaming with `display:"omitted"` delivers
//! the encrypted thinking credential as one or more `signature_delta` events
//! (not as a semantic event). [`SignatureStash`] concatenates them in arrival
//! order so the assembled assistant turn can carry the full signature in
//! `provider_meta` for the next replay. Pure accumulation, no I/O.

use std::sync::Mutex;

/// Accumulates thinking-block `signature_delta` fragments across a stream.
#[derive(Default)]
pub struct SignatureStash {
    signature: Mutex<Option<String>>,
}

impl SignatureStash {
    pub fn new() -> Self {
        Self::default()
    }

    /// A cloneable handle to the underlying `Mutex`, for sharing between the
    /// provider and a `'static` stream closure.
    pub fn shared() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::new())
    }

    /// Inspect one already-parsed SSE event for a `signature_delta` and, if
    /// found, append its fragment to the accumulated signature. No-op for any
    /// other event. The event is parsed by the caller (each stream payload is
    /// parsed exactly once); this never re-parses raw strings.
    pub fn on_event(&self, event: &serde_json::Value) {
        if event["type"].as_str() != Some("content_block_delta") {
            return;
        }
        if event["delta"]["type"].as_str() != Some("signature_delta") {
            return;
        }
        if let Some(frag) = event["delta"]["signature"].as_str() {
            let mut guard = self.signature.lock().unwrap_or_else(|e| e.into_inner());
            guard.get_or_insert_with(String::new).push_str(frag);
        }
    }

    /// Drain and return the accumulated signature, if any. The caller (the
    /// streaming/non-streaming chat paths) reads this to stamp the returned
    /// message's `provider_meta` before the next request would clobber it.
    pub fn take(&self) -> Option<String> {
        self.signature
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Set the signature directly (the non-streaming path reads it once off a
    /// `thinking` block and sets it here).
    pub fn set(&self, signature: String) {
        *self.signature.lock().unwrap_or_else(|e| e.into_inner()) = Some(signature);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_accumulate_into_stash() {
        // Streaming with display:"omitted" delivers the signature as one or
        // more `signature_delta` events; capture must concatenate them in
        // arrival order. The stash consumes already-parsed events — the same
        // `Value` the stream parser sees, never a re-parsed string.
        let stash = SignatureStash::shared();
        let parse =
            |data: &str| serde_json::from_str::<serde_json::Value>(data).expect("test event");
        stash.on_event(&parse(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EosnCk"}}"#,
        ));
        stash.on_event(&parse(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"XyZ"}}"#,
        ));
        // A non-signature event must not disturb the stash.
        stash.on_event(&parse(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        ));
        assert_eq!(stash.take().as_deref(), Some("EosnCkXyZ"));
    }

    #[test]
    fn non_signature_event_kinds_are_ignored() {
        // The delta type gates the stash: a signature inside any other event
        // shape (wrong `type`, wrong `delta.type`) must not be captured.
        let stash = SignatureStash::shared();
        let parse =
            |data: &str| serde_json::from_str::<serde_json::Value>(data).expect("test event");
        stash.on_event(&parse(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        ));
        stash.on_event(&parse(
            r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hm"}}"#,
        ));
        stash.on_event(&parse(
            r#"{"type":"content_block_start","content_block":{"type":"text"}}"#,
        ));
        assert_eq!(stash.take(), None);
    }
}
