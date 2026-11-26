use super::document::{MAX_DOCUMENT_BYTES, is_pdf, run_document_extraction, window_page_span};
use super::line_index::LineCheckpoint;
use super::text::{
    MAX_LINE_CHARS, MAX_READ_BYTES, TAIL_REVERSE_SEEK_THRESHOLD, read_bounded_line,
    stream_lines_with_budget, tail_from_eof,
};
use super::*;
use ::tool::{DocumentError, ExtractedDocument, ToolContext};
use base64::Engine as _;
use serde_json::json;
use tempfile::tempdir;

fn ctx(root: &std::path::Path) -> Arc<SystemToolContext> {
    Arc::new(SystemToolContext::new(root))
}

struct FakeExtractor {
    result: Result<ExtractedDocument, DocumentError>,
    seen_range: std::sync::Mutex<Option<Option<PageRange>>>,
}

impl DocumentExtractor for FakeExtractor {
    fn extract(
        &self,
        _mime: &str,
        _data: &[u8],
        page_range: Option<PageRange>,
    ) -> Result<ExtractedDocument, DocumentError> {
        *self.seen_range.lock().unwrap() = Some(page_range);
        self.result.clone()
    }
}

fn ctx_with_extractor(
    root: &std::path::Path,
    result: Result<ExtractedDocument, DocumentError>,
) -> Arc<SystemToolContext> {
    Arc::new(
        SystemToolContext::new(root).with_document_extractor(Arc::new(FakeExtractor {
            result,
            seen_range: std::sync::Mutex::new(None),
        })),
    )
}

fn extracted(text: &str, pages: usize, total_pages: usize, truncated: bool) -> ExtractedDocument {
    ExtractedDocument {
        text: text.to_string(),
        pages,
        total_pages,
        truncated,
        next_page: (truncated && pages < total_pages).then_some(pages + 1),
    }
}

#[test]
fn pdf_detection_distinguishes_prose_from_a_header_preamble() {
    assert!(is_pdf(b"%PDF-1.7\n"));
    assert!(is_pdf(b"   %PDF-1.7\n"));
    assert!(is_pdf(b"\xEF\xBB\xBF%PDF-1.7\n"));
    assert!(is_pdf(b"\xFF\xFEjunk\n%PDF-1.7\n"));
    assert!(!is_pdf(b"this document explains %PDF- headers\n"));
    assert!(!is_pdf(b"no header here"));
}

#[tokio::test]
async fn directory_is_rejected_and_points_to_find_files() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();

    let err = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": "sub"}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("is a directory"), "{msg}");
    assert!(msg.contains("find_files"), "{msg}");
}

