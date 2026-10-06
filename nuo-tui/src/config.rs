//! TUI presentation configuration and state.
//!
//! Stored in the unified `nuo` namespace at `$XDG_CONFIG_HOME/nuo/tui.toml`
//! — a sibling of the daemon's `config.toml`, not a field inside it — so the
//! frontend's presentation state and the daemon's behavioural policy stay
//! cleanly decoupled while both live under one application directory
//! (ADR-0011).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use nuo_wire::ColorSchemeConfig;

pub const THINKING_KEY: &str = "thinking";

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
    pub tool_density: bool,
    #[serde(default)]
    pub default_expanded: HashMap<String, bool>,
    #[serde(default)]
    pub custom_color_scheme: ColorSchemeConfig,
    #[serde(default)]
    pub input_history: InputHistoryConfig,
    /// User remaps of the chords (ADR-0172): bare `command-id → chord spec`
    /// entries for the global layer (e.g. `interrupt = "ctrl+x"`), plus a
    /// `[keybindings.session]` sub-table for the full-screen-view verbs. See
    /// [`crate::keymap::parse_key`] for the chord syntax.
    #[serde(default)]
    pub keybindings: KeybindingsConfig,
}

/// The `[keybindings]` table (ADR-0172). Unknown top-level keys are the
/// global command remaps; the nested `session` table holds the surface verbs
/// of the full-screen views.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeybindingsConfig {
    /// Global chords: `command-id → chord spec`.
    #[serde(flatten)]
    pub commands: HashMap<String, String>,
    /// Surface verbs: `verb → chord spec` (step 9). Keys are the unqualified
    /// [`crate::keymap::SurfaceVerb`] names (`open_history`, `toggle_send_mode`, …).
    pub session: HashMap<String, String>,
}

pub type MutxConfig = TuiConfig;

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            color_scheme: String::new(),
            click_outside_dismiss: true,
            expand_auto_scroll: false,
            tool_density: false,
            default_expanded: HashMap::new(),
            custom_color_scheme: ColorSchemeConfig::default(),
            input_history: InputHistoryConfig::default(),
            keybindings: KeybindingsConfig::default(),
        }
    }
}

impl TuiConfig {
    /// The effective global-chord overrides declared in `[keybindings]`
    /// (ADR-0172). Empty when the user has not customized any global chord.
    pub fn global_key_overrides(&self) -> crate::keymap::GlobalOverrides {
        crate::keymap::GlobalOverrides::from_config(&self.keybindings.commands)
    }
    /// The effective surface-verb overrides declared in `[keybindings.session]`
    /// (ADR-0172 step 9). Empty when the user has not customized any
    /// full-screen-view chord.
    pub fn surface_key_overrides(&self) -> crate::keymap::SurfaceOverrides {
        crate::keymap::SurfaceOverrides::from_config(&self.keybindings.session)
    }
    /// Load configuration from `$XDG_CONFIG_HOME/nuo/tui.toml`.
    pub fn load() -> Self {
        Self::read_from(&nuo_host::paths::get().tui_config_file()).unwrap_or_default()
    }

    fn read_from(path: &Path) -> Option<Self> {
        let content = fs::read_to_string(path).ok()?;
        toml::from_str::<TuiConfig>(&content).ok()
    }

    /// Save the configuration to `$XDG_CONFIG_HOME/nuo/tui.toml`.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let path = nuo_host::paths::get().tui_config_file();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let serialized = toml::to_string_pretty(self)?;
        nuo_host::fsutil::atomic_write_bytes(&path, serialized.as_bytes())?;
        Ok(())
    }
}

/// Effective default-expand state for a tool step. An explicit config entry
/// wins; otherwise the declared component default applies; a tool with no
/// declared component (MCP / unknown) stays collapsed.
pub fn tool_default_expanded(config: &TuiConfig, name: &str) -> bool {
    config.default_expanded.get(name).copied().unwrap_or_else(|| {
        crate::tools::component_for(name)
            .map(|component| component.default_expanded())
            .unwrap_or(false)
    })
}

