//! Color palette used across the renderer.

use std::borrow::Cow;
use std::path::Path;

use nuo_wire::{ColorSchemeConfig, ComponentThemesConfig, ThemeFile};
use nuotc::{Color, Modifier, Style};

/// Metadata for one color scheme shown by the Appearance config page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorSchemePreset {
    pub id: Cow<'static, str>,
    pub label: Cow<'static, str>,
    pub description: Cow<'static, str>,
    pub custom: bool,
    pub is_file: bool,
}

impl ColorSchemePreset {
    pub const fn static_preset(
        id: &'static str,
        label: &'static str,
        description: &'static str,
        custom: bool,
    ) -> Self {
        Self {
            id: Cow::Borrowed(id),
            label: Cow::Borrowed(label),
            description: Cow::Borrowed(description),
            custom,
            is_file: false,
        }
    }
}

/// Built-in palettes. Order is the UI order.
pub const COLOR_SCHEMES: [ColorSchemePreset; 7] = [
    ColorSchemePreset::static_preset("zen", "Zen", "Quiet charcoal with sage accents", false),
    ColorSchemePreset::static_preset(
        "midnight",
        "Midnight",
        "Deep navy with crisp blue accents",
        false,
    ),
    ColorSchemePreset::static_preset("nord", "Nord", "Cool arctic blues and soft contrast", false),
    ColorSchemePreset::static_preset(
        "catppuccin",
        "Catppuccin",
        "Warm mocha with lavender accents",
        false,
    ),
    ColorSchemePreset::static_preset(
        "paper",
        "Paper",
        "Warm light surface for bright terminals",
        false,
    ),
    ColorSchemePreset::static_preset(
        "ansi16",
        "ANSI 16",
        "High-contrast 16-color ANSI palette for consoles",
        false,
    ),
    ColorSchemePreset::static_preset(
        "monochrome",
        "Monochrome",
        "High-contrast monochrome palette for DEC VT100 / getty",
        false,
    ),
];

// Default Styling Constants
pub const DEFAULT_CRATE_FG: Color = Color::Rgb(180, 190, 254);
pub const DEFAULT_CARET_FG: Color = Color::Rgb(213, 213, 205);
pub const DEFAULT_INPUT_PLACEHOLDER_FG: Color = Color::Rgb(119, 125, 117);
pub const DEFAULT_KEYCAP_FG: Color = Color::Rgb(226, 228, 220);
pub const DEFAULT_KEYCAP_BG: Color = Color::Rgb(28, 31, 29);
pub const DEFAULT_KEYCAP_LABEL_FG: Color = Color::Rgb(158, 166, 155);
pub const DEFAULT_KEYCAP_ACCENT_FG: Color = Color::Rgb(163, 184, 153);
pub const DEFAULT_KEYCAP_WARN_FG: Color = Color::Rgb(201, 165, 110);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticPalette {
    pub background: Color,
    pub surface: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

/// Styles used during rendering.
#[derive(Clone)]
pub struct Theme {
    pub user_fg: Color,
    pub error_fg: Color,
    pub system_fg: Color,
    pub code_fg: Color,
    pub code_bg: Color,
    pub heading_fg: Color,
    pub quote_fg: Color,
    pub dim_fg: Color,
    pub selected_bg: Color,
    // opencode-style semantic design tokens.
    /// Base background painted across the entire terminal frame so the TUI
    /// owns every pixel rather than relying on the terminal emulator default.
    pub app_bg: Color,
    /// Primary foreground text.
    pub text: Color,
    /// Muted/secondary text.
    pub text_muted: Color,
    /// Intermediate foreground for an interactive step header that is under
    /// the pointer but not in its expanded/active state — sits between
    /// `text_muted` (idle) and `text` (expanded) so hover reads as a softer
    /// affordance than "open".
    pub text_hover: Color,
    /// The transient **interaction** hue (ADR-0174): the color a collapsed
    /// step summary takes while the pointer rests on it or keyboard focus is
    /// on it. This is the *affordance* channel — a hue shift, not a luminance
    /// step — so "look here, this is interactive" never competes with the
    /// disclosure channel's brightness ladder for salience. Derived per
    /// scheme by tinting the muted resting tone toward the accent, keeping
    /// the hue visibly distinct from both `text_muted` and `text`.
    pub affordance_fg: Color,
    /// Solid background for panels (modals, sheets).
    pub panel_bg: Color,
    /// Background for the live input box while it owns the keyboard — the
    /// brightest interactive surface, so "I am typing here" reads from
    /// luminance alone. Input-specific pair token 1 of 2: the input box owns
    /// both values outright (they are deliberately *not* derived from or
    /// shared with the user-message panel tokens), so retuning the input
    /// never ripples into other surfaces and vice versa.
    pub input_bg_active: Color,
    /// Background for the input box while it does **not** own the keyboard
    /// (a transcript step is focused) — a recessed, inert band between
    /// `app_bg` and `panel_bg`. Input-specific pair token 2 of 2; see
    /// [`Theme::input_bg_active`]. The active↔inactive step is large enough
    /// that each state is distinguishable both from the app background and
    /// from the other state.
    pub input_bg_inactive: Color,
    /// Used for sent user messages so they read as read-only compared to the
    /// live input box.
    pub user_panel_bg: Color,
    /// Background for user messages staged in the send queue (waiting for
    /// the in-flight turn to finish). Dimmer than `user_panel_bg` so a
    /// queued message reads as more "pending" than a delivered one without
    /// losing the panel affordance.
    pub user_panel_bg_queued: Color,
    /// Slightly raised background for footer/option bars.
    pub element_bg: Color,
    /// Background for menus / suggestion popups.
    pub menu_bg: Color,
    /// Dim overlay drawn behind modals to fake alpha.
    pub backdrop: Color,
    /// Brightness multiplier (0.0–1.0) applied to every cell of the live
    /// surface while a [`Recess::Dim`](crate::primitives::Recess) modal is open.
    /// The terminal cannot alpha-blend, so a dim-recess modal darkens the
    /// transcript/chrome in place by scaling each color by this factor — lower
    /// is darker. This is the single knob for how strongly an open modal
    /// recedes the background for focus.
    pub modal_dim_factor: f32,
    /// Brand / selection color.
    pub primary: Color,
    pub warning: Color,
    pub success: Color,
    pub info: Color,
    /// Diff banding. Every block-level code/text surface shares one
    /// design contract (see the disclosure module): colors flow through
    /// theme tokens rather than magic `Color::Rgb` literals, so retuning
    /// the palette in one place retunes every block. The diff block is
    /// the reference renderer — it owns the row/highlight pair — and the
    /// flat code blocks (read / bash / listing / matches / markdown) reuse
    /// the same token system via [`code_surface`](Theme::code_surface).
    /// Low-chroma row tint so added/removed blocks read at a glance.
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
    /// Brighter per-word highlight tint layered on top of the row band;
    /// the exact edited word sits on this brighter surface.
    pub diff_add_hl: Color,
    pub diff_del_hl: Color,
    /// Command-card band (ADR-0109). A slash/shell command row is a *card*,
    /// not flat prose: `body_surface` is its idle band, `body_hover` the
    /// band while the row is under the pointer or keyboard focus — the same
    /// element_bg → input_bg_active hover ladder the notice card uses. The
    /// pair is derived (not two more literals) so every scheme gets a
    /// coherent step for free; see [`Theme::command_surface`].
    pub command_band_bg: Color,
    pub command_band_bg_hover: Color,

    // Component-specific tokens
    pub caret_fg: Color,
    pub input_selection_bg: Color,
    pub input_placeholder_fg: Color,
    pub crate_fg: Color,
    pub crate_bg: Color,
    pub keycap_fg: Color,
    pub keycap_bg: Color,
    pub keycap_label_fg: Color,
    pub keycap_accent_fg: Color,
    pub keycap_warn_fg: Color,
    /// Typography and glyph set (ADR-0180).
    pub glyphs: nuotc::GlyphSet,
    /// Visual elevation archetype governing container framing and spatial insets (ADR-0181).
    pub elevation: nuotc::ElevationArchetype,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            user_fg: Color::Rgb(165, 177, 164),
            error_fg: Color::Rgb(190, 111, 104),
            system_fg: Color::Rgb(111, 116, 110),
            code_fg: Color::Rgb(166, 178, 163),
            code_bg: Color::Rgb(17, 19, 18),
            heading_fg: Color::Rgb(190, 194, 181),
            quote_fg: Color::Rgb(156, 145, 118),
            dim_fg: Color::Rgb(94, 99, 94),
            selected_bg: Color::Rgb(38, 48, 44),
            // Zen palette: ink-black base, charcoal surfaces, quiet sage
            // accents. Contrast comes from luminance, not saturated hue, so
            // the interface stays calm while preserving semantic cues.
            app_bg: Color::Rgb(7, 8, 8),
            text: Color::Rgb(213, 213, 205),
            text_muted: Color::Rgb(119, 125, 117),
            text_hover: Color::Rgb(175, 180, 172),
            affordance_fg: Color::Rgb(150, 163, 150),
            panel_bg: Color::Rgb(14, 15, 15),
            input_bg_active: Color::Rgb(26, 28, 27),
            input_bg_inactive: Color::Rgb(16, 17, 17),
            user_panel_bg: Color::Rgb(17, 22, 19),
            user_panel_bg_queued: Color::Rgb(9, 12, 11),
            element_bg: Color::Rgb(21, 23, 22),
            menu_bg: Color::Rgb(17, 19, 18),
            backdrop: Color::Rgb(3, 4, 4),
            modal_dim_factor: 0.5,
            primary: Color::Rgb(142, 161, 145),
            warning: Color::Rgb(181, 149, 93),
            success: Color::Rgb(117, 148, 117),
            info: Color::Rgb(128, 153, 156),
            diff_add_bg: Color::Rgb(18, 31, 22),
            diff_del_bg: Color::Rgb(32, 20, 20),
            diff_add_hl: Color::Rgb(42, 64, 48),
            diff_del_hl: Color::Rgb(64, 40, 40),
            command_band_bg: Color::Rgb(17, 19, 18),
            command_band_bg_hover: Color::Rgb(26, 28, 27),

            caret_fg: DEFAULT_CARET_FG,
            input_selection_bg: Color::Rgb(38, 48, 44),
            input_placeholder_fg: DEFAULT_INPUT_PLACEHOLDER_FG,
            crate_fg: DEFAULT_CRATE_FG,
            crate_bg: Color::Rgb(25, 27, 34),
            keycap_fg: DEFAULT_KEYCAP_FG,
            keycap_bg: DEFAULT_KEYCAP_BG,
            keycap_label_fg: DEFAULT_KEYCAP_LABEL_FG,
            keycap_accent_fg: DEFAULT_KEYCAP_ACCENT_FG,
            keycap_warn_fg: DEFAULT_KEYCAP_WARN_FG,
            glyphs: nuotc::UNICODE_GLYPHS,
            elevation: nuotc::ElevationArchetype::Chromatic,
        }
    }
}

