//! Constructors for harness-authored model-context messages.
//!
//! Call sites retain control over *when* context is inserted, while this module
//! owns the role, visibility, provenance, and attachment invariants of the
//! resulting [`Message`]. Genuine user input, assistant output, and tool-result
//! protocol messages are not harness context and keep their source-owned paths.

use crate::{ImagePart, InjectionKind, InjectionOrigin, Message, Role};

/// Build a hidden user-role context message.
pub(crate) fn hidden_user(kind: InjectionKind, content: impl Into<String>) -> Message {
    Message::injected(Role::User, content, InjectionOrigin::new(kind))
}

/// Build a hidden user-role context message with a provenance reason.
pub(crate) fn hidden_user_with_reason(
    kind: InjectionKind,
    reason: impl Into<String>,
    content: impl Into<String>,
) -> Message {
    Message::injected(
        Role::User,
        content,
        InjectionOrigin::new(kind).with_reason(reason),
    )
}

/// Build visible harness-authored user context, used for live steering.
pub(crate) fn visible_user(kind: InjectionKind, content: impl Into<String>) -> Message {
    Message::new(Role::User, content).with_origin(InjectionOrigin::new(kind))
}

/// Build the user-role image companion for a tool result.
pub(crate) fn tool_image(source: &str, mime: String, data: String) -> Message {
    Message::new(Role::User, format!("Image from {source}"))
        .with_images(vec![ImagePart { mime, data }])
        .with_origin(InjectionOrigin::new(InjectionKind::ToolImage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_context_stamps_role_visibility_and_origin() {
        let message = hidden_user(InjectionKind::InterAgent, "steer");
        assert_eq!(message.role, Role::User);
        assert!(message.hidden);
        assert_eq!(
            message.origin.as_ref().map(|origin| origin.kind),
            Some(InjectionKind::InterAgent)
        );
    }

    #[test]
    fn visible_context_is_not_hidden() {
        let message = visible_user(InjectionKind::SubagentSteer, "new task");
        assert_eq!(message.role, Role::User);
        assert!(!message.hidden);
    }

    #[test]
    fn tool_image_stamps_its_projection_origin() {
        let message = tool_image("screenshot", "image/png".into(), "bytes".into());
        assert_eq!(message.role, Role::User);
        assert!(!message.hidden);
        assert_eq!(
            message.origin.as_ref().map(|origin| origin.kind),
            Some(InjectionKind::ToolImage)
        );
        assert_eq!(message.images.as_ref().map(Vec::len), Some(1));
    }
}
