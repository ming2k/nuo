//! Overlay modal renderers, split by functional domain.
//!
//! Sub-modules:
//! - [`provider`] — provider picker + API-key / model-id editor
//! - [`session`] — sessions picker + session-context dashboard modal
//! - [`tools`] — tools manager modal (the interactive tool-list surface)
//! - [`skills`] — skills modal (loaded-skill list with detail expansion)
//! - [`mcp`] — MCP manager modal (per-server enable/reconnect surface)
//! - [`permission`] — permission sheet + question modal
//! - [`history`] — history search modal
//! - [`crate::views::settings`] — full-screen dual-pane settings view
//! - [`toast`] — copy / armed-action notice bubbles
//! - [`common`] — shared helpers (time formatting, truncation, caret, glyphs)

pub mod btw;
pub mod command_palette;
pub mod common;
pub mod dashboard;
pub mod dialog_keys;
pub mod history;
pub mod mcp;
pub mod permission;
pub mod permissions_manager;
pub mod provider;
pub mod provider_delete_confirm;
pub mod queue;
pub mod session;
pub mod skills;
pub mod telemetry;
pub mod toast;
pub mod tools;
pub mod tree;
pub mod usage_stats;

#[allow(unused_imports)]
pub use crate::views::settings::{
    ConfigCategory, ConfigFocus, SettingsProps, build_websearch_provider_dropdown,
    build_websearch_reader_dropdown, draw_settings_view,
};
pub use dashboard::{
    ConsoleCommand, ConsoleLine, ConsoleVerb, DashboardFocus, creation_order, draw_dashboard,
    draw_session_preview, parse_console_command,
};
// `DashboardRects` is used by the event loop via `draw_dashboard`'s return; it
// is part of the module's public API surface.
#[allow(unused_imports)]
pub use dashboard::DashboardRects;
pub use history::draw_history_panel;
// The old centered `/host` modal (`host.rs`) was superseded by the full-screen
// `dashboard` surface and removed; `/host` now opens the dashboard.
pub use btw::{BtwModalProps, draw_btw_modal};
pub(crate) use command_palette::draw_command_palette;
pub use dialog_keys::draw_dialog_keys;
pub use mcp::draw_mcp_modal;
pub use permission::{
    draw_input_injection, draw_permission_sheet, draw_question_modal, permission_action_count,
};
pub use permissions_manager::draw_permissions_manager;
pub use provider::{
    CustomEditorProps, draw_connections_modal, draw_custom_provider_editor, draw_model_editor,
    draw_models_modal, draw_oauth_pending, draw_preset_chooser, draw_web_value_editor,
};
pub use provider_delete_confirm::draw_provider_delete_confirm;
pub use queue::{QueueModalProps, draw_queue_modal};
pub use session::draw_sessions_modal;
pub use skills::draw_skills_modal;
pub use telemetry::{
    ContextUsageProps, draw_telemetry_modal, telemetry_attempt_count, telemetry_attempt_key,
    telemetry_round_count,
};
pub use toast::{draw_armed_toast, draw_copy_toast, draw_notice_toast};
pub use tools::draw_tools_modal;
pub use tree::draw_tree_modal;
pub use usage_stats::draw_usage_stats_modal;
