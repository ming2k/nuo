//! TUI presentation configuration and state.
//!
//! Stored in the unified `nuo` namespace at `$XDG_CONFIG_HOME/nuo/terminal.toml`
//! — a sibling of the server's `server.toml`, not a field inside it — so the
//! frontend's presentation state and the server's behavioural policy stay
//! cleanly decoupled while both live under one application directory
//! (ADR-0011, ADR-0031).
use std::fs;
use std::path::{Path, PathBuf};

use ui_primitives::{ColorSchemeConfig, ThemeFile};

use view::tui_config::TuiConfig;
#[cfg(test)]
use view::tui_config::{THINKING_KEY, reasoning_default_expanded, tool_default_expanded};

/// The effective global-chord overrides declared in `[keybindings]`.
/// Empty when the user has not customized any global chord.
pub fn global_key_overrides(config: &TuiConfig) -> crate::shell::keymap::GlobalOverrides {
    crate::shell::keymap::GlobalOverrides::from_config(&config.keybindings.commands)
}

/// The effective surface-verb overrides declared in `[keybindings.session]`.
/// Empty when the user has not customized any full-screen-view chord.
pub fn surface_key_overrides(config: &TuiConfig) -> crate::shell::keymap::SurfaceOverrides {
    crate::shell::keymap::SurfaceOverrides::from_config(&config.keybindings.session)
}

/// Load configuration from `$XDG_CONFIG_HOME/nuo/terminal.toml` (ADR-0031).
pub fn load_tui_config() -> TuiConfig {
    read_tui_config_from(&host::paths::get().terminal_config_file()).unwrap_or_default()
}

fn read_tui_config_from(path: &Path) -> Option<TuiConfig> {
    let content = fs::read_to_string(path).ok()?;
    toml::from_str::<TuiConfig>(&content).ok()
}

/// Save the configuration to `$XDG_CONFIG_HOME/nuo/terminal.toml` (ADR-0031).
pub fn save_tui_config(config: &TuiConfig) -> Result<(), Box<dyn std::error::Error>> {
    let path = host::paths::get().terminal_config_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let serialized = toml::to_string_pretty(config)?;
    host::fsutil::atomic_write_bytes(&path, serialized.as_bytes())?;
    Ok(())
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
    let nuo_themes = host::paths::get().themes_dir();
    if !dirs.contains(&nuo_themes) {
        dirs.push(nuo_themes);
    }

    dirs
}

