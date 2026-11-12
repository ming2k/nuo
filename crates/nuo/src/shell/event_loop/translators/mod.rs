//! Stream translators for the event loop (M1).
//!
//! The response translator and monitor translator consume wire frames and produce
//! typed `AppMutation`s onto a bounded channel. They own no application state.

pub(crate) mod monitor;
pub(crate) mod response;

pub(crate) use monitor::spawn_monitor_translator;
pub(crate) use response::{ResponseTranslatorConfig, spawn_response_translator};