/// Semantic accessors (ADR-0001 P4): renderers reference intent
/// (surface / body / raised / ok / err / …) rather than the raw palette field
/// names, so the palette can be retuned in one place. The fields stay `pub`
/// for `Theme::default()` construction; new rendering code should prefer these.
impl Theme {
    /// High-contrast 16-color ANSI theme for ECMA-48 terminals and Linux virtual console (ADR-0180).
    pub fn ansi16() -> Self {
        Self {
            user_fg: Color::White,
            error_fg: Color::LightRed,
            system_fg: Color::Gray,
            code_fg: Color::White,
            code_bg: Color::Reset,
            heading_fg: Color::LightYellow,
            quote_fg: Color::Yellow,
            dim_fg: Color::DarkGray,
            selected_bg: Color::DarkGray,

            app_bg: Color::Reset,
            text: Color::White,
            text_muted: Color::Gray,
            text_hover: Color::LightYellow,
            affordance_fg: Color::Yellow,
            panel_bg: Color::Reset,
            input_bg_active: Color::Reset,
            input_bg_inactive: Color::Reset,
            user_panel_bg: Color::Reset,
            user_panel_bg_queued: Color::Reset,
            element_bg: Color::Reset,
            menu_bg: Color::Reset,
            backdrop: Color::Reset,
            modal_dim_factor: 1.0,
            primary: Color::Cyan,
            warning: Color::LightYellow,
            success: Color::LightGreen,
            info: Color::LightBlue,
            diff_add_bg: Color::Reset,
            diff_del_bg: Color::Reset,
            diff_add_hl: Color::LightGreen,
            diff_del_hl: Color::LightRed,
            command_band_bg: Color::Reset,
            command_band_bg_hover: Color::DarkGray,

            caret_fg: Color::White,
            input_selection_bg: Color::DarkGray,
            input_placeholder_fg: Color::Gray,
            crate_fg: Color::LightBlue,
            crate_bg: Color::Reset,
            keycap_fg: Color::White,
            keycap_bg: Color::Reset,
            keycap_label_fg: Color::Gray,
            keycap_accent_fg: Color::LightGreen,
            keycap_warn_fg: Color::LightYellow,
            glyphs: nuotc::UNICODE_GLYPHS,
            elevation: nuotc::ElevationArchetype::Hybrid,
        }
    }

    /// High-contrast monochrome theme for DEC VT100, physical serial lines, and getty sessions (ADR-0180).
    /// All colors resolve to [`Color::Reset`] to prevent color escapes, with visual
    /// hierarchy expressed strictly through reverse video, underline, and borders.
    pub fn monochrome() -> Self {
        Self {
            user_fg: Color::Reset,
            error_fg: Color::Reset,
            system_fg: Color::Reset,
            code_fg: Color::Reset,
            code_bg: Color::Reset,
            heading_fg: Color::Reset,
            quote_fg: Color::Reset,
            dim_fg: Color::Reset,
            selected_bg: Color::Reset,

            app_bg: Color::Reset,
            text: Color::Reset,
            text_muted: Color::Reset,
            text_hover: Color::Reset,
            affordance_fg: Color::Reset,
            panel_bg: Color::Reset,
            input_bg_active: Color::Reset,
            input_bg_inactive: Color::Reset,
            user_panel_bg: Color::Reset,
            user_panel_bg_queued: Color::Reset,
            element_bg: Color::Reset,
            menu_bg: Color::Reset,
            backdrop: Color::Reset,
            modal_dim_factor: 1.0,
            primary: Color::Reset,
            warning: Color::Reset,
            success: Color::Reset,
            info: Color::Reset,
            diff_add_bg: Color::Reset,
            diff_del_bg: Color::Reset,
            diff_add_hl: Color::Reset,
            diff_del_hl: Color::Reset,
            command_band_bg: Color::Reset,
            command_band_bg_hover: Color::Reset,

            caret_fg: Color::Reset,
            input_selection_bg: Color::Reset,
            input_placeholder_fg: Color::Reset,
            crate_fg: Color::Reset,
            crate_bg: Color::Reset,
            keycap_fg: Color::Reset,
            keycap_bg: Color::Reset,
            keycap_label_fg: Color::Reset,
            keycap_accent_fg: Color::Reset,
            keycap_warn_fg: Color::Reset,
            glyphs: nuotc::ASCII_GLYPHS,
            elevation: nuotc::ElevationArchetype::Structured,
        }
    }