#[tokio::test]
async fn text_page_reports_its_window_and_continuation() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("src.rs"), "a\nb\nc\nd\n").unwrap();

    let out = ReadTool::new(ctx(dir.path()))
        .execute(
            &ToolContext::default(),
            json!({"path": "src.rs", "offset": 2, "limit": 2}),
        )
        .await
        .unwrap();

    match out {
        ToolOutput::Code {
            text,
            start_line,
            prefix,
            suffix,
            lang,
            ..
        } => {
            assert_eq!(text, "b\nc");
            assert_eq!(start_line, 2);
            assert_eq!(lang.as_deref(), Some("rs"));
            assert!(
                prefix.as_deref().is_some_and(|prefix| prefix
                    .starts_with("[Lines 2-3 of 4 from `src.rs`]")
                    && prefix.contains("version")),
                "{prefix:?}"
            );
            assert_eq!(
                suffix.as_deref(),
                Some("[1 more line — read with offset=4]")
            );
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn text_page_is_capped_by_lines_and_reports_the_next_offset() {
    let dir = tempdir().unwrap();
    let content = (0..(MAX_READ_LINES + 5))
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("big.txt"), content).unwrap();

    let out = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": "big.txt"}))
        .await
        .unwrap();

    match out {
        ToolOutput::Code { text, suffix, .. } => {
            assert_eq!(text.lines().count(), MAX_READ_LINES);
            assert!(suffix.unwrap().contains("read with offset=2001"));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn negative_offset_reads_the_tail() {
    let dir = tempdir().unwrap();
    let content = (0..10)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("log.txt"), content).unwrap();

    let out = ReadTool::new(ctx(dir.path()))
        .execute(
            &ToolContext::default(),
            json!({"path": "log.txt", "offset": -3}),
        )
        .await
        .unwrap();

    match out {
        ToolOutput::Code {
            text, start_line, ..
        } => {
            assert_eq!(text, "line 7\nline 8\nline 9");
            assert_eq!(start_line, 8);
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn binary_is_reported_not_decoded() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("bin"), b"a\0b").unwrap();

    let err = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": "bin"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("binary file"));
}

#[tokio::test]
async fn pdf_is_extracted_into_a_paginated_text_result() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("doc.pdf"), b"%PDF-1.7\n").unwrap();
    let ctx = ctx_with_extractor(
        dir.path(),
        Ok(extracted(
            "[page 1]\nalpha\nbeta\n[page 2]\ngamma\ndelta",
            2,
            3,
            true,
        )),
    );

    let out = ReadTool::new(ctx)
        .execute(
            &ToolContext::default(),
            json!({"path": "doc.pdf", "offset": 1, "limit": 2}),
        )
        .await
        .unwrap();

    match out {
        ToolOutput::Code {
            text,
            start_line,
            prefix,
            suffix,
            lang,
            ..
        } => {
            assert_eq!(text, "[page 1]\nalpha");
            assert_eq!(start_line, 1);
            assert_eq!(lang, None);
            let prefix = prefix.unwrap();
            assert!(prefix.contains("Lines 1-2 of 6"));
            assert!(prefix.contains("PDF text, 2 of 3 pages, extraction truncated"));
            assert!(suffix.unwrap().contains("read with offset=3"));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn pdf_extractor_can_come_from_the_invocation_service_map() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("doc.pdf"), b"%PDF-1.7\n").unwrap();

    let mut services = tool::ServiceMapBuilder::new();
    services.provide(Arc::new(FakeExtractor {
        result: Ok(extracted("from the service map", 1, 1, false)),
        seen_range: std::sync::Mutex::new(None),
    }) as Arc<dyn DocumentExtractor>);
    let call_ctx = ToolContext::default().with_services(services.build());

    let out = ReadTool::new(ctx(dir.path()))
        .execute(&call_ctx, json!({"path": "doc.pdf"}))
        .await
        .unwrap();
    match out {
        ToolOutput::Code { text, .. } => assert_eq!(text, "from the service map"),
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn pdf_pages_parameter_forwards_range_and_surfaces_next_page_hint() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("doc.pdf"), b"%PDF-1.7\n").unwrap();

    let extractor = Arc::new(FakeExtractor {
        result: Ok(ExtractedDocument {
            text: "[page 2]\nsecond".to_string(),
            pages: 1,
            total_pages: 5,
            truncated: false,
            next_page: Some(3),
        }),
        seen_range: std::sync::Mutex::new(None),
    });
    let ctx = Arc::new(
        SystemToolContext::new(dir.path())
            .with_document_extractor(extractor.clone() as Arc<dyn DocumentExtractor>),
    );

    let out = ReadTool::new(ctx.clone())
        .execute(
            &ToolContext::default(),
            json!({"path": "doc.pdf", "pages": "2"}),
        )
        .await
        .unwrap();

    assert_eq!(
        *extractor.seen_range.lock().unwrap(),
        Some(Some(PageRange {
            start: 2,
            end: Some(2)
        }))
    );
    match out {
        ToolOutput::Code { suffix, .. } => {
            assert_eq!(
                suffix.as_deref(),
                Some("[more PDF pages remain — read with pages=\"3-\"]")
            );
        }
        other => panic!("expected Code, got {other:?}"),
    }

    let err = ReadTool::new(ctx)
        .execute(
            &ToolContext::default(),
            json!({"path": "doc.pdf", "pages": "5-2"}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("invalid `pages` range"));
}

#[tokio::test]
async fn document_extraction_timeout_is_a_named_failure() {
    struct SlowExtractor;
    impl DocumentExtractor for SlowExtractor {
        fn extract(
            &self,
            _mime: &str,
            _data: &[u8],
            _page_range: Option<PageRange>,
        ) -> Result<ExtractedDocument, DocumentError> {
            std::thread::sleep(std::time::Duration::from_millis(50));
            Ok(extracted("late", 1, 1, false))
        }
    }

    let err = run_document_extraction(
        Arc::new(SlowExtractor),
        b"irrelevant".to_vec(),
        None,
        std::time::Duration::from_millis(5),
        "read",
        "doc.pdf",
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("exceeded 5ms"),
        "expected the timeout to be named, got {err}"
    );
}

#[tokio::test]
async fn pdf_without_an_extractor_is_an_explicit_failure() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("doc.pdf"), b"%PDF-1.7\n").unwrap();

    let err = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": "doc.pdf"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no document text extractor"));
}

