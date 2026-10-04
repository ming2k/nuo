//! Pure-data custom color scheme and theme file schemas shared by persistence and frontends.

use serde::{Deserialize, Serialize};

/// User-editable semantic colors for a custom frontend palette.
///
/// Values use `#RRGGBB`. Frontends validate input before persisting it and
/// fall back to these defaults if an older hand-edited config contains an
/// invalid value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorSchemeConfig {
    pub background: String,
    pub surface: String,
    pub text: String,
    pub muted: String,
    pub accent: String,
    pub success: String,
    pub warning: String,
    pub error: String,
}

impl Default for ColorSchemeConfig {
    fn default() -> Self {
        Self {
            background: "#070808".to_string(),
            surface: "#0e0f0f".to_string(),
            text: "#d5d5cd".to_string(),
            muted: "#777d75".to_string(),
            accent: "#8ea191".to_string(),
            success: "#759475".to_string(),
            warning: "#b5955d".to_string(),
            error: "#be6f68".to_string(),
        }
    }
}

/// Component-specific override for the live prompt / input box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InputThemeConfig {
    pub bg_active: Option<String>,
    pub bg_inactive: Option<String>,
    pub caret: Option<String>,
    pub selection: Option<String>,
    pub placeholder: Option<String>,
}

/// Component-specific override for crate tags and package badges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CrateThemeConfig {
    pub fg: Option<String>,
    pub badge_bg: Option<String>,
}

/// Component-specific override for diff rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DiffThemeConfig {
    pub add_bg: Option<String>,
    pub del_bg: Option<String>,
    pub add_hl: Option<String>,
    pub del_hl: Option<String>,
}

/// Component-specific override for command card rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CommandThemeConfig {
    pub idle_bg: Option<String>,
    pub hover_bg: Option<String>,
}

/// Component-specific override for keyboard shortcut keys and affordance labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct KeycapThemeConfig {
    pub key_fg: Option<String>,
    pub key_bg: Option<String>,
    pub label_fg: Option<String>,
    pub accent_fg: Option<String>,
    pub warn_fg: Option<String>,
}

/// View/canvas surface overrides (Layer 0: Full-screen destinations).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ViewThemeConfig {
    pub canvas: Option<String>,
    pub header_bg: Option<String>,
    pub header_fg: Option<String>,
}

/// Sheet surface overrides (Layer 1: Edge-anchored drawers like Permission).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SheetThemeConfig {
    pub surface: Option<String>,
    pub border: Option<String>,
}

/// Dialog surface overrides (Layer 2: Center-anchored dialogs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DialogThemeConfig {
    pub surface: Option<String>,
    pub border: Option<String>,
    pub backdrop: Option<String>,
    pub dim_factor: Option<f32>,
}

/// Overlay surface overrides (Layer 3: Corner floats, toasts, popups).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OverlayThemeConfig {
    pub toast_bg: Option<String>,
    pub shadow: Option<String>,
}

/// Spatial 4-layer surface theme overrides container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SurfacesThemeConfig {
    pub view: Option<ViewThemeConfig>,
    pub sheet: Option<SheetThemeConfig>,
    #[serde(alias = "modal")]
    pub dialog: Option<DialogThemeConfig>,
    pub overlay: Option<OverlayThemeConfig>,
}

/// Feedback tone container and border colors (Info / Warning / Error / Success).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FeedbackToneConfig {
    pub container: Option<String>,
    pub border: Option<String>,
    pub text: Option<String>,
}

/// Structured feedback notification theme container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FeedbackThemeConfig {
    pub info: Option<FeedbackToneConfig>,
    pub warning: Option<FeedbackToneConfig>,
    pub error: Option<FeedbackToneConfig>,
    pub success: Option<FeedbackToneConfig>,
}

/// Specialized component theme overrides container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ComponentThemesConfig {
    pub input: Option<InputThemeConfig>,
    #[serde(rename = "crate")]
    pub crate_component: Option<CrateThemeConfig>,
    pub diff: Option<DiffThemeConfig>,
    pub command: Option<CommandThemeConfig>,
    pub keycap: Option<KeycapThemeConfig>,
}

