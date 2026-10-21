//! Server-Sent Events byte-stream decoder shared by every streaming consumer.
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

use crate::error::EgressError;

/// Decode a streaming SSE response into a flat stream of `data:` payload
/// strings (the `data:` prefix and surrounding whitespace stripped; the
/// `[DONE]` sentinel filtered out).
pub fn data_payloads(
    response: crate::egress::HttpResponse,
    label: &'static str,
) -> BoxStream<'static, Result<String, EgressError>> {
    payloads_from_chunks(response.into_byte_stream(), label, |error: EgressError| {
        error
    })
}

/// Core decoder over an arbitrary byte-chunk stream.
pub fn payloads_from_chunks<C, S, E, F>(
    chunks: S,
    label: &'static str,
    map_error: F,
) -> BoxStream<'static, Result<String, EgressError>>
where
    S: futures::Stream<Item = Result<C, E>> + Send + 'static,
    C: AsRef<[u8]> + 'static,
    E: 'static,
    F: Fn(E) -> EgressError + Send + 'static,
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
        let mut payloads: Vec<Result<String, EgressError>> = Vec::new();
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
            vec![Err(EgressError::Stream {
                label,
                message: format!(
                    "{label} stream ended with an incomplete SSE event \
                     ({} trailing byte(s)); the response was likely truncated.",
                    buffer.len()
                ),
                retryable: true,
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
) -> Vec<Result<String, EgressError>> {
    let mut lines = Vec::new();
    while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
        let line = match std::str::from_utf8(&buffer[..pos]) {
            Ok(line) => Ok(line.trim().to_string()),
            Err(error) => Err(EgressError::Decode {
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

    fn collect_payloads(chunks: &[&[u8]]) -> Vec<Result<String, EgressError>> {
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
            chunks.iter().map(|chunk| Ok(chunk.to_vec())).collect();
        futures::executor::block_on(
            payloads_from_chunks(futures::stream::iter(chunks), "Test", |error| {
                EgressError::Transport {
                    label: "Test",
                    message: error.to_string(),
                    retryable: true,
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
        assert_eq!(data_payload_from_line(""), None);
    }

    #[test]
    fn drain_reassembles_split_utf8_across_chunks() {
        let frame = "data: {\"text\":\"😀😁\"}\n".as_bytes().to_vec();
        let split = frame.len() - 5;
        let mut buffer: Vec<u8> = Vec::new();

        buffer.extend_from_slice(&frame[..split]);
        assert!(drain_complete_lines(&mut buffer, "Test").is_empty());

        buffer.extend_from_slice(&frame[split..]);
        let lines = drain_complete_lines(&mut buffer, "Test");
        assert_eq!(lines, vec![Ok("data: {\"text\":\"😀😁\"}".to_string())]);
        assert!(buffer.is_empty());
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
        let mut buffer = b"data: ok\n\xFF\xFE bad\ndata: after\n".to_vec();
        let lines = drain_complete_lines(&mut buffer, "Test");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], Ok("data: ok".to_string()));
        let error = lines[1].as_ref().unwrap_err();
        assert!(matches!(error, EgressError::Decode { .. }));
        assert_eq!(lines[2], Ok("data: after".to_string()));
        assert!(buffer.is_empty());
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
        let items = collect_payloads(&[b"data: one\n\n", b"data: {\"trunc"]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], Ok("one".to_string()));
        let error = items[1].as_ref().unwrap_err();
        assert!(matches!(
            error,
            EgressError::Stream {
                retryable: true,
                ..
            }
        ));
    }
}