#[tokio::test]
async fn pdf_failures_are_named() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("doc.pdf"), b"%PDF-1.7\n").unwrap();

    let cases = [
        (DocumentError::Encrypted, "encrypted"),
        (
            DocumentError::NoText { pages: 3 },
            "no extractable text in 3 pages",
        ),
        (
            DocumentError::Malformed {
                detail: "bad xref".to_string(),
            },
            "could not be parsed as a PDF",
        ),
    ];

    for (error, needle) in cases {
        let err = ReadTool::new(ctx_with_extractor(dir.path(), Err(error)))
            .execute(&ToolContext::default(), json!({"path": "doc.pdf"}))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains(needle),
            "expected {needle:?} in {err}"
        );
    }
}

#[tokio::test]
async fn pdf_header_may_follow_leading_bytes() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("doc.pdf"),
        b"\xFF\xFEjunk before the header\n%PDF-1.7\n",
    )
    .unwrap();
    let ctx = ctx_with_extractor(dir.path(), Ok(extracted("hello", 1, 1, false)));

    let out = ReadTool::new(ctx)
        .execute(&ToolContext::default(), json!({"path": "doc.pdf"}))
        .await
        .unwrap();
    match out {
        ToolOutput::Code { text, prefix, .. } => {
            assert_eq!(text, "hello");
            assert!(prefix.unwrap().contains("PDF text, 1 page"));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn a_pdf_extension_does_not_make_text_a_pdf() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("notes.pdf"), "plain text\n").unwrap();

    let out = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": "notes.pdf"}))
        .await
        .unwrap();
    match out {
        ToolOutput::Code { text, lang, .. } => {
            assert_eq!(text, "plain text");
            assert_eq!(lang.as_deref(), Some("pdf"));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn prose_mentioning_the_pdf_marker_stays_text() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("notes.txt"),
        "The marker %PDF- belongs at the start of a PDF file.\n",
    )
    .unwrap();

    let out = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": "notes.txt"}))
        .await
        .unwrap();
    match out {
        ToolOutput::Code { text, .. } => {
            assert!(text.starts_with("The marker %PDF-"));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn pdf_source_ceiling_is_enforced_before_reading() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("huge.pdf");
    let mut file = std::fs::File::create(&path).unwrap();
    std::io::Write::write_all(&mut file, b"%PDF-1.7\n").unwrap();
    file.set_len(MAX_DOCUMENT_BYTES + 1).unwrap();

    let err = ReadTool::new(ctx_with_extractor(
        dir.path(),
        Ok(extracted("unused", 1, 1, false)),
    ))
    .execute(&ToolContext::default(), json!({"path": "huge.pdf"}))
    .await
    .unwrap_err();
    assert!(err.to_string().contains("PDF ingest ceiling"));
}

#[tokio::test]
async fn image_is_returned_as_a_typed_payload() {
    let dir = tempdir().unwrap();
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRfake";
    std::fs::write(dir.path().join("shot.png"), png).unwrap();

    let out = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": "shot.png"}))
        .await
        .unwrap();
    match out {
        ToolOutput::Image { mime, data } => {
            assert_eq!(mime, "image/png");
            let raw = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap();
            assert_eq!(raw, png);
        }
        other => panic!("expected Image, got {other:?}"),
    }
}

#[tokio::test]
async fn image_normalizer_is_applied_at_the_boundary() {
    struct Shrink;
    impl ImageNormalizer for Shrink {
        fn normalize(&self, _data: &[u8], _mime: &str) -> Option<tool::NormalizedImage> {
            Some(tool::NormalizedImage {
                data: b"small".to_vec(),
                mime: "image/jpeg".to_string(),
                width: 1,
                height: 1,
            })
        }
    }

    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("shot.png"),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRfake",
    )
    .unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()).with_image_normalizer(Arc::new(Shrink)));

    let out = ReadTool::new(ctx)
        .execute(&ToolContext::default(), json!({"path": "shot.png"}))
        .await
        .unwrap();
    match out {
        ToolOutput::Image { mime, data } => {
            assert_eq!(mime, "image/jpeg");
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap(),
                b"small"
            );
        }
        other => panic!("expected Image, got {other:?}"),
    }
}

