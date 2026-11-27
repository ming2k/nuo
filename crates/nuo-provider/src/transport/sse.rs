//! Server-Sent Events (SSE) byte-stream decoder shared by streaming consumers.
//!
//! Streaming endpoints deliver the response as an opaque run of byte chunks
//! whose boundaries are dictated by TLS/TCP framing — *not* by SSE frames or
//! UTF-8 character boundaries.
//!
//! The decoder here accumulates raw bytes and only performs UTF-8 decoding at
//! `\n` line boundaries, so a multi-byte sequence split across chunks is
//! reassembled before any caller observes it.

use futures::StreamExt;
use futures::stream::BoxStream;

use super::client::HttpResponse;
use super::error::TransportError;

/// Decode a streaming SSE response into a flat stream of `data:` payload
/// strings (the `data:` prefix and surrounding whitespace stripped; the
/// `[DONE]` sentinel filtered out).
pub fn data_payloads(
    response: HttpResponse,
    label: &'static str,
) -> BoxStream<'static, Result<String, TransportError>> {
    payloads_from_chunks(
        response.into_byte_stream(),
        label,
        |error: TransportError| error,
    )
}

/// Core decoder over an arbitrary byte-chunk stream.
pub fn payloads_from_chunks<C, S, E, F>(
    chunks: S,
    label: &'static str,
    map_error: F,
) -> BoxStream<'static, Result<String, TransportError>>
where
    S: futures::Stream<Item = Result<C, E>> + Send + 'static,
    C: AsRef<[u8]> + 'static,
    E: 'static,
    F: Fn(E) -> TransportError + Send + 'static,
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
        let mut payloads: Vec<Result<String, TransportError>> = Vec::new();
        for line in drain_complete_lines(&mut buffer, label) {
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

    let tail = futures::stream::once(async move {
        let buffer = tail_buffer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if buffer.is_empty() {
            Vec::new()
        } else {
            vec![Err(TransportError::Stream {
                label,
                message: format!(
                    "{label} stream ended with an incomplete SSE event \
                     ({} trailing byte(s)); the response was likely truncated.",
                    buffer.len()
                ),
                retryable: true,
                diagnostic: None,
            })]
        }
    });
    decoded.chain(tail).flat_map(futures::stream::iter).boxed()
}

/// Extract the `data:` payload from a single (already complete) SSE line.
fn data_payload_from_line(line: &str) -> Option<&str> {
    line.strip_prefix("data:")
        .map(str::trim_start)
        .filter(|data| *data != "[DONE]")
}

fn drain_complete_lines(
    buffer: &mut Vec<u8>,
    label: &'static str,
) -> Vec<Result<String, TransportError>> {
    let mut lines = Vec::new();
    while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
        let line = match std::str::from_utf8(&buffer[..pos]) {
            Ok(line) => Ok(line.trim().to_string()),
            Err(error) => Err(TransportError::Decode {
                label,
                message: format!("{label} stream carried invalid UTF-8: {error}"),
            }),
        };
        buffer.drain(..pos + 1);
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_payloads(chunks: &[&[u8]]) -> Vec<Result<String, TransportError>> {
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
            chunks.iter().map(|chunk| Ok(chunk.to_vec())).collect();
        futures::executor::block_on(
            payloads_from_chunks(futures::stream::iter(chunks), "Test", |error| {
                TransportError::Transport {
                    label: "Test",
                    message: error.to_string(),
                    retryable: true,
                    diagnostic: None,
                }
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
    }

    #[test]
    fn reassembles_split_multibyte_utf8() {
        let heart = "❤️"; // UTF-8: [226, 157, 164, 239, 184, 143]
        let heart_bytes = heart.as_bytes();
        let split_1 = [b"data: ".as_slice(), &heart_bytes[..2]].concat();
        let split_2 = [&heart_bytes[2..], b"\n\n"].concat();

        let results = collect_payloads(&[&split_1, &split_2]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].as_ref().unwrap(), heart);
    }

    #[test]
    fn handles_multiple_events_in_one_chunk() {
        let chunk = b"data: one\n\ndata: two\n\n";
        let results = collect_payloads(&[chunk]);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].as_ref().unwrap(), "one");
        assert_eq!(results[1].as_ref().unwrap(), "two");
    }

    #[test]
    fn flags_truncated_trailing_bytes_as_retryable_error() {
        let results = collect_payloads(&[b"data: complete\n", b"data: incomp"]);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].as_ref().unwrap(), "complete");
        let error = results[1].as_ref().unwrap_err();
        assert!(error.is_retryable());
    }

    #[test]
    fn fuzz_sse_framer_handles_arbitrary_splits_and_corrupt_utf8_without_panicking() {
        // Every 1-byte split of a realistic multi-event UTF-8 SSE stream must
        // yield the exact same payloads as feeding it in one chunk.
        let stream =
            b": ping\r\nevent: delta\r\ndata: {\"t\":\"\xe4\xbd\xa0\xe5\xa5\xbd\"}\r\n\r\ndata: [DONE]\n";
        let whole = collect_payloads(&[stream]);
        let byte_by_byte: Vec<&[u8]> = stream.chunks(1).collect();
        let split = collect_payloads(&byte_by_byte);
        assert_eq!(whole.len(), 1);
        assert_eq!(split.len(), 1);
        assert_eq!(whole[0].as_ref().unwrap(), split[0].as_ref().unwrap());

        // Seeded random byte streams (including invalid UTF-8) must never panic.
        let mut state: u64 = 0xf00d_baad_1234_5678;
        let mut next_u8 = || -> u8 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 33) as u8
        };
        for case in 0..256 {
            let chunk_count = (case % 6) + 1;
            let mut owned: Vec<Vec<u8>> = Vec::with_capacity(chunk_count);
            for _ in 0..chunk_count {
                let len = (next_u8() as usize % 32) + 1;
                let mut bytes: Vec<u8> = (0..len).map(|_| next_u8()).collect();
                if case % 3 == 0 && bytes.len() >= 6 {
                    bytes[0..6].copy_from_slice(b"data: ");
                }
                if case % 2 == 0 {
                    bytes.push(b'\n');
                }
                owned.push(bytes);
            }
            let refs: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
            let _ = collect_payloads(&refs);
        }
    }
}
