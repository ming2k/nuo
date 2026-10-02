//! The Connections (provider-instance management) and Models (flat
//! provider/model picker) modals, the API-key / model-id editor, and the
//! custom-provider editor modals.

pub mod common;
pub mod connections;
pub mod editor;
pub mod models;
pub mod oauth;

#[cfg(test)]
mod tests;

pub use connections::draw_connections_modal;
pub use editor::{
    CustomEditorProps, draw_custom_provider_editor, draw_model_editor, draw_preset_chooser,
    draw_web_value_editor,
};
pub use models::draw_models_modal;
pub use oauth::draw_oauth_pending;