#[tokio::test]
async fn offset_zero_is_rejected() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();

    let err = ReadTool::new(ctx(dir.path()))
        .execute(
            &ToolContext::default(),
            json!({"path": "a.txt", "offset": 0}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("1-based"), "{err}");
}

#[tokio::test]
async fn pages_argument_is_rejected_on_non_pdf_files() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    std::fs::write(
        dir.path().join("shot.png"),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRfake",
    )
    .unwrap();

    let err = ReadTool::new(ctx(dir.path()))
        .execute(
            &ToolContext::default(),
            json!({"path": "a.txt", "pages": "1-2"}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("only valid for PDF"), "{err}");

    let err = ReadTool::new(ctx(dir.path()))
        .execute(
            &ToolContext::default(),
            json!({"path": "shot.png", "pages": "1"}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("only valid for PDF"), "{err}");
}

#[tokio::test]
async fn negative_offset_has_no_hidden_100_line_cap_and_stays_byte_bounded() {
    let dir = tempdir().unwrap();
    let content = (0..300)
        .map(|index| format!("line {index:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("many.log"), content).unwrap();

    let out = ReadTool::new(ctx(dir.path()))
        .execute(
            &ToolContext::default(),
            json!({"path": "many.log", "offset": -250}),
        )
        .await
        .unwrap();
    match out {
        ToolOutput::Code {
            text, start_line, ..
        } => {
            assert_eq!(text.lines().count(), 250);
            assert_eq!(start_line, 51);
            assert!(text.starts_with("line 050\n"));
            assert!(text.ends_with("line 299"));
        }
        other => panic!("expected Code, got {other:?}"),
    }

    // Even a large negative offset must still stay within MAX_READ_BYTES.
    let wide = (0..1_000)
        .map(|index| format!("{index:04} {}", "x".repeat(200)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("wide.log"), wide).unwrap();
    let out = ReadTool::new(ctx(dir.path()))
        .execute(
            &ToolContext::default(),
            json!({"path": "wide.log", "offset": -1000}),
        )
        .await
        .unwrap();
    match out {
        ToolOutput::Code { text, .. } => {
            assert!(text.len() <= MAX_READ_BYTES);
            assert!(text.ends_with(&format!("0999 {}", "x".repeat(200))));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[test]
fn reverse_seek_tail_reads_only_the_end_of_a_large_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("huge.log");
    let body = (0..4_000)
        .map(|index| format!("row-{index:04}-{}", "a".repeat(60)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, format!("{body}\n")).unwrap();
    let len = std::fs::metadata(&path).unwrap().len();
    assert!(len > 128 * 1024);

    let mut file = std::fs::File::open(&path).unwrap();
    let page = tail_from_eof(
        &mut file,
        len,
        5,
        "huge.log",
        b"row-0000",
        &ToolContext::default(),
        128 * 1024,
    )
    .expect("tail_from_eof");

    assert_eq!(page.start_line, 0);
    assert_eq!(page.total, None);
    assert_eq!(page.tail_of_bytes, Some(len));
    let lines: Vec<&str> = page.text.lines().collect();
    assert_eq!(lines.len(), 5);
    assert!(lines[0].starts_with("row-3995-"));
    assert!(lines[4].starts_with("row-3999-"));

    let output = page.into_output_for(Some("log".to_string()), "huge.log", None, None, None);
    match output {
        ToolOutput::Code {
            prefix, start_line, ..
        } => {
            assert_eq!(start_line, 0);
            assert!(prefix.unwrap().contains("Last 5 lines of ~"));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn binary_diagnostics_identify_known_formats() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("db.sqlite"), b"SQLite format 3\0rest").unwrap();
    std::fs::write(dir.path().join("archive.zip"), b"PK\x03\x04\0rest").unwrap();
    std::fs::write(dir.path().join("prog"), b"\x7FELF\x02\x01\x01\0").unwrap();
    std::fs::write(dir.path().join("win.csv"), b"\xFF\xFEa\x00b\x00").unwrap();

    for (file, expected) in [
        ("db.sqlite", "SQLite database"),
        ("archive.zip", "ZIP archive"),
        ("prog", "ELF executable"),
        ("win.csv", "UTF-16LE text with BOM"),
    ] {
        let err = ReadTool::new(ctx(dir.path()))
            .execute(&ToolContext::default(), json!({"path": file}))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("binary file"), "{file}: {msg}");
        assert!(
            msg.contains(expected),
            "{file}: expected {expected:?} in {msg}"
        );
    }
}

#[tokio::test]
async fn pdf_pagination_hits_versioned_cache_and_maps_header_pages() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingExtractor {
        calls: AtomicUsize,
    }
    impl DocumentExtractor for CountingExtractor {
        fn extract(
            &self,
            _mime: &str,
            _data: &[u8],
            _page_range: Option<PageRange>,
        ) -> Result<ExtractedDocument, DocumentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(extracted(
                "[page 1]\nalpha\nbeta\n[page 2]\ngamma\ndelta",
                2,
                5,
                false,
            ))
        }
    }

    let dir = tempdir().unwrap();
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(&pdf_path, b"%PDF-1.7\nv1").unwrap();

    let extractor = Arc::new(CountingExtractor {
        calls: AtomicUsize::new(0),
    });
    let tool_ctx = Arc::new(
        SystemToolContext::new(dir.path())
            .with_document_extractor(extractor.clone() as Arc<dyn DocumentExtractor>),
    );
    let read = ReadTool::new(tool_ctx);

    // First read extracts and populates cache.
    let first = read
        .execute(
            &ToolContext::default(),
            json!({"path": "doc.pdf", "offset": 1, "limit": 2}),
        )
        .await
        .unwrap();
    assert_eq!(extractor.calls.load(Ordering::SeqCst), 1);
    match first {
        ToolOutput::Code { prefix, .. } => {
            assert!(prefix.unwrap().contains("showing page 1"));
        }
        other => panic!("expected Code, got {other:?}"),
    }

    // Second read (next page of lines, starting mid-page 1 into page 2) hits cache.
    let second = read
        .execute(
            &ToolContext::default(),
            json!({"path": "doc.pdf", "offset": 3, "limit": 3}),
        )
        .await
        .unwrap();
    assert_eq!(extractor.calls.load(Ordering::SeqCst), 1);
    match second {
        ToolOutput::Code { prefix, .. } => {
            assert!(prefix.unwrap().contains("showing pages 1-2"));
        }
        other => panic!("expected Code, got {other:?}"),
    }

    // Modifying the file changes its version and invalidates the cache.
    std::fs::write(&pdf_path, b"%PDF-1.7\nv2-updated").unwrap();
    let _third = read
        .execute(&ToolContext::default(), json!({"path": "doc.pdf"}))
        .await
        .unwrap();
    assert_eq!(extractor.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn window_page_span_tracks_mid_page_slices() {
    let text = "[page 1]\na\nb\n[page 2]\nc\nd\n[page 3]\ne";
    assert_eq!(window_page_span(text, 1, 2), Some((1, 1)));
    assert_eq!(window_page_span(text, 2, 2), Some((1, 1)));
    assert_eq!(window_page_span(text, 3, 2), Some((1, 2)));
    assert_eq!(window_page_span(text, 5, 3), Some((2, 3)));
    assert_eq!(window_page_span("no markers\nhere", 1, 2), None);
}

#[tokio::test]
async fn virtual_path_miss_does_not_read_relative_cwd_files() {
    let dir = tempdir().unwrap();
    let err = ReadTool::new(ctx(dir.path()))
        .execute(
            &ToolContext::default(),
            json!({"path": ".nuo/virtual/missing.log"}),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("virtual path not found or expired"),
        "{err}"
    );
}

#[tokio::test]
async fn large_text_file_streams_the_requested_page() {
    let dir = tempdir().unwrap();
    let content = (0..3_000)
        .map(|index| format!("{index:04} {}", "x".repeat(80)))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(content.len() > MAX_READ_BYTES);
    std::fs::write(dir.path().join("big.log"), content).unwrap();

    let out = ReadTool::new(Arc::new(SystemToolContext::new(dir.path())))
        .execute(
            &ToolContext::default(),
            json!({"path": "big.log", "offset": 2500, "limit": 3}),
        )
        .await
        .unwrap();

    match out {
        ToolOutput::Code {
            text, start_line, ..
        } => {
            assert_eq!(start_line, 2500);
            assert_eq!(text.lines().count(), 3);
            assert!(text.starts_with("2499 "));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn large_text_file_tail_read_is_bounded() {
    let dir = tempdir().unwrap();
    let content = (0..3_000)
        .map(|index| format!("{index:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("big.log"), content).unwrap();

    let out = ReadTool::new(Arc::new(SystemToolContext::new(dir.path())))
        .execute(
            &ToolContext::default(),
            json!({"path": "big.log", "offset": -3}),
        )
        .await
        .unwrap();

    match out {
        ToolOutput::Code {
            text, start_line, ..
        } => {
            assert_eq!(text, "2997\n2998\n2999");
            assert_eq!(start_line, 2998);
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn long_lines_are_clamped_and_labelled() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("long.txt"), "x".repeat(2_500)).unwrap();
    let out = ReadTool::new(Arc::new(SystemToolContext::new(dir.path())))
        .execute(&ToolContext::default(), json!({"path": "long.txt"}))
        .await
        .unwrap();
    match out {
        ToolOutput::Code { text, .. } => {
            let prefix: String = text.chars().take(MAX_LINE_CHARS).collect();
            assert_eq!(prefix.chars().count(), MAX_LINE_CHARS);
            assert!(text.contains("line truncated to 2000 chars"));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn utf8_bom_is_stripped_once() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("bom.txt"), "\u{FEFF}alpha\nbeta\n").unwrap();
    let out = ReadTool::new(Arc::new(SystemToolContext::new(dir.path())))
        .execute(&ToolContext::default(), json!({"path": "bom.txt"}))
        .await
        .unwrap();
    match out {
        ToolOutput::Code { text, .. } => assert_eq!(text, "alpha\nbeta"),
        other => panic!("expected Code, got {other:?}"),
    }
}

#[test]
fn a_single_long_line_is_consumed_but_not_retained() {
    use std::io::{BufReader, Cursor};

    let text = format!("{}\n", "a".repeat(100_000));
    let mut reader = BufReader::new(Cursor::new(text.into_bytes()));
    let mut line = Vec::new();
    let consumed = read_bounded_line(
        &mut reader,
        &mut line,
        "long.log",
        &[],
        100_001,
        1,
        &ToolContext::default(),
    )
    .expect("bounded line");
    assert_eq!(consumed, 100_001);
    assert!(line.len() <= MAX_LINE_CHARS * 4);
    let mut next = Vec::new();
    assert_eq!(
        read_bounded_line(
            &mut reader,
            &mut next,
            "long.log",
            &[],
            100_001,
            2,
            &ToolContext::default()
        )
        .expect("eof"),
        0
    );
}

#[test]
fn a_truncated_line_stays_on_a_utf8_boundary() {
    use std::io::{BufReader, Cursor};

    let text = format!("{}\n", "€".repeat(3_000));
    let mut reader = BufReader::new(Cursor::new(text.into_bytes()));
    let mut line = Vec::new();
    read_bounded_line(
        &mut reader,
        &mut line,
        "utf8.log",
        &[],
        9_001,
        1,
        &ToolContext::default(),
    )
    .expect("bounded line");
    assert!(line.len() <= MAX_LINE_CHARS * 4);
    assert!(std::str::from_utf8(&line).is_ok());
}

#[tokio::test]
async fn a_repeated_deep_read_uses_the_sparse_index() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("big.log");
    let content = "line\n".repeat(300_000);
    std::fs::write(&path, content).unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let tool = ReadTool::new(Arc::clone(&ctx));
    let args = json!({"path": "big.log", "offset": 250_000, "limit": 2});

    let first = tool
        .execute(&ToolContext::default(), args.clone())
        .await
        .unwrap();
    let version = file_version(&path, &std::fs::metadata(&path).unwrap());
    let checkpoints = ctx.line_index.load(&path, &version);
    assert!(
        !checkpoints.is_empty(),
        "the first scan must seed the index"
    );

    let second = tool.execute(&ToolContext::default(), args).await.unwrap();
    assert_eq!(first.to_text(), second.to_text());
    assert!(
        second.to_text().contains("Lines 250000-250001"),
        "{}",
        second.to_text()
    );
}

#[test]
fn small_files_use_a_content_addressed_version() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("versioned.txt");
    std::fs::write(&path, "aaaa").unwrap();
    let first = file_version(&path, &std::fs::metadata(&path).unwrap());
    assert!(first.starts_with('c'), "{first}");

    std::fs::write(&path, "bbbb").unwrap();
    let second = file_version(&path, &std::fs::metadata(&path).unwrap());
    assert_ne!(first, second, "same-length content must change the version");
}

#[test]
fn sparse_line_index_resumes_at_the_best_checkpoint_and_evicts_lru() {
    let cache = LineIndexCache::default();
    let path = Path::new("indexed.log");
    let version = "v1";
    let mut checkpoints = vec![
        LineCheckpoint {
            offset: 200,
            line: 20,
        },
        LineCheckpoint { offset: 0, line: 0 },
        LineCheckpoint {
            offset: 100,
            line: 10,
        },
    ];
    cache.store(path, version.to_string(), &mut checkpoints);

    let (offset, line, all) = cache.resume(path, version, 15).expect("resume");
    assert_eq!((offset, line), (100, 10));
    assert_eq!(all.len(), 3);
    let (offset, line, _) = cache.resume(path, version, 21).expect("resume");
    assert_eq!((offset, line), (200, 20));
    assert!(cache.resume(path, "v2", 21).is_none());

    // Fill cache to capacity (16) and verify least-recently-used entry is evicted.
    let lru_cache = LineIndexCache::default();
    for i in 0..16 {
        let mut cps = vec![LineCheckpoint {
            offset: 10,
            line: 1,
        }];
        lru_cache.store(Path::new(&format!("f{i}.log")), "v1".to_string(), &mut cps);
    }
    // Touch f0 so f1 becomes the least recently used.
    assert!(!lru_cache.load(Path::new("f0.log"), "v1").is_empty());
    let mut cps = vec![LineCheckpoint {
        offset: 10,
        line: 1,
    }];
    lru_cache.store(Path::new("f16.log"), "v1".to_string(), &mut cps);
    assert!(!lru_cache.load(Path::new("f0.log"), "v1").is_empty());
    assert!(lru_cache.load(Path::new("f1.log"), "v1").is_empty());
}

#[test]
fn a_forward_offset_beyond_the_scan_budget_is_refused() {
    use std::io::{BufReader, Cursor};

    let text = "line\n".repeat(10_000);
    let reader = BufReader::new(Cursor::new(text.into_bytes()));
    let error = stream_lines_with_budget(
        reader,
        Some(9_000),
        2,
        "big.log",
        &ToolContext::default(),
        128,
    )
    .expect_err("scan budget must stop a far offset");
    assert!(error.contains("scanning"), "{error}");
}

#[test]
fn a_cancelled_read_stops_before_scanning_further() {
    use std::io::{BufReader, Cursor};

    let text = "line\n".repeat(10_000);
    let reader = BufReader::new(Cursor::new(text.into_bytes()));
    let ctx = ToolContext::default();
    ctx.cancel_token.cancel();
    let error = stream_lines_with_budget(reader, Some(5), 2, "big.log", &ctx, u64::MAX)
        .expect_err("a cancelled context must abort the scan");
    assert!(error.contains("cancelled"), "{error}");
}

#[tokio::test]
async fn limit_zero_is_rejected() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();

    let err = ReadTool::new(ctx(dir.path()))
        .execute(
            &ToolContext::default(),
            json!({"path": "a.txt", "limit": 0}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("at least 1"), "{err}");
}

#[tokio::test]
async fn virtual_path_resolver_hit_rejects_pages() {
    struct StaticVfs;
    #[async_trait]
    impl tool::VirtualFileResolver for StaticVfs {
        async fn resolve_virtual(&self, _path: &str) -> Option<String> {
            Some("line 1\nline 2\n".to_string())
        }
    }

    let dir = tempdir().unwrap();
    let tool_ctx =
        Arc::new(SystemToolContext::new(dir.path()).with_vfs_resolver(Arc::new(StaticVfs)));
    let err = ReadTool::new(tool_ctx)
        .execute(
            &ToolContext::default(),
            json!({"path": "/tmp/nuo/vfs/abc123.log", "pages": "1"}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("only valid for PDF"), "{err}");
}

#[test]
fn reverse_seek_tail_handles_single_huge_line_without_unbounded_buffering() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("single.json");
    // 512 KiB of 3-byte UTF-8 chars with zero newlines.
    let body = "€".repeat(175_000);
    std::fs::write(&path, &body).unwrap();
    let len = std::fs::metadata(&path).unwrap().len();
    assert!(len > 500_000);

    let mut file = std::fs::File::open(&path).unwrap();
    let page = tail_from_eof(
        &mut file,
        len,
        5,
        "single.json",
        b"\xe2\x82\xac",
        &ToolContext::default(),
        MAX_READ_SCAN_BYTES,
    )
    .expect("oversized single line tail");
    assert_eq!(page.start_line, 0);
    assert_eq!(page.tail_of_bytes, Some(len));
    assert!(page.text.contains("line truncated to 2000 chars"));
    assert!(page.text.len() <= MAX_READ_BYTES);
}

#[test]
fn reverse_seek_tail_matches_str_lines_on_single_newline_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nl.txt");
    std::fs::write(&path, "\n").unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    let page = tail_from_eof(
        &mut file,
        1,
        5,
        "nl.txt",
        b"\n",
        &ToolContext::default(),
        MAX_READ_SCAN_BYTES,
    )
    .expect("single newline");
    assert_eq!(page.total, Some(1));
    assert_eq!(page.start_line, 1);
    assert_eq!(page.text, "");
}

#[test]
fn reverse_seek_tail_reports_nul_near_eof_accurately() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("corrupt.log");
    let mut bytes = "normal line\n".repeat(10_000).into_bytes();
    bytes.extend_from_slice(b"tail\0corrupt\n");
    std::fs::write(&path, &bytes).unwrap();

    let mut file = std::fs::File::open(&path).unwrap();
    let err = tail_from_eof(
        &mut file,
        bytes.len() as u64,
        5,
        "corrupt.log",
        b"normal line\n",
        &ToolContext::default(),
        MAX_READ_SCAN_BYTES,
    )
    .expect_err("NUL near EOF must fail");
    assert!(err.contains("binary file"), "{err}");
    assert!(err.contains("NUL byte near EOF"), "{err}");
    assert!(!err.contains("in the first"), "{err}");
}

#[tokio::test]
async fn read_tool_uses_reverse_seek_tail_above_threshold() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("big.log");
    let line = format!("{}\n", "a".repeat(100));
    let repeats = (TAIL_REVERSE_SEEK_THRESHOLD as usize / line.len()) + 50;
    let mut body = line.repeat(repeats);
    body.push_str("tail-penultimate\ntail-final\n");
    std::fs::write(&path, &body).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > TAIL_REVERSE_SEEK_THRESHOLD);

    let out = ReadTool::new(ctx(dir.path()))
        .execute(
            &ToolContext::default(),
            json!({"path": "big.log", "offset": -2}),
        )
        .await
        .unwrap();
    match out {
        ToolOutput::Code {
            text,
            start_line,
            prefix,
            ..
        } => {
            assert_eq!(start_line, 0);
            assert_eq!(text, "tail-penultimate\ntail-final");
            assert!(prefix.unwrap().contains("Last 2 lines of ~"));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn pdf_cache_normalizes_equivalent_paths() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingExtractor(AtomicUsize);
    impl DocumentExtractor for CountingExtractor {
        fn extract(
            &self,
            _mime: &str,
            _data: &[u8],
            _page_range: Option<PageRange>,
        ) -> Result<ExtractedDocument, DocumentError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(extracted("[page 1]\nhello", 1, 1, false))
        }
    }

    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("doc.pdf"), b"%PDF-1.7\nbody").unwrap();
    let extractor = Arc::new(CountingExtractor(AtomicUsize::new(0)));
    let tool_ctx = Arc::new(
        SystemToolContext::new(dir.path())
            .with_document_extractor(extractor.clone() as Arc<dyn DocumentExtractor>),
    );
    let read = ReadTool::new(tool_ctx);

    let _ = read
        .execute(&ToolContext::default(), json!({"path": "doc.pdf"}))
        .await
        .unwrap();
    let _ = read
        .execute(&ToolContext::default(), json!({"path": "./doc.pdf"}))
        .await
        .unwrap();
    assert_eq!(
        extractor.0.load(Ordering::SeqCst),
        1,
        "equivalent lexical paths must share one cache entry"
    );
}

#[tokio::test]
async fn large_image_above_first_chunk_streams_bounded_and_enforces_ceiling() {
    let dir = tempdir().unwrap();
    let mut big_png = b"\x89PNG\r\n\x1a\n".to_vec();
    big_png.resize(300 * 1024, 0xAB);
    std::fs::write(dir.path().join("big.png"), &big_png).unwrap();

    let out = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": "big.png"}))
        .await
        .unwrap();
    match out {
        ToolOutput::Image { mime, data } => {
            assert_eq!(mime, "image/png");
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap();
            assert_eq!(decoded.len(), big_png.len());
        }
        other => panic!("expected Image, got {other:?}"),
    }

    let huge_path = dir.path().join("huge.png");
    let mut file = std::fs::File::create(&huge_path).unwrap();
    std::io::Write::write_all(&mut file, b"\x89PNG\r\n\x1a\n").unwrap();
    file.set_len(super::image::MAX_MEDIA_BYTES + 1).unwrap();

    let err = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": "huge.png"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("image ingest ceiling"), "{err}");
}