/// Full standalone theme file loaded from `$XDG_CONFIG_HOME/mutx/themes/<id>.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeFile {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: Option<String>,
    pub version: Option<String>,
    #[serde(alias = "palette")]
    pub colors: ColorSchemeConfig,
    pub surfaces: Option<SurfacesThemeConfig>,
    pub feedback: Option<FeedbackThemeConfig>,
    pub components: Option<ComponentThemesConfig>,
}

impl Default for ThemeFile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: "Custom".to_string(),
            description: String::new(),
            author: None,
            version: None,
            colors: ColorSchemeConfig::default(),
            surfaces: None,
            feedback: None,
            components: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_theme_file_with_components() {
        let raw = r##"
name = "Tokyo Night"
description = "Clean dark theme celebrating the lights of downtown Tokyo"
author = "folke"
version = "1.0.0"

[colors]
background = "#1a1b26"
surface = "#24283b"
text = "#c0caf5"
muted = "#565f89"
accent = "#7aa2f7"
success = "#9ece6a"
warning = "#e0af68"
error = "#f7768e"

[components.input]
bg_active = "#292e42"
bg_inactive = "#1f2335"
caret = "#c0caf5"

[components.crate]
fg = "#bb9af7"
badge_bg = "#283457"

[components.keycap]
key_fg = "#e2e4dc"
key_bg = "#1c1f1d"
label_fg = "#9ea69b"
"##;
        let parsed: ThemeFile = toml::from_str(raw).expect("theme file should parse");
        assert_eq!(parsed.name, "Tokyo Night");
        assert_eq!(parsed.colors.background, "#1a1b26");
        let components = parsed.components.expect("components should exist");
        let input = components.input.expect("input should exist");
        assert_eq!(input.bg_active.as_deref(), Some("#292e42"));
        assert_eq!(input.caret.as_deref(), Some("#c0caf5"));
        let crate_c = components.crate_component.expect("crate should exist");
        assert_eq!(crate_c.fg.as_deref(), Some("#bb9af7"));
        let keycap = components.keycap.expect("keycap should exist");
        assert_eq!(keycap.key_fg.as_deref(), Some("#e2e4dc"));
        assert_eq!(keycap.key_bg.as_deref(), Some("#1c1f1d"));
        assert_eq!(keycap.label_fg.as_deref(), Some("#9ea69b"));
    }

    #[test]
    fn parses_new_surfaces_and_feedback_theme_file() {
        let raw = r##"
name = "Cyberpunk Obsidian"
description = "Clean modern cyberpunk palette"

[palette]
background = "#090a10"
surface = "#141724"
text = "#e6edf3"
muted = "#7d8590"
accent = "#00f0ff"
success = "#00ff88"
warning = "#ffd700"
error = "#ff0055"

[surfaces.view]
canvas = "#090a10"
header_bg = "#10121d"

[surfaces.sheet]
surface = "#181c2d"
border = "#2d3552"

[surfaces.dialog]
surface = "#141724"
border = "#00f0ff"
dim_factor = 0.55

[feedback.warning]
container = "#26200a"
border = "#ffd700"

[components.input]
caret = "#00f0ff"
"##;
        let parsed: ThemeFile = toml::from_str(raw).expect("new theme schema should parse");
        assert_eq!(parsed.name, "Cyberpunk Obsidian");
        assert_eq!(parsed.colors.background, "#090a10");
        let surfaces = parsed.surfaces.expect("surfaces should exist");
        assert_eq!(
            surfaces.view.and_then(|v| v.canvas).as_deref(),
            Some("#090a10")
        );
        assert_eq!(surfaces.dialog.and_then(|m| m.dim_factor), Some(0.55));
        let feedback = parsed.feedback.expect("feedback should exist");
        assert_eq!(
            feedback.warning.and_then(|w| w.container).as_deref(),
            Some("#26200a")
        );
    }
}