    /// Resolve theme with adaptive fallback to terminal capability profile when using default settings.
    pub fn resolve_with_profile(
        name: &str,
        custom: &ColorSchemeConfig,
        workspace: Option<&Path>,
        profile: &nuotc::TerminalProfile,
    ) -> Self {
        let trimmed = name.trim().to_ascii_lowercase();
        // If user explicitly chose a scheme other than default/zen, honor their choice.
        let mut theme = if !trimmed.is_empty() && trimmed != "zen" && trimmed != "default" {
            Self::from_color_scheme_with_workspace(name, custom, workspace)
        } else {
            // Adaptive default based on profile
            match profile.color_standard {
                nuotc::ColorStandard::Monochrome => Self::monochrome(),
                nuotc::ColorStandard::Ansi16 => Self::ansi16(),
                nuotc::ColorStandard::Indexed256 | nuotc::ColorStandard::DirectColor => {
                    Self::from_color_scheme_with_workspace(name, custom, workspace)
                }
            }
        };
        theme.glyphs = nuotc::GlyphSet::for_standard(profile.charset_standard);
        theme.elevation = profile.elevation_archetype();
        theme
    }

    /// Whether the active theme is running in DEC VT100 monochrome mode.
    pub fn is_monochrome(&self) -> bool {
        self.text == Color::Reset && self.app_bg == Color::Reset
    }

    /// Whether the active theme is running in 16-color ANSI mode.
    pub fn is_ansi16(&self) -> bool {
        matches!(self.text, Color::White) && matches!(self.text_muted, Color::Gray)
    }

    /// Return all available color schemes: built-ins + custom theme files across workspace and user locations.
    pub fn available_color_schemes() -> Vec<ColorSchemePreset> {
        Self::available_color_schemes_with_workspace(None)
    }

    /// Return all available color schemes given an optional workspace root.
    pub fn available_color_schemes_with_workspace(
        workspace: Option<&Path>,
    ) -> Vec<ColorSchemePreset> {
        let mut list = Vec::new();
        // 1. Built-in presets (Zen, Midnight, Nord, Catppuccin, Paper)
        for preset in &COLOR_SCHEMES {
            list.push(preset.clone());
        }
        // 2. Custom files from workspace, mutx themes_dir, and legacy muta themes_dir
        let files = crate::config::load_all_theme_files(workspace);
        for file in files {
            if !list
                .iter()
                .any(|item| item.id.eq_ignore_ascii_case(&file.id))
                && !file.id.eq_ignore_ascii_case("custom")
            {
                list.push(ColorSchemePreset {
                    id: Cow::Owned(file.id),
                    label: Cow::Owned(file.name),
                    description: Cow::Owned(file.description),
                    custom: false,
                    is_file: true,
                });
            }
        }
        list
    }

    /// Canonicalize a persisted scheme id. Unknown and empty ids use Zen.
    pub fn normalize_color_scheme(name: &str) -> String {
        Self::normalize_color_scheme_with_workspace(name, None)
    }

    pub fn normalize_color_scheme_with_workspace(name: &str, workspace: Option<&Path>) -> String {
        let name = name.trim();
        let schemes = Self::available_color_schemes_with_workspace(workspace);
        schemes
            .iter()
            .find(|scheme| scheme.id.eq_ignore_ascii_case(name))
            .map(|scheme| scheme.id.to_string())
            .unwrap_or_else(|| "zen".to_string())
    }

    pub fn color_scheme_index(name: &str) -> usize {
        Self::color_scheme_index_with_workspace(name, None)
    }

    pub fn color_scheme_index_with_workspace(name: &str, workspace: Option<&Path>) -> usize {
        let name = name.trim();
        let schemes = Self::available_color_schemes_with_workspace(workspace);
        schemes
            .iter()
            .position(|scheme| scheme.id.eq_ignore_ascii_case(name))
            .unwrap_or(0)
    }

    pub fn color_scheme_label(name: &str) -> String {
        Self::color_scheme_label_with_workspace(name, None)
    }

    pub fn color_scheme_label_with_workspace(name: &str, workspace: Option<&Path>) -> String {
        let name = name.trim();
        let schemes = Self::available_color_schemes_with_workspace(workspace);
        schemes
            .iter()
            .find(|scheme| scheme.id.eq_ignore_ascii_case(name))
            .map(|scheme| scheme.label.to_string())
            .unwrap_or_else(|| "Zen".to_string())
    }

    /// Build a complete renderer theme from a preset id, external theme file, or custom semantics.
    pub fn from_color_scheme(name: &str, custom: &ColorSchemeConfig) -> Self {
        Self::from_color_scheme_with_workspace(name, custom, None)
    }

