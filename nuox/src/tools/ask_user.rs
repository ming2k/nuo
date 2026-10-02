use super::{ToolPresenter, ToolView};

pub struct AskUserPresenter;

impl ToolPresenter for AskUserPresenter {
    fn summary(&self, view: &ToolView) -> String {
        let count = view
            .args
            .get("questions")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(1);
        let first = view
            .args
            .get("questions")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|q| q.get("question"))
            .and_then(|v| v.as_str());
        if let Some(q) = first {
            if count > 1 {
                format!("Ask {} questions: {}", count, q)
            } else {
                format!("Ask: {}", q)
            }
        } else {
            format!(
                "Ask {} question{}",
                count,
                if count == 1 { "" } else { "s" }
            )
        }
    }
}
