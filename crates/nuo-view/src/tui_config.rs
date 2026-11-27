//! Pure zero-IO TUI presentation configuration model (`[INV-ARCH-VIEW-ZERO-IO]`).
//!
//! Disk loading/saving (`$XDG_CONFIG_HOME/nuo/terminal.toml`) and keymap
//! override resolution remain in `nuo::shell::config`; this module owns the
//! declarative data types and pure disclosure-default accessors consumed by
//! `nuo_view::views::settings`.

use std::collections::HashMap;

use nuo_ui_primitives::ColorSchemeConfig;
use serde::{Deserialize, Serialize};

use crate::disclosure_config::{DisclosureSource, reasoning_expanded, tool_expanded};
use crate::tools::ToolComponent;

pub use crate::disclosure_config::THINKING_KEY;

fn default_true() -> bool {
    true
}

/// Input-history behaviour: prompt dedup and slash-command persistence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputHistoryConfig {
    pub dedup: bool,
    pub record_commands: bool,
}

impl Default for InputHistoryConfig {
    fn default() -> Self {
        Self {
            dedup: true,
            record_commands: false,
        }
    }
}

/// Complete configuration for the TUI frontend application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    pub color_scheme: String,
    #[serde(default = "default_true")]
    pub click_outside_dismiss: bool,
    pub expand_auto_scroll: bool,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub default_expanded: HashMap<String, bool>,
    #[serde(default)]
    pub custom_color_scheme: ColorSchemeConfig,
    #[serde(default)]
    pub input_history: InputHistoryConfig,
    /// User remaps of the chords: bare `command-id → chord spec`
    /// entries for the global layer (e.g. `interrupt = "ctrl+x"`), plus a
    /// `[keybindings.session]` sub-table for the full-screen-view verbs.
    #[serde(default, skip_serializing_if = "KeybindingsConfig::is_empty")]
    pub keybindings: KeybindingsConfig,
}

/// User chord remaps declared under `[keybindings]` in `terminal.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeybindingsConfig {
    /// Global chords: `command-id → chord spec`.
    #[serde(flatten)]
    pub commands: HashMap<String, String>,
    /// Surface verbs: `verb → chord spec` (`open_history`, `toggle_send_mode`, …).
    pub session: HashMap<String, String>,
}

impl KeybindingsConfig {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.session.is_empty()
    }
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            color_scheme: String::new(),
            click_outside_dismiss: true,
            expand_auto_scroll: false,
            default_expanded: HashMap::new(),
            custom_color_scheme: ColorSchemeConfig::default(),
            input_history: InputHistoryConfig::default(),
            keybindings: KeybindingsConfig::default(),
        }
    }
}

impl DisclosureSource for TuiConfig {
    fn disclosure_default(&self, name: &str) -> Option<bool> {
        self.default_expanded.get(name).copied()
    }
}

/// Effective default-expand state for a tool step. An explicit config entry
/// wins; otherwise the declared component default applies; a tool with no
/// declared component (MCP / unknown) stays collapsed.
pub fn tool_default_expanded(config: &TuiConfig, name: &str) -> bool {
    tool_expanded(config, name)
}

/// Effective default-expand state for a reasoning trace. Defaults to
/// collapsed (`false`) when not configured.
pub fn reasoning_default_expanded(config: &TuiConfig) -> bool {
    reasoning_expanded(config)
}

/// Record a user's disclosure choice for one interactive tool component.
///
/// If `expanded` equals the component's declared default, the override is pruned
/// from `config.default_expanded` so the configuration stays minimal and clean.
/// When it differs, every name the component claims is written so aliases
/// honour the choice (ADR-0020).
pub fn set_component_default_expanded(
    config: &mut TuiConfig,
    component: &ToolComponent,
    expanded: bool,
) {
    for name in component.names() {
        if expanded == component.default_expanded() {
            config.default_expanded.remove(name);
        } else {
            config.default_expanded.insert(name.to_string(), expanded);
        }
    }
}
