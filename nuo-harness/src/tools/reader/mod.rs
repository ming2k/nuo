//! Pluggable page-content backends ("readers") for `read_url` — the depth
//! half of the two-stage research pipeline (websearch = breadth, read_url =
//! depth; ADR-0117).
//!
//! A reader turns one URL into clean page text. The Jina reader delegates to
//! `r.jina.ai`, which renders JavaScript and extracts the main content
//! server-side as Markdown.
//!
//! SSRF note: readers receive only URLs that already passed
//! [`crate::tools::ssrf::assert_public_url`]. The Jina reader sends the URL to
//! a third party, so it must never be pointed at private addresses — the
//! pre-check in `read_url` enforces this before any reader runs.

use crate::tools::reader::jina::ReadPage;

pub mod jina;

/// Which page-content backend `read_url` uses.
pub(crate) enum Reader {
    Jina(jina::JinaReader),
    Disabled,
}

pub(crate) fn build_reader(cfg: &nuo_wire::WebRuntimeConfig) -> Reader {
    match cfg.behavior.reader {
        nuo_wire::WebReaderProvider::Jina => Reader::Jina(jina::JinaReader {
            api_key: cfg
                .reader_credential
                .as_ref()
                .map(|k| k.expose_secret().to_string()),
        }),
        nuo_wire::WebReaderProvider::Disabled => Reader::Disabled,
    }
}

impl Reader {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Reader::Jina(_) => "jina",
            Reader::Disabled => "disabled",
        }
    }

    /// Read one URL and return clean page text plus the content type of the
    /// underlying response. `raw` skips text extraction for non-HTML
    /// content when applicable.
    ///
    /// Errors are surfaced verbatim to the model/user.
    pub(crate) async fn read(
        &self,
        client: &crate::tools::web::http::WebHttp,
        url: &str,
        _raw: bool,
    ) -> Result<ReaderOutput, String> {
        match self {
            Reader::Jina(j) => j.read(client, url).await,
            Reader::Disabled => Err("web reader is disabled in configuration".to_string()),
        }
    }
}

/// What a reader produced for one URL.
pub(crate) struct ReaderOutput {
    /// Clean text, ready for the model. For non-HTML content or `raw=true`
    /// this is the body verbatim.
    pub text: String,
    /// Content type reported by the *underlying* fetch (e.g. from Jina's
    /// target response), used by `read_url` to label the output.
    #[allow(dead_code)]
    pub content_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reader field is an enum now: an unknown name fails at parse time,
    /// so `build_reader` can never receive one.
    fn runtime_from_toml(text: &str) -> nuo_wire::WebRuntimeConfig {
        let behavior: nuo_wire::WebConfig =
            toml::from_str(text).expect("reader field parses");
        nuo_wire::WebRuntimeConfig {
            behavior,
            search_credential: None,
            reader_credential: None,
        }
    }

    #[test]
    fn an_unknown_reader_name_is_rejected_at_parse_time() {
        let error = toml::from_str::<nuo_wire::WebConfig>("reader = \"totally-bogus\"")
            .expect_err("unknown reader names must not parse");
        assert!(
            error.to_string().contains("unsupported web reader"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn build_reader_disables_when_configured() {
        assert_eq!(
            build_reader(&runtime_from_toml("reader = \"disabled\"")).name(),
            "disabled"
        );
    }

    #[test]
    fn build_reader_selects_jina_by_name() {
        assert_eq!(
            build_reader(&runtime_from_toml("reader = \"jina\"")).name(),
            "jina"
        );
    }
}
