//! Server-Sent Events byte-stream decoder shared by every streaming provider.
//!
//! Streaming chat-completion endpoints deliver the response as an opaque run
//! of byte chunks whose boundaries are dictated by TLS/TCP framing — *not* by
//! SSE frames or UTF-8 character boundaries. A single CJK character occupies
//! 3 bytes in UTF-8; if a chunk boundary lands inside those bytes, decoding
//! each chunk on its own (e.g. with [`String::from_utf8_lossy`]) permanently
//! replaces the split bytes with `U+FFFD` (`�`) — the `���` artefact seen in
//! CJK output.
//!
//! The decoder here accumulates raw bytes and only performs UTF-8 decoding at
//! `\n` line boundaries, so a multi-byte sequence (or a partial SSE frame)
//! split across chunks is reassembled before any provider ever observes it.
//!
//! Two strictness rules keep a damaged stream from degrading output silently
//! (the IncompleteEvent / strict-UTF-8 discipline): decoding is *strict* —
//! `\n` never appears inside a multi-byte UTF-8 sequence, so a complete line
//! that fails to decode is genuine corruption, surfaced as an explicit stream
//! error rather than rewritten with `U+FFFD` — and a byte stream that ends
//! with a partial frame still buffered was cut off by the transport, so the
//! stream ends with an explicit (retryable) error instead of dropping the
//! incomplete tail.

use futures::StreamExt;
use futures::stream::BoxStream;

use nuo_contracts::{ProviderError, ProviderErrorKind};

/// Decode a streaming SSE response into a flat stream of `data:` payload
/// strings (the `data:` prefix and surrounding whitespace stripped; the
/// `[DONE]` sentinel filtered out).
///
/// Byte reassembly happens internally, so callers never observe a character or
/// frame split across network chunks. Each yielded item is one SSE `data:`
/// event's payload — finer-grained and more responsive than batching by
/// network chunk, and the standard shape expected of an SSE reader.
pub fn data_payloads(
    response: crate::egress::HttpResponse,
    provider: &'static str,
) -> BoxStream<'static, Result<String, ProviderError>> {
    // Both transports hand back provider errors already; the identity mapper
    // keeps the signature honest for the owned path.
    payloads_from_chunks(
        response.into_byte_stream(),
        provider,
        |error: ProviderError| error,
    )
}

/// Core decoder over an arbitrary byte-chunk stream.
///
/// The transport is a parameter, not an assumption: this is the seam that lets
/// any byte source — `reqwest` today, the owned `muta-net` transport tomorrow —
/// feed the *same* SSE reassembly, so the two can be compared byte for byte
/// (ADR-0200's shadow criterion). `map_error` turns the source's error type into
/// the provider error the retry classifier understands.
pub fn payloads_from_chunks<C, S, E, F>(
    chunks: S,
    provider: &'static str,
    map_error: F,
) -> BoxStream<'static, Result<String, ProviderError>>
where
    S: futures::Stream<Item = Result<C, E>> + Send + 'static,
    C: AsRef<[u8]> + 'static,
    E: 'static,
    F: Fn(E) -> ProviderError + Send + 'static,
{
    let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let tail_buffer = std::sync::Arc::clone(&buffer);
    let decoded = chunks.map(move |item| {
        let chunk = match item {
            Ok(chunk) => chunk,
            Err(error) => return vec![Err(map_error(error))],
        };
        let mut buffer = buffer.lock().unwrap_or_else(|error| error.into_inner());
        buffer.extend_from_slice(chunk.as_ref());
        let mut payloads: Vec<Result<String, ProviderError>> = Vec::new();
        for line in drain_complete_lines(&mut buffer, provider) {
            match line {
                Ok(line) => {
                    if let Some(data) = data_payload_from_line(&line) {
                        payloads.push(Ok(data.to_string()));
                    }
                }
                Err(error) => payloads.push(Err(error)),
            }
        }
        payloads
    });
    // EOF with bytes still buffered means the transport ended the stream
    // mid-event (connection drop, proxy timeout). The leftover is not a
    // complete event and must not be delivered as one; surface it as an
    // explicit retryable error rather than silently ending the stream.
    let tail = futures::stream::once(async move {
        let buffer = tail_buffer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if buffer.is_empty() {
            Vec::new()
        } else {
            vec![Err(ProviderError::new(
                provider,
                ProviderErrorKind::Transport,
                format!(
                    "{provider} stream ended with an incomplete SSE event \
                     ({} trailing byte(s)); the response was likely truncated.",
                    buffer.len()
                ),
            )
            .retryable(None))]
        }
    });
    decoded.chain(tail).flat_map(futures::stream::iter).boxed()
}

/// Extract the `data:` payload from a single (already complete) SSE line.
///
/// Returns `None` for non-data lines (event/id/retry/comments) and for the
/// `[DONE]` sentinel. Accepts both `data:` and `data: ` prefixes.
fn data_payload_from_line(line: &str) -> Option<&str> {
    line.strip_prefix("data:")
        .map(str::trim_start)
        .filter(|data| *data != "[DONE]")
}

