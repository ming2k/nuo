//! End-to-end `read` dispatch for PDFs through the real host extractor.
//!
//! The unit suites test the tool seam with a fake extractor and the host
//! extractor with fixture bytes. This suite is the linkage check: the real
//! `ReadTool` must find the injected `HostDocumentExtractor`, classify the
//! file as a PDF, and return the paginated `Code` shape the model sees.

use std::sync::Arc;

use nuo_tool::{Tool, ToolContext, ToolOutput};
use nuo_tool_fs::{ReadTool, SystemToolContext};
use serde_json::json;

const TWO_PAGE: &[u8] = include_bytes!("../../../crates/nuo-host/testdata/two_page.pdf");
const NO_TEXT: &[u8] = include_bytes!("../../../crates/nuo-host/testdata/no_text.pdf");

fn tool(dir: &std::path::Path) -> ReadTool {
    let ctx = Arc::new(
        SystemToolContext::new(dir)
            .with_document_extractor(Arc::new(nuo_host::document::HostDocumentExtractor)),
    );
    ReadTool::new(ctx)
}

#[tokio::test]
async fn read_extracts_a_real_pdf_through_the_host_seam() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.pdf"), TWO_PAGE).unwrap();

    let out = tool(dir.path())
        .execute(&ToolContext::default(), json!({"path": "doc.pdf"}))
        .await
        .unwrap();

    match out {
        ToolOutput::Code {
            text, prefix, lang, ..
        } => {
            assert_eq!(lang, None);
            assert!(text.contains("[page 1]"));
            assert!(text.contains("Page one hello"));
            assert!(text.contains("[page 2]"));
            assert!(text.contains("Page two world"));
            assert!(prefix.unwrap().contains("PDF text, 2 pages"));
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

#[tokio::test]
async fn read_names_a_pdf_with_no_text_layer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("scan.pdf"), NO_TEXT).unwrap();

    let err = tool(dir.path())
        .execute(&ToolContext::default(), json!({"path": "scan.pdf"}))
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("no extractable text"),
        "expected a typed no-text failure, got {message}"
    );
    assert!(message.contains("scanned PDF"));
}
