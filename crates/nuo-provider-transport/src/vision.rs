//! Route-scoped image projection for the wire (ADR-0230).
//!
//! Image attachments live in two different planes, and conflating them is the
//! bug this module exists to prevent:
//!
//! - **Durable**: the transcript stores pasted images as content-addressed
//!   blobs and keeps them forever (ADR-0186). History is never rewritten
//!   because the user switched models mid-session.
//! - **Per request**: the wire body is a projection of that history, built
//!   fresh for every turn against the *current* route's capabilities.
//!
//! So "the history contains an image and the new model cannot see it" is not a
//! per-message decision that must be remembered — it is a projection decision
//! taken once per request, here, for every transport.
//!
//! The policy is deliberately narrow (see
//! [`ModelCapabilities::accepts_images`]): only a route that *declared* no
//! image input (`Some(false)`) has its attachments dropped. An undeclared route
//! (`None`) is attempted — providers reject images loudly, while a client that
//! strips them silently leaves the model answering confidently about a picture
//! it never received.

use nuo_model_codec::{Message, ModelCapabilities};

/// Project `messages` for a route whose image support is `capabilities`.
///
/// Returns the projected list and the number of `ImagePart`s dropped, so a
/// caller that wants to tell the user can count them without re-scanning the
/// request. Text content is always preserved: the model just does not see the
/// pixels.
pub fn project_images_for_route(
    model: &str,
    messages: Vec<Message>,
    capabilities: &ModelCapabilities,
) -> (Vec<Message>, usize) {
    if capabilities.accepts_images() {
        return (messages, 0);
    }

    let mut dropped = 0usize;
    let projected = messages
        .into_iter()
        .map(|mut message| {
            if let Some(images) = message.images.take() {
                dropped += images.len();
            }
            message
        })
        .collect();

    if dropped > 0 {
        tracing::debug!(
            target: "nuo_model_codec::provider",
            model = %model,
            dropped,
            "dropping images for a route that declares no image input",
        );
    }
    (projected, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_model_codec::{ImagePart, ModelCapabilities, Role};

    fn caps(vision: Option<bool>) -> ModelCapabilities {
        let mut capabilities = ModelCapabilities::for_channel("test-model", None);
        capabilities.vision = vision;
        capabilities
    }

    fn image_message(text: &str) -> Message {
        Message::new(Role::User, text).with_images(vec![ImagePart {
            mime: "image/png".to_string(),
            data: "aGk=".to_string(),
        }])
    }

    #[test]
    fn declared_text_only_route_drops_images_and_counts_them() {
        let messages = vec![image_message("look"), Message::new(Role::Assistant, "hm")];
        let (projected, dropped) = project_images_for_route("m", messages, &caps(Some(false)));
        assert_eq!(dropped, 1);
        assert!(projected[0].images.is_none());
        // The prose survives — only the pixels are gone.
        assert_eq!(projected[0].content, "look");
    }

    #[test]
    fn undeclared_route_keeps_images() {
        // The whole point of ADR-0230: an endpoint that advertises nothing must
        // not have its images silently stripped.
        let messages = vec![image_message("look")];
        let (projected, dropped) = project_images_for_route("m", messages, &caps(None));
        assert_eq!(dropped, 0);
        assert_eq!(projected[0].images.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn declared_vision_route_keeps_images() {
        let messages = vec![image_message("look")];
        let (projected, dropped) = project_images_for_route("m", messages, &caps(Some(true)));
        assert_eq!(dropped, 0);
        assert_eq!(projected[0].images.as_ref().map(Vec::len), Some(1));
    }
}