/// Drain complete (`\n`-terminated) lines from a raw byte buffer, decoding
/// each as UTF-8. Trailing bytes after the final newline are retained so a
/// partial multi-byte sequence or SSE frame is completed on the next read.
///
/// Decoding is strict: a complete line that is not valid UTF-8 is stream
/// corruption (not a chunk-boundary artefact — reassembly handles those), so
/// it yields an `Err` item instead of a lossy `U+FFFD` rewrite. The corrupt
/// line's bytes are still drained so one bad frame does not wedge the rest
/// of the stream.
fn drain_complete_lines(
    buffer: &mut Vec<u8>,
    provider: &'static str,
) -> Vec<Result<String, ProviderError>> {
    let mut lines = Vec::new();
    while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
        let line = match std::str::from_utf8(&buffer[..pos]) {
            Ok(line) => Ok(line.trim().to_string()),
            Err(error) => Err(ProviderError::new(
                provider,
                ProviderErrorKind::Decode,
                format!("{provider} stream carried invalid UTF-8: {error}"),
            )
            .retryable(None)),
        };
        buffer.drain(..pos + 1);
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the chunk decoder over scripted network chunks, collecting every
    /// item the same way a provider stream consumer would.
    fn collect_payloads(chunks: &[&[u8]]) -> Vec<Result<String, ProviderError>> {
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
            chunks.iter().map(|chunk| Ok(chunk.to_vec())).collect();
        futures::executor::block_on(
            payloads_from_chunks(futures::stream::iter(chunks), "Test", |error| {
                ProviderError::new("Test", ProviderErrorKind::Transport, error.to_string())
            })
            .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn extracts_data_payload_and_strips_prefix() {
        assert_eq!(data_payload_from_line("data: hello"), Some("hello"));
        assert_eq!(data_payload_from_line("data:hello"), Some("hello"));
        assert_eq!(data_payload_from_line("data:  spaced"), Some("spaced"));
    }

    #[test]
    fn ignores_non_data_and_done_sentinel() {
        assert_eq!(data_payload_from_line(": keep-alive comment"), None);
        assert_eq!(data_payload_from_line("event: ping"), None);
        assert_eq!(data_payload_from_line("id: 42"), None);
        assert_eq!(data_payload_from_line("data: [DONE]"), None);
        assert_eq!(data_payload_from_line(""), None);
    }

    #[test]
    fn drain_reassembles_split_utf8_across_chunks() {
        // "😀😁" is two wide chars (8 bytes). Split the second char (4 bytes)
        // across two network chunks the way a TLS read would: the first chunk
        // ends with an incomplete leading byte sequence, the second completes
        // it. Decoding per-chunk would yield U+FFFD (`�`); buffering bytes and
        // decoding at the `\n` boundary must preserve the original text.
        let frame = "data: {\"text\":\"😀😁\"}\n".as_bytes().to_vec();
        let split = frame.len() - 5; // split inside the second 4-byte emoji
        let mut buffer: Vec<u8> = Vec::new();

        buffer.extend_from_slice(&frame[..split]);
        assert!(
            drain_complete_lines(&mut buffer, "Test").is_empty(),
            "no newline yet -> nothing decoded, partial bytes retained"
        );

        buffer.extend_from_slice(&frame[split..]);
        let lines = drain_complete_lines(&mut buffer, "Test");
        assert_eq!(lines, vec![Ok("data: {\"text\":\"😀😁\"}".to_string())]);
        assert!(buffer.is_empty(), "buffer must be fully drained");
    }

    #[test]
    fn drain_handles_crlf_and_retains_partial_tail() {
        let mut buffer = b"data: one\r\ndata: two\npartial".to_vec();
        let lines = drain_complete_lines(&mut buffer, "Test");
        assert_eq!(
            lines,
            vec![Ok("data: one".to_string()), Ok("data: two".to_string())]
        );
        assert_eq!(buffer, b"partial");
    }

    #[test]
    fn drain_rejects_invalid_utf8_line_as_retryable_error() {
        // A *complete* line that is not valid UTF-8 cannot be a chunk-boundary
        // artefact (reassembly covers those); it is corruption and must
        // surface explicitly instead of being rewritten with U+FFFD.
        let mut buffer = b"data: ok\n\xFF\xFE bad\ndata: after\n".to_vec();
        let lines = drain_complete_lines(&mut buffer, "Test");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], Ok("data: ok".to_string()));
        let error = lines[1].as_ref().unwrap_err();
        assert!(error.message().contains("invalid UTF-8"), "{error}");
        assert!(matches!(
            error.retry_disposition(),
            nuo_contracts::RetryDisposition::Retry { .. }
        ));
        assert_eq!(lines[2], Ok("data: after".to_string()));
        assert!(buffer.is_empty(), "corrupt line must not wedge the stream");
    }

    #[test]
    fn stream_ends_cleanly_after_terminated_frames() {
        let items = collect_payloads(&[b"data: one\n\n", b"data: [DONE]\n\n"]);
        assert_eq!(items, vec![Ok("one".to_string())]);
    }

    #[test]
    fn stream_reassembles_split_frame_without_tail_error() {
        let items = collect_payloads(&[b"data: {\"a\":", b"1}\n\n"]);
        assert_eq!(items, vec![Ok("{\"a\":1}".to_string())]);
    }

    #[test]
    fn stream_rejects_incomplete_tail_as_retryable_error() {
        // EOF while a partial frame is still buffered: the transport cut the
        // stream mid-event. The leftover must surface as an explicit error
        // rather than vanish (previously the stream just ended).
        let items = collect_payloads(&[b"data: one\n\n", b"data: {\"trunc"]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], Ok("one".to_string()));
        let error = items[1].as_ref().unwrap_err();
        assert!(error.message().contains("incomplete SSE event"), "{error}");
        assert!(matches!(
            error.retry_disposition(),
            nuo_contracts::RetryDisposition::Retry { .. }
        ));
    }
}
