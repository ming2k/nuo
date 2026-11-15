use super::*;
use serde_json::json;
use tempfile::tempdir;
use tool::ToolContext;

fn ctx(root: &std::path::Path) -> Arc<SystemToolContext> {
    Arc::new(SystemToolContext::new(root))
}

struct FakeExtractor {
    result: Result<ExtractedDocument, DocumentError>,
}

impl DocumentExtractor for FakeExtractor {
    fn extract(&self, _mime: &str, _data: &[u8]) -> Result<ExtractedDocument, DocumentError> {
        self.result.clone()
    }
}

fn ctx_with_extractor(
    root: &std::path::Path,
    result: Result<ExtractedDocument, DocumentError>,
) -> Arc<SystemToolContext> {
    Arc::new(
        SystemToolContext::new(root).with_document_extractor(Arc::new(FakeExtractor { result })),
    )
}

fn extracted(text: &str, pages: usize, total_pages: usize, truncated: bool) -> ExtractedDocument {
    ExtractedDocument {
        text: text.to_string(),
        pages,
        total_pages,
        truncated,
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
async fn directory_listing_is_one_level_sorted_and_paginated() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("b.txt"), "b").unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();

    let out = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": ".", "limit": 2}))
        .await
        .unwrap();

    match out {
        ToolOutput::Listing {
            entries,
            total,
            offset,
            next,
        } => {
            assert_eq!(total, Some(3));
            assert_eq!(offset, 1);
            assert_eq!(entries, vec!["sub/", "a.txt"]);
            assert_eq!(next, Some(3));
        }
        other => panic!("expected Listing, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_directory_reports_zero_entries() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("empty")).unwrap();

    let out = ReadTool::new(ctx(dir.path()))
        .execute(&ToolContext::default(), json!({"path": "empty"}))
        .await
        .unwrap();

    assert_eq!(out.to_text(), "[0 entries]");
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
        } => {
            assert_eq!(text, "b\nc");
            assert_eq!(start_line, 2);
            assert_eq!(lang.as_deref(), Some("rs"));
            assert_eq!(prefix.as_deref(), Some("[Lines 2-3 of 4 from `src.rs`]"));
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
async fn document_extraction_timeout_is_a_named_failure() {
    struct SlowExtractor;
    impl DocumentExtractor for SlowExtractor {
        fn extract(&self, _mime: &str, _data: &[u8]) -> Result<ExtractedDocument, DocumentError> {
            std::thread::sleep(std::time::Duration::from_millis(50));
            Ok(extracted("late", 1, 1, false))
        }
    }

    let err = run_document_extraction(
        Arc::new(SlowExtractor),
        b"irrelevant".to_vec(),
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