/// Effective default-expand state for a reasoning trace. Defaults to
/// collapsed (`false`) when not configured.
pub fn reasoning_default_expanded(config: &TuiConfig) -> bool {
    config
        .default_expanded
        .get(THINKING_KEY)
        .copied()
        .unwrap_or(false)
}

/// Record a user's disclosure choice for one interactive tool component.
///
/// Every name the component claims is written, so a step persisted under a
/// legacy or alias spelling (`bash`, `write_todos`) honours the same choice as
/// its canonical name. The fan-out is derived from the registry rather than
/// hand-maintained, which is what previously let alias coverage drift
/// (ADR-0020).
pub fn set_component_default_expanded(
    config: &mut TuiConfig,
    component: &crate::tools::ToolComponent,
    expanded: bool,
) {
    for name in component.names() {
        config
            .default_expanded
            .insert(name.to_string(), expanded);
    }
}

/// Discover all candidate theme directories: project-local `.nuo/themes` (and
/// a plain `themes/`) for the workspace and current directory, then the
/// unified user directory `$XDG_CONFIG_HOME/nuo/themes`.
pub fn candidate_theme_dirs(workspace: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. Project-local: `.nuo/themes`, then a plain `themes/` directory.
    let push_project = |base: &Path, dirs: &mut Vec<PathBuf>| {
        for rel in [Path::new(".nuo").join("themes"), PathBuf::from("themes")] {
            let p = base.join(rel);
            if !dirs.contains(&p) {
                dirs.push(p);
            }
        }
    };
    if let Some(ws) = workspace
        && !ws.as_os_str().is_empty()
    {
        push_project(ws, &mut dirs);
    }
    if let Ok(cwd) = std::env::current_dir() {
        push_project(&cwd, &mut dirs);
    }

    // 2. The unified user theme directory: `$XDG_CONFIG_HOME/nuo/themes`.
    let nuo_themes = nuo_host::paths::get().themes_dir();
    if !dirs.contains(&nuo_themes) {
        dirs.push(nuo_themes);
    }

    dirs
}

