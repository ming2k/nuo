//! Presenter for `read_image`.

use super::{ToolPresenter, ToolView};
use crate::components::inline_layout::SemanticLine;
use crate::components::path::PathView;

pub struct ReadImagePresenter;

impl ToolPresenter for ReadImagePresenter {
    fn render_summary<'a>(&self, view: &'a ToolView) -> SemanticLine<'a> {
        if let Some(path) = view.str("path") {
            SemanticLine::new()
                .push_fixed("Read image ")
                .push_path(PathView::from_str(path).maybe_base_dir(view.workspace_root))
        } else {
            SemanticLine::plain("Read image")
        }
    }

    fn summary(&self, view: &ToolView) -> String {
        self.render_summary(view).to_plain_text()
    }
    // `result_kind` defaults to `Code`, which renders the model-facing
    // placeholder text ("[image: image/png]") in a code block. The actual
    // pixels are not drawn in-terminal (most terminals lack a reliable image
    // protocol); the image is delivered to the model out-of-band via the
    // peel-out user message, so the on-screen block just confirms what was sent.
}

#[cfg(test)]
mod tests {
    use super::{ReadImagePresenter, ToolPresenter, ToolView};
    use serde_json::{Map, Value, json};

    fn view(args: Value) -> ToolView<'static> {
        let owned: Map<String, Value> = match args {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        let args = Box::leak(owned.into());
        ToolView {
            name: "read_image",
            args,
            profile: None,
            workspace_root: None,
        }
    }

    #[test]
    fn summary_names_the_image_path() {
        let v = view(json!({"path": "screenshots/bug.png"}));
        assert_eq!(
            ReadImagePresenter.summary(&v),
            "Read image screenshots/bug.png"
        );
    }

    #[test]
    fn summary_falls_back_without_path() {
        let v = view(json!({}));
        assert_eq!(ReadImagePresenter.summary(&v), "Read image");
    }
}
