//! High-level browser opener for the TUI application, backed by `nuo_host::opener`.

use nuo_host::opener::{OpenOutcome, SystemOpener};

/// Open a URL in the user's default browser with extensive cross-platform fallbacks.
pub fn open_browser(url: &str) -> Result<(), String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("Unsupported URL scheme: {url}"));
    }

    match SystemOpener::open_url(url) {
        Ok(OpenOutcome::Launched { .. }) => Ok(()),
        Ok(OpenOutcome::Headless {
            url_or_path,
            osc8_link,
        }) => {
            // In headless/SSH mode, print clickable OSC 8 sequence or link to stderr
            if let Some(link) = osc8_link {
                eprintln!("\n[browser] Open this URL: {link}\n");
            } else {
                eprintln!("\n[browser] Open this URL: {url_or_path}\n");
            }
            Ok(())
        }
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_browser_rejects_invalid_scheme() {
        assert!(open_browser("ftp://example.com").is_err());
        assert!(open_browser("file:///etc/passwd").is_err());
        assert!(open_browser("javascript:alert(1)").is_err());
    }
}
