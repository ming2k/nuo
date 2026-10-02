//! Clipboard integration: OSC52 terminal sequences + system clipboard.
//!
//! Delegates low-level cross-platform clipboard interactions to [`nuo_host::clipboard`].

pub use nuo_host::clipboard::{
    ClipboardRead, CopyOutcome, PlatformClipboard, base64_encode_bytes, file_uri_to_path,
    parse_gnome_copied_files, parse_uri_list, paste_text_as_file_paths, write_osc52,
};

/// Copy text to the clipboard (native system clipboard or OSC 52 sequence).
pub async fn copy(text: &str) -> Result<CopyOutcome, String> {
    PlatformClipboard::copy_text(text).await
}

/// Read content from the clipboard (image, file references, or plain text).
pub async fn read() -> ClipboardRead {
    PlatformClipboard::read().await
}

/// Base64 encode an image payload for inline attachment previews.
#[must_use]
pub fn base64_image(data: &[u8]) -> String {
    base64_encode_bytes(data)
}

/// Adapter implementing [`nuo_client::UiBridge`] by delegating to the TUI's
/// real clipboard path. Used by the slash-command dispatcher so it stays frontend-agnostic.
pub struct TuiClipboard;

#[async_trait::async_trait]
impl nuo_client::UiBridge for TuiClipboard {
    async fn copy_to_clipboard(&self, text: &str) -> Result<nuo_client::CopyOutcome, String> {
        copy(text).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn copy_and_read_api_contracts() {
        let outcome = copy("hello").await;
        // In CI/headless, this may fall back to OSC 52 or succeed natively
        assert!(outcome.is_ok());
    }

    #[test]
    fn paste_text_as_file_paths_requires_every_line_to_be_an_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let png = dir.path().join("shot.png");
        std::fs::write(&png, b"png").expect("write file");
        let note = dir.path().join("note.txt");
        std::fs::write(&note, b"x").expect("write file");

        assert_eq!(
            paste_text_as_file_paths(png.to_str().unwrap()).as_deref(),
            Some(&[png.clone()][..])
        );
        assert_eq!(
            paste_text_as_file_paths(&format!("file://{}\n", png.display())).as_deref(),
            Some(&[png.clone()][..])
        );
        assert_eq!(
            paste_text_as_file_paths(&format!("{}\n{}", png.display(), note.display())).as_deref(),
            Some(&[png.clone(), note.clone()][..])
        );
        assert_eq!(
            paste_text_as_file_paths(&format!("see {} please", png.display())),
            None
        );
        assert_eq!(
            paste_text_as_file_paths(dir.path().join("missing.png").to_str().unwrap()),
            None
        );
        assert_eq!(paste_text_as_file_paths(dir.path().to_str().unwrap()), None);
        assert_eq!(paste_text_as_file_paths("relative/shot.png"), None);
        assert_eq!(
            paste_text_as_file_paths("https://example.com/shot.png"),
            None
        );
        assert_eq!(paste_text_as_file_paths(""), None);
    }
}