    /// Build a complete renderer theme from a preset id, external theme file, or custom semantics with workspace support.
    pub fn from_color_scheme_with_workspace(
        name: &str,
        _custom: &ColorSchemeConfig,
        workspace: Option<&Path>,
    ) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "midnight" => Self::from_semantic(SemanticPalette {
                background: Color::Rgb(6, 10, 18),
                surface: Color::Rgb(14, 20, 32),
                text: Color::Rgb(218, 225, 240),
                muted: Color::Rgb(112, 124, 148),
                accent: Color::Rgb(91, 156, 255),
                success: Color::Rgb(87, 190, 141),
                warning: Color::Rgb(226, 174, 91),
                error: Color::Rgb(226, 105, 117),
            }),
            "nord" => Self::from_semantic(SemanticPalette {
                background: Color::Rgb(46, 52, 64),
                surface: Color::Rgb(59, 66, 82),
                text: Color::Rgb(236, 239, 244),
                muted: Color::Rgb(136, 148, 166),
                accent: Color::Rgb(136, 192, 208),
                success: Color::Rgb(163, 190, 140),
                warning: Color::Rgb(235, 203, 139),
                error: Color::Rgb(191, 97, 106),
            }),
            "catppuccin" => Self::from_semantic(SemanticPalette {
                background: Color::Rgb(30, 30, 46),
                surface: Color::Rgb(49, 50, 68),
                text: Color::Rgb(205, 214, 244),
                muted: Color::Rgb(147, 153, 178),
                accent: Color::Rgb(203, 166, 247),
                success: Color::Rgb(166, 227, 161),
                warning: Color::Rgb(249, 226, 175),
                error: Color::Rgb(243, 139, 168),
            }),
            "paper" => Self::from_semantic(SemanticPalette {
                background: Color::Rgb(247, 246, 242),
                surface: Color::Rgb(255, 255, 252),
                text: Color::Rgb(43, 45, 48),
                muted: Color::Rgb(106, 110, 114),
                accent: Color::Rgb(55, 100, 165),
                success: Color::Rgb(56, 126, 79),
                warning: Color::Rgb(157, 105, 31),
                error: Color::Rgb(177, 63, 58),
            }),
            "ansi16" => Self::ansi16(),
            "monochrome" | "mono" => Self::monochrome(),
            "custom" => Self::default(),
            "zen" => Self::default(),
            other => {
                let files = crate::config::load_all_theme_files(workspace);
                if let Some(found) = files.into_iter().find(|t| t.id.eq_ignore_ascii_case(other)) {
                    Self::from_theme_file(&found)
                } else {
                    Self::default()
                }
            }
        }
    }

    /// Build a complete renderer theme from a parsed [`ThemeFile`].
    pub fn from_theme_file(file: &ThemeFile) -> Self {
        let mut theme = Self::from_custom(&file.colors);
        theme.apply_surfaces_overrides(&file.surfaces);
        theme.apply_component_overrides(&file.components);
        theme
    }

    /// Apply spatial 4-layer surface overrides onto an existing theme.
    pub fn apply_surfaces_overrides(
        &mut self,
        overrides: &Option<nuo_wire::SurfacesThemeConfig>,
    ) {
        let Some(surfaces) = overrides else { return };
        if let Some(ref view) = surfaces.view {
            if let Some(val) = view.canvas.as_deref().and_then(Self::color_from_hex) {
                self.app_bg = val;
            }
            if let Some(val) = view.header_bg.as_deref().and_then(Self::color_from_hex) {
                self.menu_bg = val;
            }
        }
        if let Some(ref sheet) = surfaces.sheet
            && let Some(val) = sheet.surface.as_deref().and_then(Self::color_from_hex)
        {
            self.element_bg = val;
        }
        if let Some(ref dialog) = surfaces.dialog {
            if let Some(val) = dialog.surface.as_deref().and_then(Self::color_from_hex) {
                self.panel_bg = val;
            }
            if let Some(val) = dialog.backdrop.as_deref().and_then(Self::color_from_hex) {
                self.backdrop = val;
            }
            if let Some(val) = dialog.dim_factor {
                self.modal_dim_factor = val.clamp(0.0, 1.0);
            }
        }
    }

    /// Apply component-specific overrides onto an existing theme.
    pub fn apply_component_overrides(&mut self, overrides: &Option<ComponentThemesConfig>) {
        let Some(components) = overrides else { return };
        if let Some(ref input) = components.input {
            if let Some(val) = input.bg_active.as_deref().and_then(Self::color_from_hex) {
                self.input_bg_active = val;
            }
            if let Some(val) = input.bg_inactive.as_deref().and_then(Self::color_from_hex) {
                self.input_bg_inactive = val;
            }
            if let Some(val) = input.caret.as_deref().and_then(Self::color_from_hex) {
                self.caret_fg = val;
            }
            if let Some(val) = input.selection.as_deref().and_then(Self::color_from_hex) {
                self.input_selection_bg = val;
            }
            if let Some(val) = input.placeholder.as_deref().and_then(Self::color_from_hex) {
                self.input_placeholder_fg = val;
            }
        }
        if let Some(ref crate_c) = components.crate_component {
            if let Some(val) = crate_c.fg.as_deref().and_then(Self::color_from_hex) {
                self.crate_fg = val;
            }
            if let Some(val) = crate_c.badge_bg.as_deref().and_then(Self::color_from_hex) {
                self.crate_bg = val;
            }
        }
        if let Some(ref diff) = components.diff {
            if let Some(val) = diff.add_bg.as_deref().and_then(Self::color_from_hex) {
                self.diff_add_bg = val;
            }
            if let Some(val) = diff.del_bg.as_deref().and_then(Self::color_from_hex) {
                self.diff_del_bg = val;
            }
            if let Some(val) = diff.add_hl.as_deref().and_then(Self::color_from_hex) {
                self.diff_add_hl = val;
            }
            if let Some(val) = diff.del_hl.as_deref().and_then(Self::color_from_hex) {
                self.diff_del_hl = val;
            }
        }
        if let Some(ref command) = components.command {
            if let Some(val) = command.idle_bg.as_deref().and_then(Self::color_from_hex) {
                self.command_band_bg = val;
            }
            if let Some(val) = command.hover_bg.as_deref().and_then(Self::color_from_hex) {
                self.command_band_bg_hover = val;
            }
        }
        if let Some(ref keycap) = components.keycap {
            if let Some(val) = keycap.key_fg.as_deref().and_then(Self::color_from_hex) {
                self.keycap_fg = val;
            }
            if let Some(val) = keycap.key_bg.as_deref().and_then(Self::color_from_hex) {
                self.keycap_bg = val;
            }
            if let Some(val) = keycap.label_fg.as_deref().and_then(Self::color_from_hex) {
                self.keycap_label_fg = val;
            }
            if let Some(val) = keycap.accent_fg.as_deref().and_then(Self::color_from_hex) {
                self.keycap_accent_fg = val;
            }
            if let Some(val) = keycap.warn_fg.as_deref().and_then(Self::color_from_hex) {
                self.keycap_warn_fg = val;
            }
        }
    }

    /// Colors used by the compact scheme preview in `/config`.
    pub fn preview_colors(name: &str, custom: &ColorSchemeConfig) -> [Color; 5] {
        let theme = Self::from_color_scheme(name, custom);
        [
            theme.surface(),
            theme.panel(),
            theme.brand(),
            theme.ok(),
            theme.warn(),
        ]
    }

    pub fn color_from_hex(value: &str) -> Option<Color> {
        let value = value.strip_prefix('#').unwrap_or(value);
        if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let r = u8::from_str_radix(&value[0..2], 16).ok()?;
        let g = u8::from_str_radix(&value[2..4], 16).ok()?;
        let b = u8::from_str_radix(&value[4..6], 16).ok()?;
        Some(Color::Rgb(r, g, b))
    }

    fn from_custom(custom: &ColorSchemeConfig) -> Self {
        let fallback = ColorSchemeConfig::default();
        let color = |value: &str, fallback_value: &str| {
            Self::color_from_hex(value)
                .or_else(|| Self::color_from_hex(fallback_value))
                .unwrap_or(Color::Black)
        };
        Self::from_semantic(SemanticPalette {
            background: color(&custom.background, &fallback.background),
            surface: color(&custom.surface, &fallback.surface),
            text: color(&custom.text, &fallback.text),
            muted: color(&custom.muted, &fallback.muted),
            accent: color(&custom.accent, &fallback.accent),
            success: color(&custom.success, &fallback.success),
            warning: color(&custom.warning, &fallback.warning),
            error: color(&custom.error, &fallback.error),
        })
    }

    fn from_semantic(palette: SemanticPalette) -> Self {
        let SemanticPalette {
            background,
            surface,
            text,
            muted,
            accent,
            success,
            warning,
            error,
        } = palette;
        let light = luminance(background) > 150.0;
        let code_bg = mix(background, text, if light { 0.06 } else { 0.05 });
        let user_bg = mix(surface, accent, if light { 0.08 } else { 0.10 });
        let menu_bg = mix(background, surface, 0.72);
        Self {
            user_fg: mix(text, accent, 0.18),
            error_fg: error,
            system_fg: muted,
            code_fg: mix(text, accent, 0.10),
            code_bg,
            heading_fg: text,
            quote_fg: warning,
            dim_fg: mix(background, muted, if light { 0.82 } else { 0.70 }),
            selected_bg: mix(surface, accent, if light { 0.18 } else { 0.24 }),
            app_bg: background,
            text,
            text_muted: muted,
            text_hover: mix(muted, text, 0.55),
            // The affordance hue tints the muted resting tone toward the
            // accent: same dimness family as `text_hover`, but visibly
            // *tinted*, so the hover/focus cue reads as a hue channel that
            // cannot be confused with the disclosure ladder's luminance rungs.
            affordance_fg: mix(muted, accent, if light { 0.55 } else { 0.62 }),
            panel_bg: surface,
            // The input pair derives from `panel_bg`, keeping the two states
            // related (same hue family, two distinct luminance steps) while
            // staying input-owned: inactive sits at the panel's own level of
            // elevation, active is lifted well clear of every other surface
            // token so the focused box cannot be confused with the ambient
            // chrome (element_bg sits at ~5% toward text; active goes to 8%).
            input_bg_active: mix(surface, text, if light { 0.06 } else { 0.08 }),
            input_bg_inactive: mix(surface, text, if light { 0.015 } else { 0.02 }),
            user_panel_bg: user_bg,
            user_panel_bg_queued: mix(background, user_bg, 0.45),
            element_bg: mix(surface, text, if light { 0.035 } else { 0.05 }),
            menu_bg,
            backdrop: mix(background, Color::Black, if light { 0.45 } else { 0.55 }),
            modal_dim_factor: if light { 0.72 } else { 0.5 },
            primary: accent,
            warning,
            success,
            info: mix(accent, text, 0.12),
            diff_add_bg: mix(background, success, if light { 0.11 } else { 0.15 }),
            diff_del_bg: mix(background, error, if light { 0.10 } else { 0.14 }),
            diff_add_hl: mix(background, success, if light { 0.24 } else { 0.34 }),
            diff_del_hl: mix(background, error, if light { 0.22 } else { 0.32 }),
            // Command card (ADR-0109): derive the pair from the body/raised
            // surfaces so the card inherits each scheme's elevation ladder
            // rather than a fixed literal. Idle is the menu surface lifted a
            // hair off the page; hover is the same scheme-wide active tone
            // (`input_bg_active`) the notice card hovers to, so "an
            // interactive card is lit up" reads identically everywhere.
            command_band_bg: mix(background, surface, 0.72),
            command_band_bg_hover: mix(surface, text, if light { 0.06 } else { 0.08 }),
            caret_fg: text,
            input_selection_bg: mix(surface, accent, if light { 0.18 } else { 0.24 }),
            input_placeholder_fg: muted,
            crate_fg: mix(accent, Color::Rgb(180, 190, 254), 0.65),
            crate_bg: mix(background, accent, if light { 0.08 } else { 0.12 }),
            keycap_fg: mix(text, Color::White, if light { 0.15 } else { 0.22 }),
            keycap_bg: mix(surface, text, if light { 0.06 } else { 0.08 }),
            keycap_label_fg: mix(muted, text, 0.45),
            keycap_accent_fg: mix(accent, text, 0.25),
            keycap_warn_fg: mix(warning, text, 0.15),
            glyphs: nuotc::UNICODE_GLYPHS,
            elevation: nuotc::ElevationArchetype::Chromatic,
        }
    }

    // Surfaces (backgrounds)
    /// Frame background — the base everything sits on.
    pub fn surface(&self) -> Color {
        self.app_bg
    }
    /// Step body / content surface.
    pub fn body(&self) -> Color {
        self.menu_bg
    }
    /// Raised surface (header bands, footer bars).
    pub fn raised(&self) -> Color {
        self.element_bg
    }
    /// Modal / sheet surface.
    pub fn panel(&self) -> Color {
        self.panel_bg
    }
    /// Floating overlay / notification toast surface (lighter than scene head `theme.raised()`).
    pub fn toast_bg(&self) -> Color {
        match self.element_bg {
            Color::Rgb(..) => mix(self.element_bg, Color::Rgb(255, 255, 255), 0.08),
            Color::Reset => Color::Reset,
            other => other,
        }
    }
    /// Live input-box surface while the box owns the keyboard.
    pub fn input_surface(&self) -> Color {
        self.input_bg_active
    }
    /// Live input-box surface while a transcript step owns the keyboard and
    /// the box is inert.
    pub fn input_surface_inactive(&self) -> Color {
        self.input_bg_inactive
    }
    /// Stroke color no longer exists for the composer: the box's identity is
    /// its raised tinted panel again (`input_surface` / `input_surface_inactive`),
    /// so the retired `composer_frame` / `composer_frame_inactive` tokens were
    /// removed rather than left to drift.
    /// Sent-user-message surface.
    pub fn user_surface(&self) -> Color {
        self.user_panel_bg
    }
    /// Surface for a user message staged in the send queue. Dimmer than
    /// [`Theme::user_surface`] so pending reads differently from delivered.
    pub fn user_surface_queued(&self) -> Color {
        self.user_panel_bg_queued
    }
    /// Dim overlay behind modals.
    pub fn backdrop(&self) -> Color {
        self.backdrop
    }
    /// Brightness factor (0.0–1.0) the dim-recess pass scales the live surface
    /// by. Lower is darker. See [`Theme::modal_dim_factor`](struct.Theme.html#structfield.modal_dim_factor).
    pub fn modal_dim_factor(&self) -> f32 {
        self.modal_dim_factor
    }
    /// Selection highlight background.
    pub fn selected(&self) -> Color {
        self.selected_bg
    }

    /// Style for a selectable item/row given its selected state (ADR-0180, ADR-0181).
    /// - Under `Structured` (Monochrome/VT100/Linux VT): applies DEC VT100 `Modifier::REVERSE` so the
    ///   selected/focused item is unmistakably visible without relying on background colors.
    /// - Under `Hybrid`: applies standard 16-color highlight and bold.
    /// - Under `Chromatic`: applies `theme.selected_bg` background tint.
    pub fn selection_style(&self, selected: bool) -> Style {
        if !selected {
            return Style::default();
        }
        match self.elevation {
            nuotc::ElevationArchetype::Structured => {
                Style::default().add_modifier(Modifier::REVERSE)
            }
            nuotc::ElevationArchetype::Hybrid => Style::default()
                .bg(self.selected_bg)
                .fg(self.text)
                .add_modifier(Modifier::BOLD),
            nuotc::ElevationArchetype::Chromatic => Style::default().bg(self.selected_bg),
        }
    }

    /// Style for an interactive element with focus (ADR-0180, ADR-0181).
    pub fn focus_style(&self, focused: bool) -> Style {
        if !focused {
            return Style::default();
        }
        match self.elevation {
            nuotc::ElevationArchetype::Structured => {
                Style::default().add_modifier(Modifier::REVERSE)
            }
            nuotc::ElevationArchetype::Hybrid => {
                Style::default().add_modifier(Modifier::BOLD)
            }
            nuotc::ElevationArchetype::Chromatic => Style::default()
                .fg(self.brand())
                .add_modifier(Modifier::BOLD),
        }
    }

    // Foregrounds
    pub fn fg(&self) -> Color {
        self.text
    }
    pub fn muted(&self) -> Color {
        self.text_muted
    }
    /// Foreground for an interactive step header while collapsed but under the
    /// pointer — an intermediate tone between `muted()` (idle) and `fg()`
    /// (expanded/active), so hover reads as a softer affordance than "open".
    pub fn hover(&self) -> Color {
        self.text_hover
    }
    /// The transient interaction hue (ADR-0174): the collapsed summary's
    /// color while hovered or keyboard-focused. A tinted affordance channel,
    /// distinct from every luminance rung of the disclosure ladder.
    pub fn affordance(&self) -> Color {
        self.affordance_fg
    }
    pub fn dim(&self) -> Color {
        self.dim_fg
    }
    pub fn brand(&self) -> Color {
        self.primary
    }
    pub fn ok(&self) -> Color {
        self.success
    }
    pub fn warn(&self) -> Color {
        self.warning
    }
    pub fn err(&self) -> Color {
        self.error_fg
    }
    pub fn info(&self) -> Color {
        self.info
    }

    /// Resolve semantic syntax token to theme color.
    pub fn syntax_color(&self, kind: crate::syntax::SyntaxKind) -> Color {
        use crate::syntax::SyntaxKind;
        match kind {
            SyntaxKind::Keyword => self.brand(),
            SyntaxKind::Type => self.info(),
            SyntaxKind::Function => self.warn(),
            SyntaxKind::String => self.warn(),
            SyntaxKind::Number => self.ok(),
            SyntaxKind::Comment => self.muted(),
            SyntaxKind::Constant => self.brand(),
            SyntaxKind::Operator => self.info(),
            SyntaxKind::Punctuation => self.muted(),
            SyntaxKind::Plain => self.code_text(),
        }
    }

    // Attachment chips
    // Paste chips (`[Pasted text #N +M lines (size)]`) and image chips
    // (`[Image #N (size)]`) are the composer's identifiers for staged
    // attachments. Each kind gets its own foreground + tinted band so pasted
    // blocks read as distinct typed objects inside the live input — the blue
    // marks "text block", the amber marks "image block". Both derive from
    // existing palette tokens so every color scheme gets a coherent pair.
    /// Foreground of a staged large-text-paste chip. A calm blue reads as
    /// "document / content block" — distinct from plain prose and from the
    /// brand-colored `/command` accent.
    pub fn chip_paste_fg(&self) -> Color {
        self.info
    }
    /// Tinted pill background behind a paste chip. Derived from the surface
    /// the chip sits on (the composer panel) so the band reads on both the
    /// focused and the blurred input box.
    pub fn chip_paste_bg(&self, on: Color) -> Color {
        mix(on, self.info, 0.22)
    }
    /// Foreground of a staged image chip. A warm amber reads as "media
    /// attachment", clearly distinguishable from the text-block blue.
    pub fn chip_image_fg(&self) -> Color {
        self.warning
    }
    /// Tinted pill background behind an image chip (see
    /// [`Theme::chip_paste_bg`]).
    pub fn chip_image_bg(&self, on: Color) -> Color {
        mix(on, self.warning, 0.18)
    }
    pub fn code_text(&self) -> Color {
        self.code_fg
    }
    pub fn code_surface(&self) -> Color {
        self.code_bg
    }
    /// Diff block row band — the low-chroma tint a whole added line sits on.
    /// The reference block-level renderer's colors are first-class tokens so
    /// every block-level surface shares one palette contract.
    pub fn diff_add_bg(&self) -> Color {
        self.diff_add_bg
    }
    /// Diff block row band for a whole removed line.
    pub fn diff_del_bg(&self) -> Color {
        self.diff_del_bg
    }
    /// Diff block per-word highlight on an added line (brighter than the row band).
    pub fn diff_add_hl(&self) -> Color {
        self.diff_add_hl
    }
    /// Diff block per-word highlight on a removed line (brighter than the row band).
    pub fn diff_del_hl(&self) -> Color {
        self.diff_del_hl
    }
    /// Command-card band (ADR-0109): the full-width surface a command row
    /// paints its card on while idle. Distinct from the page background in
    /// every scheme so a command never reads as flat prose.
    pub fn command_surface(&self) -> Color {
        self.command_band_bg
    }
    /// Command-card band while the row is hovered or keyboard-focused —
    /// the shared "interactive card lit up" tone.
    pub fn command_surface_hover(&self) -> Color {
        self.command_band_bg_hover
    }
    pub fn heading(&self) -> Color {
        self.heading_fg
    }
    pub fn quote(&self) -> Color {
        self.quote_fg
    }
    pub fn user_text(&self) -> Color {
        self.user_fg
    }
    pub fn system_text(&self) -> Color {
        self.system_fg
    }
    /// Caret / cursor insertion point color.
    pub fn caret(&self) -> Color {
        self.caret_fg
    }
    /// Input placeholder text color.
    pub fn input_placeholder(&self) -> Color {
        self.input_placeholder_fg
    }
    /// Input selection highlight color.
    pub fn input_selection(&self) -> Color {
        self.input_selection_bg
    }
    /// Crate identifier / tag foreground color.
    pub fn crate_tag(&self) -> Color {
        self.crate_fg
    }
    /// Crate badge background tint.
    pub fn crate_badge(&self) -> Color {
        self.crate_bg
    }
    /// Keyboard keycap glyph foreground (crisp, high-luminance neutral).
    pub fn keycap_fg(&self) -> Color {
        self.keycap_fg
    }
    /// Keyboard keycap badge/pill background (micro-elevated affordance).
    pub fn keycap_bg(&self) -> Color {
        self.keycap_bg
    }
    /// Keyboard affordance action label foreground (readable silver/muted-plus).
    pub fn keycap_label(&self) -> Color {
        self.keycap_label_fg
    }
    /// Keyboard primary action keycap accent (e.g. Enter send).
    pub fn keycap_accent(&self) -> Color {
        self.keycap_accent_fg
    }
    /// Keyboard interrupt / exit / warning keycap color (e.g. Esc Esc, Ctrl+C).
    pub fn keycap_warn(&self) -> Color {
        self.keycap_warn_fg
    }
    /// Standard keycap style (crisp keycap fg + bold).
    pub fn keycap_style(&self) -> nuotc::Style {
        nuotc::Style::default()
            .fg(self.keycap_fg)
            .add_modifier(nuotc::Modifier::BOLD)
    }
    /// Keycap badge/pill style with background lift.
    pub fn keycap_badge_style(&self) -> nuotc::Style {
        nuotc::Style::default()
            .fg(self.keycap_fg)
            .bg(self.keycap_bg)
            .add_modifier(nuotc::Modifier::BOLD)
    }
    /// Keycap affordance action label style.
    pub fn keycap_label_style(&self) -> nuotc::Style {
        nuotc::Style::default().fg(self.keycap_label_fg)
    }

    // 4-Layer Spatial Surfaces Tokens
    pub fn surfaces(&self) -> SurfacesTokens {
        SurfacesTokens {
            view: ViewTokens {
                canvas: self.app_bg,
                header_bg: self.menu_bg,
                header_fg: self.text,
                header_badge: self.primary,
            },
            sheet: SheetTokens {
                surface: self.element_bg,
                border: self.primary,
                accent_bar: self.warning,
            },
            modal: ModalTokens {
                surface: self.panel_bg,
                border: self.primary,
                backdrop: self.backdrop,
                dim_factor: self.modal_dim_factor,
                selected: self.selected_bg,
            },
            overlay: OverlayTokens {
                toast_bg: self.element_bg,
                shadow: Color::Black,
            },
        }
    }

    // Feedback Severity Tokens
    pub fn feedback(&self, severity: nuo_wire::NoticeSeverity) -> FeedbackToneTokens {
        match severity {
            nuo_wire::NoticeSeverity::Info => FeedbackToneTokens {
                container: mix(self.app_bg, self.info, 0.15),
                border: self.info,
                text: self.text,
            },
            nuo_wire::NoticeSeverity::Warning => FeedbackToneTokens {
                container: mix(self.app_bg, self.warning, 0.18),
                border: self.warning,
                text: self.text,
            },
            nuo_wire::NoticeSeverity::Error => FeedbackToneTokens {
                container: mix(self.app_bg, self.error_fg, 0.18),
                border: self.error_fg,
                text: self.text,
            },
        }
    }

    pub fn feedback_success(&self) -> FeedbackToneTokens {
        FeedbackToneTokens {
            container: mix(self.app_bg, self.success, 0.15),
            border: self.success,
            text: self.text,
        }
    }

    // Component Overrides Tokens
    pub fn components(&self) -> ComponentsTokens {
        ComponentsTokens {
            input: InputTokens {
                bg_active: self.input_bg_active,
                bg_inactive: self.input_bg_inactive,
                caret: self.caret_fg,
                selection: self.input_selection_bg,
                placeholder: self.input_placeholder_fg,
            },
            diff: DiffTokens {
                add_bg: self.diff_add_bg,
                del_bg: self.diff_del_bg,
                add_hl: self.diff_add_hl,
                del_hl: self.diff_del_hl,
            },
            command: CommandTokens {
                idle_bg: self.command_band_bg,
                hover_bg: self.command_band_bg_hover,
            },
            crate_tag: self.crate_fg,
            crate_badge: self.crate_bg,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewTokens {
    pub canvas: Color,
    pub header_bg: Color,
    pub header_fg: Color,
    pub header_badge: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SheetTokens {
    pub surface: Color,
    pub border: Color,
    pub accent_bar: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModalTokens {
    pub surface: Color,
    pub border: Color,
    pub backdrop: Color,
    pub dim_factor: f32,
    pub selected: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayTokens {
    pub toast_bg: Color,
    pub shadow: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacesTokens {
    pub view: ViewTokens,
    pub sheet: SheetTokens,
    pub modal: ModalTokens,
    pub overlay: OverlayTokens,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedbackToneTokens {
    pub container: Color,
    pub border: Color,
    pub text: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputTokens {
    pub bg_active: Color,
    pub bg_inactive: Color,
    pub caret: Color,
    pub selection: Color,
    pub placeholder: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffTokens {
    pub add_bg: Color,
    pub del_bg: Color,
    pub add_hl: Color,
    pub del_hl: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandTokens {
    pub idle_bg: Color,
    pub hover_bg: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentsTokens {
    pub input: InputTokens,
    pub diff: DiffTokens,
    pub command: CommandTokens,
    pub crate_tag: Color,
    pub crate_badge: Color,
}

fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        _ => (128, 128, 128),
    }
}

pub fn mix(a: Color, b: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let channel =
        |left: u8, right: u8| (left as f32 + (right as f32 - left as f32) * amount).round() as u8;
    Color::Rgb(channel(ar, br), channel(ag, bg), channel(ab, bb))
}

fn luminance(color: Color) -> f32 {
    let (r, g, b) = rgb(color);
    0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_scheme_falls_back_to_zen() {
        let custom = ColorSchemeConfig::default();
        let fallback = Theme::from_color_scheme("not-a-theme", &custom);
        assert_eq!(fallback.surface(), Theme::default().surface());
        assert_eq!(Theme::normalize_color_scheme(""), "zen");
    }

    #[test]
    fn every_preset_has_a_distinct_canonical_index() {
        let schemes = Theme::available_color_schemes();
        for (index, scheme) in schemes.iter().enumerate() {
            assert_eq!(Theme::color_scheme_index(&scheme.id), index);
        }
    }

    #[test]
    fn discovers_workspace_themes_in_available_color_schemes() {
        let temp = tempfile::tempdir().expect("temp dir");
        let ws = temp.path().join("proj");
        let ws_themes = ws.join(".nuox").join("themes");
        std::fs::create_dir_all(&ws_themes).unwrap();

        let raw = r##"
name = "Monokai Pro"
description = "Monokai pro dark"
[colors]
background = "#2d2a2e"
surface = "#403e41"
text = "#fcfcfa"
muted = "#727072"
accent = "#ffd866"
success = "#a9dc76"
warning = "#fc9867"
error = "#ff6188"
"##;
        std::fs::write(ws_themes.join("monokai-pro.toml"), raw).unwrap();

        let schemes = Theme::available_color_schemes_with_workspace(Some(&ws));
        assert!(schemes.iter().any(|s| s.id == "monokai-pro" && s.is_file));
        let theme = Theme::from_color_scheme_with_workspace(
            "monokai-pro",
            &ColorSchemeConfig::default(),
            Some(&ws),
        );
        assert_eq!(theme.app_bg, Color::Rgb(45, 42, 46));
    }

    #[test]
    fn component_overrides_apply_correctly() {
        let raw = r##"
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
bg_active = "#2a2a2a"
bg_inactive = "#111111"
caret = "#ff00ff"
selection = "#333333"
placeholder = "#777777"

[components.crate]
fg = "#00ffff"
badge_bg = "#1a3333"

[components.diff]
add_bg = "#003300"
del_bg = "#330011"
add_hl = "#006600"
del_hl = "#660022"

[components.command]
idle_bg = "#181818"
hover_bg = "#282828"
"##;
        let file: ThemeFile = toml::from_str(raw).expect("should parse");
        let theme = Theme::from_theme_file(&file);
        assert_eq!(theme.input_surface(), Color::Rgb(42, 42, 42));
        assert_eq!(theme.input_surface_inactive(), Color::Rgb(17, 17, 17));
        assert_eq!(theme.caret(), Color::Rgb(255, 0, 255));
        assert_eq!(theme.input_selection(), Color::Rgb(51, 51, 51));
        assert_eq!(theme.input_placeholder(), Color::Rgb(119, 119, 119));
        assert_eq!(theme.crate_tag(), Color::Rgb(0, 255, 255));
        assert_eq!(theme.crate_badge(), Color::Rgb(26, 51, 51));
        assert_eq!(theme.diff_add_bg(), Color::Rgb(0, 51, 0));
        assert_eq!(theme.diff_del_bg(), Color::Rgb(51, 0, 17));
        assert_eq!(theme.diff_add_hl(), Color::Rgb(0, 102, 0));
        assert_eq!(theme.diff_del_hl(), Color::Rgb(102, 0, 34));
        assert_eq!(theme.command_surface(), Color::Rgb(24, 24, 24));
        assert_eq!(theme.command_surface_hover(), Color::Rgb(40, 40, 40));
    }

    /// The input box owns two dedicated background tokens (independent of the
    /// other surface tokens) and the active/inactive pair must stay
    /// distinguishable in *every* scheme: each state must clear the app
    /// background by a visible margin, and the two states must clear each
    /// other by a visible margin. This is the regression guard for the
    /// "activated and deactivated input look identical to the background"
    /// defect that motivated the pair.
    #[test]
    fn input_surfaces_stay_distinguishable_in_every_scheme() {
        const MIN_STEP: f32 = 4.0;
        let custom = ColorSchemeConfig::default();
        for scheme in &COLOR_SCHEMES {
            if scheme.id == "ansi16" || scheme.id == "monochrome" {
                // ADR-0180: Console/Monochrome profiles anchor backgrounds to Reset
                // and establish distinction via borders/reverse video rather than RGB luminance.
                continue;
            }
            let theme = Theme::from_color_scheme(&scheme.id, &custom);
            let active = theme.input_surface();
            let inactive = theme.input_surface_inactive();
            let base = theme.surface();

            // Light schemes mix toward a dark text (surfaces get *darker* as
            // they elevate), so the margin is measured as an absolute delta.
            let step_from_app_active = (luminance(active) - luminance(base)).abs();
            let step_from_app_inactive = (luminance(inactive) - luminance(base)).abs();
            let step_between_states = (luminance(active) - luminance(inactive)).abs();

            assert!(
                step_from_app_active >= MIN_STEP,
                "{scheme:?}: active input only {step_from_app_active:.1} above app_bg"
            );
            assert!(
                step_from_app_inactive >= MIN_STEP,
                "{scheme:?}: inactive input only {step_from_app_inactive:.1} above app_bg"
            );
            assert!(
                step_between_states >= MIN_STEP,
                "{scheme:?}: active/inactive input only {step_between_states:.1} apart"
            );
            assert_ne!(
                active, inactive,
                "{scheme:?}: active and inactive input must be distinct colors"
            );
        }
    }

    /// ADR-0109: the command card must read as a *card* in every scheme —
    /// both its idle band and its hover band have to clear the page
    /// background by a visible margin (otherwise the row collapses back
    /// into "flat prose", the exact defect the card exists to fix), and the
    /// hover step must itself be visible. Derived for every preset via
    /// `from_semantic`, asserted here so a future scheme cannot silently
    /// flatten it.
    #[test]
    fn command_card_bands_stay_visible_in_every_scheme() {
        const MIN_STEP: f32 = 2.0;
        let custom = ColorSchemeConfig::default();
        for scheme in &COLOR_SCHEMES {
            if scheme.id == "ansi16" || scheme.id == "monochrome" {
                // ADR-0180: Console/Monochrome profiles anchor backgrounds to Reset
                // and establish distinction via borders/reverse video rather than RGB luminance.
                continue;
            }
            let theme = Theme::from_color_scheme(&scheme.id, &custom);
            let idle = theme.command_surface();
            let hover = theme.command_surface_hover();
            let base = theme.surface();

            let idle_step = (luminance(idle) - luminance(base)).abs();
            let hover_step = (luminance(hover) - luminance(base)).abs();
            let hover_idle_step = (luminance(hover) - luminance(idle)).abs();

            assert!(
                idle_step >= MIN_STEP,
                "{scheme:?}: idle command band only {idle_step:.1} from app_bg"
            );
            assert!(
                hover_step >= MIN_STEP,
                "{scheme:?}: hover command band only {hover_step:.1} from app_bg"
            );
            assert!(
                hover_idle_step >= MIN_STEP,
                "{scheme:?}: command hover only {hover_idle_step:.1} above its idle band"
            );
        }
    }

    #[test]
    fn spatial_surfaces_and_feedback_tokens_contract() {
        let theme = Theme::default();
        let surfaces = theme.surfaces();
        assert_eq!(surfaces.view.canvas, theme.surface());
        assert_eq!(surfaces.modal.surface, theme.panel());
        assert_eq!(surfaces.modal.dim_factor, 0.5);

        let fb_warn = theme.feedback(nuo_wire::NoticeSeverity::Warning);
        assert_eq!(fb_warn.border, theme.warn());

        let fb_err = theme.feedback(nuo_wire::NoticeSeverity::Error);
        assert_eq!(fb_err.border, theme.err());

        let fb_info = theme.feedback(nuo_wire::NoticeSeverity::Info);
        assert_eq!(fb_info.border, theme.info());
    }

    #[test]
    fn parses_and_applies_theme_surfaces_overrides() {
        let raw = r##"
name = "Custom Theme"
description = "Test"

[palette]
background = "#050505"
surface = "#101010"
text = "#ffffff"
muted = "#888888"
accent = "#00ffcc"
success = "#00cc00"
warning = "#cccc00"
error = "#cc0000"

[surfaces.view]
canvas = "#010101"
header_bg = "#111111"

[surfaces.modal]
surface = "#222222"
dim_factor = 0.65
"##;
        let file: ThemeFile = toml::from_str(raw).expect("should parse");
        let theme = Theme::from_theme_file(&file);
        assert_eq!(theme.surfaces().view.canvas, Color::Rgb(1, 1, 1));
        assert_eq!(theme.surfaces().view.header_bg, Color::Rgb(17, 17, 17));
        assert_eq!(theme.surfaces().modal.surface, Color::Rgb(34, 34, 34));
        assert_eq!(theme.surfaces().modal.dim_factor, 0.65);
    }

    #[test]
    fn parses_and_applies_theme_keycap_overrides() {
        let raw = r##"
name = "Keycap Test Theme"
description = "Test"

[palette]
background = "#050505"
surface = "#101010"
text = "#ffffff"
muted = "#888888"
accent = "#00ffcc"
success = "#00cc00"
warning = "#cccc00"
error = "#cc0000"

[components.keycap]
key_fg = "#ffffff"
key_bg = "#222222"
label_fg = "#aaaaaa"
accent_fg = "#00ffcc"
warn_fg = "#ff8800"
"##;
        let file: ThemeFile = toml::from_str(raw).expect("should parse");
        let theme = Theme::from_theme_file(&file);
        assert_eq!(theme.keycap_fg(), Color::Rgb(255, 255, 255));
        assert_eq!(theme.keycap_bg(), Color::Rgb(34, 34, 34));
        assert_eq!(theme.keycap_label(), Color::Rgb(170, 170, 170));
        assert_eq!(theme.keycap_accent(), Color::Rgb(0, 255, 204));
        assert_eq!(theme.keycap_warn(), Color::Rgb(255, 136, 0));
    }

    #[test]
    fn test_ansi16_and_monochrome_presets() {
        let ansi = Theme::from_color_scheme("ansi16", &ColorSchemeConfig::default());
        assert_eq!(ansi.app_bg, Color::Reset);
        assert_eq!(ansi.text, Color::White);
        assert_eq!(ansi.error_fg, Color::LightRed);

        let mono = Theme::from_color_scheme("monochrome", &ColorSchemeConfig::default());
        assert_eq!(mono.app_bg, Color::Reset);
        assert_eq!(mono.text, Color::Reset);
        assert_eq!(mono.error_fg, Color::Reset);
    }

    #[test]
    fn test_resolve_with_profile_adapts_defaults() {
        let vt100_prof = nuotc::TerminalProfile::dec_vt100_monochrome();
        let theme =
            Theme::resolve_with_profile("zen", &ColorSchemeConfig::default(), None, &vt100_prof);
        assert_eq!(theme.app_bg, Color::Reset);
        assert_eq!(theme.text, Color::Reset);

        let linux_prof = nuotc::TerminalProfile::ecma48_ansi16();
        let theme_linux =
            Theme::resolve_with_profile("zen", &ColorSchemeConfig::default(), None, &linux_prof);
        assert_eq!(theme_linux.app_bg, Color::Reset);
        assert_eq!(theme_linux.text, Color::White);

        // Explicit non-default choice is preserved
        let theme_nord =
            Theme::resolve_with_profile("nord", &ColorSchemeConfig::default(), None, &vt100_prof);
        assert_ne!(theme_nord.text, Color::Reset);
    }
}