/// Load custom theme files from a single directory.
pub fn load_theme_files(themes_dir: &Path) -> Vec<nuo_wire::ThemeFile> {
    let mut themes = Vec::new();
    let Ok(entries) = fs::read_dir(themes_dir) else {
        return themes;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("toml")
            && let Ok(content) = fs::read_to_string(&path)
            && let Ok(mut theme) = toml::from_str::<nuo_wire::ThemeFile>(&content)
        {
            if theme.id.is_empty()
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                theme.id = stem.to_string();
            }
            if (theme.name.is_empty() || theme.name == "Custom")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                let title = stem
                    .split(['-', '_'])
                    .filter(|w| !w.is_empty())
                    .map(|w| {
                        let mut chars = w.chars();
                        match chars.next() {
                            None => String::new(),
                            Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                if !title.is_empty() {
                    theme.name = title;
                }
            }
            themes.push(theme);
        }
    }
    themes.sort_by(|a, b| a.name.cmp(&b.name));
    themes
}

/// Load all custom theme files from all candidate theme directories, deduplicating by id.
pub fn load_all_theme_files(workspace: Option<&Path>) -> Vec<nuo_wire::ThemeFile> {
    let mut all_themes = Vec::new();
    for dir in candidate_theme_dirs(workspace) {
        let loaded = load_theme_files(&dir);
        for theme in loaded {
            if !all_themes
                .iter()
                .any(|t: &nuo_wire::ThemeFile| t.id.eq_ignore_ascii_case(&theme.id))
            {
                all_themes.push(theme);
            }
        }
    }
    all_themes.sort_by(|a, b| a.name.cmp(&b.name));
    all_themes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(defaults: &[(&str, bool)]) -> TuiConfig {
        let mut map = HashMap::new();
        for (k, v) in defaults {
            map.insert((*k).to_string(), *v);
        }
        TuiConfig {
            default_expanded: map,
            ..TuiConfig::default()
        }
    }

    /// The declared component policy: `edit_text` / `write_file` (Diffs) and
    /// the shell family (Command) open by default; everything else — including
    /// an undeclared tool — stays collapsed (ADR-0020).
    #[test]
    fn unlisted_tool_falls_back_to_declared_component_default() {
        let cfg = TuiConfig::default();
        assert!(tool_default_expanded(&cfg, "edit_text"));
        assert!(tool_default_expanded(&cfg, "write_file"));
        assert!(tool_default_expanded(&cfg, "execute_command"));
        assert!(!tool_default_expanded(&cfg, "read_text"));
        assert!(!tool_default_expanded(&cfg, "todo"));
        // Undeclared (dynamic / MCP) tools are never open by default.
        assert!(!tool_default_expanded(&cfg, "mcp__foo__bar"));
    }

    #[test]
    fn explicit_override_wins_over_declared_default() {
        let cfg = config(&[
            ("edit_text", false),
            ("execute_command", false),
            ("read_text", true),
        ]);
        assert!(!tool_default_expanded(&cfg, "edit_text"));
        assert!(!tool_default_expanded(&cfg, "execute_command"));
        assert!(tool_default_expanded(&cfg, "read_text"));
        // Still falls back for unlisted tools.
        assert!(!tool_default_expanded(&cfg, "search_text"));
    }

    #[test]
    fn thinking_defaults_collapsed_and_is_overridable() {
        assert!(!reasoning_default_expanded(&TuiConfig::default()));
        let cfg = config(&[(THINKING_KEY, true)]);
        assert!(reasoning_default_expanded(&cfg));
    }

    #[test]
    fn parses_tui_table_from_toml() {
        let toml = r#"
[default_expanded]
edit_text = true
execute_command = true
thinking = true
"#;
        let cfg: TuiConfig = toml::from_str(toml).expect("parses");
        assert!(tool_default_expanded(&cfg, "edit_text"));
        assert!(tool_default_expanded(&cfg, "execute_command"));
        assert!(!tool_default_expanded(&cfg, "read_text"));
        assert!(reasoning_default_expanded(&cfg));
    }

    #[test]
    fn parses_keybindings_table_and_builds_overrides() {
        let toml = r##"
[keybindings]
palette = "ctrl+k"
quit = "ctrl+shift+q"
"##;
        let cfg: TuiConfig = toml::from_str(toml).expect("parses");
        assert_eq!(cfg.keybindings.commands.len(), 2);
        assert!(cfg.keybindings.session.is_empty());
        let o = cfg.global_key_overrides();
        assert_eq!(
            o.effective_binding(crate::keymap::CommandId::CommandPalette),
            Some(crate::keymap::Key::ctrl('k'))
        );
        assert_eq!(
            o.effective_binding(crate::keymap::CommandId::Quit),
            Some(crate::keymap::Key {
                modifiers: crossterm::event::KeyModifiers::CONTROL
                    .union(crossterm::event::KeyModifiers::SHIFT),
                code: crossterm::event::KeyCode::Char('q')
            })
        );
        // Unconfigured commands keep their canonical binding.
        assert_eq!(
            o.effective_binding(crate::keymap::CommandId::OpenTelemetry),
            Some(crate::keymap::Key::CTRL_O)
        );
    }

    #[test]
    fn parses_session_verb_table_into_surface_overrides() {
        let toml = r##"
[keybindings]
palette = "ctrl+k"

[keybindings.session]
open_history = "ctrl+shift+r"
toggle_send_mode = "ctrl+t"
bogus = "ctrl+z"
"##;
        let cfg: TuiConfig = toml::from_str(toml).expect("parses");
        // Global parsing ignores the nested session table and unknown verbs.
        let g = cfg.global_key_overrides();
        assert_eq!(
            g.effective_binding(crate::keymap::CommandId::CommandPalette),
            Some(crate::keymap::Key::ctrl('k'))
        );
        assert_eq!(
            g.effective_binding(crate::keymap::CommandId::Quit),
            Some(crate::keymap::Key::CTRL_C)
        );
        // Surface parsing picks only the session-table verb names.
        let s = cfg.surface_key_overrides();
        assert_eq!(
            s.effective_binding(crate::keymap::SurfaceVerb::OpenHistory),
            crate::keymap::Key::CTRL_SHIFT_R
        );
        assert_eq!(
            s.effective_binding(crate::keymap::SurfaceVerb::ToggleSendMode),
            crate::keymap::Key::ctrl('t')
        );
        // Unconfigured verbs keep canonical; unknown verb names are skipped.
        assert_eq!(
            s.effective_binding(crate::keymap::SurfaceVerb::HistoryPrev),
            crate::keymap::Key::ALT_P
        );
    }

    #[test]
    fn empty_config_yields_defaults() {
        let cfg: TuiConfig = toml::from_str("").expect("empty parses");
        assert!(cfg.default_expanded.is_empty());
        assert!(tool_default_expanded(&cfg, "edit_text"));
        assert!(tool_default_expanded(&cfg, "write_file"));
        assert!(!reasoning_default_expanded(&cfg));
    }

    #[test]
    fn parses_color_scheme_and_custom_palette() {
        let toml = r##"
color_scheme = "custom"

[custom_color_scheme]
background = "#101218"
accent = "#7aa2f7"
"##;
        let cfg: TuiConfig = toml::from_str(toml).expect("parses");
        assert_eq!(cfg.color_scheme, "custom");
        assert_eq!(cfg.custom_color_scheme.background, "#101218");
        assert_eq!(cfg.custom_color_scheme.accent, "#7aa2f7");
        // Missing custom fields inherit their semantic defaults.
        assert_eq!(
            cfg.custom_color_scheme.text,
            nuo_wire::ColorSchemeConfig::default().text
        );
    }

    #[test]
    fn load_theme_files_reads_and_sorts_valid_toml() {
        let temp = tempfile::tempdir().expect("temp dir");
        let themes_dir = temp.path().join("themes");
        std::fs::create_dir_all(&themes_dir).unwrap();

        let theme_a = r##"
name = "Dracula"
description = "Vampire dark palette"
[colors]
background = "#282a36"
surface = "#44475a"
text = "#f8f8f2"
muted = "#6272a4"
accent = "#bd93f9"
success = "#50fa7b"
warning = "#ffb86c"
error = "#ff5555"
"##;

        let theme_b = r##"
name = "Cyberpunk"
description = "Neon high-contrast"
[colors]
background = "#050505"
surface = "#151515"
text = "#ffffff"
muted = "#808080"
accent = "#00ffff"
success = "#00ff00"
warning = "#ffff00"
error = "#ff0055"

[components.input]
bg_active = "#222222"
caret = "#00ffff"
"##;

        std::fs::write(themes_dir.join("dracula.toml"), theme_a).unwrap();
        std::fs::write(themes_dir.join("cyberpunk.toml"), theme_b).unwrap();
        std::fs::write(themes_dir.join("corrupt.toml"), "invalid [== toml").unwrap();
        std::fs::write(themes_dir.join("readme.txt"), "not a theme").unwrap();

        let loaded = load_theme_files(&themes_dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "Cyberpunk");
        assert_eq!(loaded[0].id, "cyberpunk");
        assert_eq!(loaded[1].name, "Dracula");
        assert_eq!(loaded[1].id, "dracula");
        let cyberpunk_components = loaded[0].components.as_ref().unwrap();
        assert_eq!(
            cyberpunk_components
                .input
                .as_ref()
                .unwrap()
                .caret
                .as_deref(),
            Some("#00ffff")
        );
    }

    #[test]
    fn load_all_theme_files_discovers_workspace_themes_and_deduplicates() {
        let temp = tempfile::tempdir().expect("temp dir");
        let ws_dir = temp.path().join("my-project");
        let ws_themes = ws_dir.join(".nuo").join("themes");
        std::fs::create_dir_all(&ws_themes).unwrap();

        let theme_proj = r##"
name = "Solarized Dark"
description = "Precision colors for machines and people"
[colors]
background = "#002b36"
surface = "#073642"
text = "#839496"
muted = "#586e75"
accent = "#268bd2"
success = "#859900"
warning = "#b58900"
error = "#dc322f"
"##;
        std::fs::write(ws_themes.join("solarized-dark.toml"), theme_proj).unwrap();

        let loaded = load_all_theme_files(Some(&ws_dir));
        assert!(
            loaded
                .iter()
                .any(|t| t.id == "solarized-dark" && t.name == "Solarized Dark")
        );
    }
}
