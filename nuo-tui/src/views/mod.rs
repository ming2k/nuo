//! Full-screen scene destinations (ADR-0205).
//!
//! A **scene** is an independent, full-screen destination (`Conversation`, `Dashboard`,
//! `Settings`, `TaskInspection`, `Aside`). Dialogs and sheets float over scenes and
//! never own the full screen.

pub mod settings;

#[allow(unused_imports)]
pub use settings::{
    ConfigCategory, ConfigFocus, ConfigRects, SettingsProps, build_websearch_provider_dropdown,
    build_websearch_reader_dropdown, draw_settings_view,
};
