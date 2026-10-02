//! Presenters for `read_url` and `search_web`.

use super::{ResultKind, ToolPresenter, ToolView, truncate};

pub struct WebReaderPresenter;

impl ToolPresenter for WebReaderPresenter {
    fn summary(&self, view: &ToolView) -> String {
        view.str("url")
            .map(|url| format!("Read {}", url))
            .unwrap_or_else(|| "Read URL".to_string())
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::WebArticle
    }
}

pub struct WebSearchPresenter;

impl ToolPresenter for WebSearchPresenter {
    fn summary(&self, view: &ToolView) -> String {
        view.str("query")
            .map(|query| format!("Web search \"{}\"", truncate(query, 52)))
            .unwrap_or_else(|| "Web search".to_string())
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::WebSearch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_presenters_declare_semantic_result_kinds() {
        assert_eq!(WebReaderPresenter.result_kind(), ResultKind::WebArticle);
        assert_eq!(WebSearchPresenter.result_kind(), ResultKind::WebSearch);
    }
}