/// Load custom theme files from a single directory.
pub fn load_theme_files(themes_dir: &Path) -> Vec<ThemeFile> {
    let mut themes = Vec::new();
    let Ok(entries) = fs::read_dir(themes_dir) else {
        return themes;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("toml")
            && let Ok(content) = fs::read_to_string(&path)
            && let Ok(mut theme) = toml::from_str::<ThemeFile>(&content)
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
pub fn load_all_theme_files(workspace: Option<&Path>) -> Vec<ThemeFile> {
    let mut all_themes = Vec::new();
    for dir in candidate_theme_dirs(workspace) {
        let loaded = load_theme_files(&dir);
        for theme in loaded {
            if !all_themes
                .iter()
                .any(|t: &ThemeFile| t.id.eq_ignore_ascii_case(&theme.id))
            {
                all_themes.push(theme);
            }
        }
    }
    all_themes.sort_by(|a, b| a.name.cmp(&b.name));
    all_themes
}

/// Available color schemes for a workspace: load theme files (IO), then project
/// them purely via `Theme::available_color_schemes_from`.
pub fn available_color_schemes(
    workspace: Option<&Path>,
) -> Vec<ui_primitives::theme::ColorSchemePreset> {
    ui_primitives::theme::Theme::available_color_schemes_from(&load_all_theme_files(workspace))
}

/// Canonicalize a persisted scheme id against the workspace's theme files.
pub fn normalize_color_scheme(name: &str, workspace: Option<&Path>) -> String {
    ui_primitives::theme::Theme::normalize_color_scheme_from(name, &load_all_theme_files(workspace))
}

/// Index of a scheme id within the workspace's available schemes.
pub fn color_scheme_index(name: &str, workspace: Option<&Path>) -> usize {
    ui_primitives::theme::Theme::color_scheme_index_from(name, &load_all_theme_files(workspace))
}

/// Human label for a scheme id within the workspace's available schemes.
pub fn color_scheme_label(name: &str, workspace: Option<&Path>) -> String {
    ui_primitives::theme::Theme::color_scheme_label_from(name, &load_all_theme_files(workspace))
}

/// Build a renderer theme for a scheme id with workspace theme files.
pub fn theme_from_color_scheme(
    name: &str,
    custom: &ColorSchemeConfig,
    workspace: Option<&Path>,
) -> ui_primitives::theme::Theme {
    ui_primitives::theme::Theme::from_color_scheme_from(
        name,
        custom,
        &load_all_theme_files(workspace),
    )
}

/// Resolve a theme with adaptive profile fallback and workspace theme files.
pub fn resolve_theme_with_profile(
    name: &str,
    custom: &ColorSchemeConfig,
    workspace: Option<&Path>,
    profile: &graticule::TerminalProfile,
) -> ui_primitives::theme::Theme {
    ui_primitives::theme::Theme::resolve_with_profile(
        name,
        custom,
        &load_all_theme_files(workspace),
        profile,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::keymap::{KeyCode, KeyModifiers};
    use std::collections::HashMap;

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
    /// `todo` open by default; everything else — including the shell family
    /// (Command) and undeclared tools — stays collapsed (ADR-0020).
    #[test]
    fn unlisted_tool_falls_back_to_declared_component_default() {
        let cfg = TuiConfig::default();
        assert!(tool_default_expanded(&cfg, "edit_text"));
        assert!(tool_default_expanded(&cfg, "write_file"));
        assert!(!tool_default_expanded(&cfg, "execute_command"));
        assert!(tool_default_expanded(&cfg, "todo"));
        assert!(tool_default_expanded(&cfg, "write_todos"));
        assert!(!tool_default_expanded(&cfg, "read"));
        // Undeclared (dynamic / MCP) tools are never open by default.
        assert!(!tool_default_expanded(&cfg, "mcp__foo__bar"));
    }

    #[test]
    fn explicit_override_wins_over_declared_default() {
        let cfg = config(&[
            ("edit_text", false),
            ("execute_command", true),
            ("read", true),
        ]);
        assert!(!tool_default_expanded(&cfg, "edit_text"));
        assert!(tool_default_expanded(&cfg, "execute_command"));
        assert!(tool_default_expanded(&cfg, "read"));
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
        assert!(!tool_default_expanded(&cfg, "read"));
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
        let o = global_key_overrides(&cfg);
        assert_eq!(
            o.effective_binding(crate::shell::keymap::CommandId::CommandPalette),
            Some(crate::shell::keymap::Key::ctrl('k'))
        );
        assert_eq!(
            o.effective_binding(crate::shell::keymap::CommandId::Quit),
            Some(crate::shell::keymap::Key {
                modifiers: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
                code: KeyCode::Char('q')
            })
        );
        // Unconfigured commands keep their canonical binding.
        assert_eq!(
            o.effective_binding(crate::shell::keymap::CommandId::OpenQueue),
            Some(crate::shell::keymap::Key::CTRL_Q)
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
        let g = global_key_overrides(&cfg);
        assert_eq!(
            g.effective_binding(crate::shell::keymap::CommandId::CommandPalette),
            Some(crate::shell::keymap::Key::ctrl('k'))
        );
        assert_eq!(
            g.effective_binding(crate::shell::keymap::CommandId::Quit),
            Some(crate::shell::keymap::Key::CTRL_C)
        );
        // Surface parsing picks only the session-table verb names.
        let s = surface_key_overrides(&cfg);
        assert_eq!(
            s.effective_binding(crate::shell::keymap::SurfaceVerb::OpenHistory),
            crate::shell::keymap::Key::CTRL_SHIFT_R
        );
        assert_eq!(
            s.effective_binding(crate::shell::keymap::SurfaceVerb::ToggleSendMode),
            crate::shell::keymap::Key::ctrl('t')
        );
        // Unconfigured verbs keep canonical; unknown verb names are skipped.
        assert_eq!(
            s.effective_binding(crate::shell::keymap::SurfaceVerb::HistoryPrev),
            crate::shell::keymap::Key::ALT_P
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
            ColorSchemeConfig::default().text
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

    #[test]
    fn empty_default_expanded_is_not_serialized() {
        let cfg = TuiConfig::default();
        let serialized = toml::to_string_pretty(&cfg).expect("serializes");
        assert!(
            !serialized.contains("[default_expanded]"),
            "empty default_expanded must not be serialized, got:\n{serialized}"
        );
        assert!(
            !serialized.contains("[keybindings]"),
            "empty keybindings must not be serialized, got:\n{serialized}"
        );
    }

    #[test]
    fn set_component_default_expanded_prunes_when_matching_default() {
        let mut cfg = TuiConfig::default();
        let comp = view::tools::component_for("execute_command").expect("command component");
        assert!(!comp.default_expanded());

        // Overriding to true inserts into map
        view::tui_config::set_component_default_expanded(&mut cfg, comp, true);
        assert_eq!(cfg.default_expanded.get("execute_command"), Some(&true));

        // Reverting to false (declared default) removes from map
        view::tui_config::set_component_default_expanded(&mut cfg, comp, false);
        assert!(!cfg.default_expanded.contains_key("execute_command"));
        assert!(cfg.default_expanded.is_empty());

        // And tool_default_expanded still resolves to false via component fallback
        assert!(!tool_default_expanded(&cfg, "execute_command"));
    }
}
