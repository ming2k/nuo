//! Color palette and design token orchestrator used across the renderer.
use nuotc::{Color, Modifier, Style};
use std::borrow::Cow;

use crate::color_scheme::{
    ColorSchemeConfig, ComponentThemesConfig, NoticeSeverity, SurfacesThemeConfig, ThemeFile,
};
use crate::design::bash_fold_gutter_glyph;
use crate::elevation::ElevationArchetype;

pub mod color_math;
pub mod presets;
pub mod tokens;

#[cfg(test)]
mod tests;

pub use color_math::mix;
pub(crate) use color_math::{desaturate, luminance, min_swatch_distance, rgb_opt};

pub use presets::{
    COLOR_SCHEMES, ColorSchemePreset, DEFAULT_CARET_FG, DEFAULT_CRATE_FG,
    DEFAULT_INPUT_PLACEHOLDER_FG, DEFAULT_KEYCAP_ACCENT_FG, DEFAULT_KEYCAP_BG, DEFAULT_KEYCAP_FG,
    DEFAULT_KEYCAP_LABEL_FG, DEFAULT_KEYCAP_WARN_FG, ListingClass, SemanticPalette,
};
pub(crate) use presets::{LISTING_DIR_HUE, LISTING_EXEC_HUE, LISTING_LINK_HUE, RUNNING_HUE};

pub use tokens::{
    ColorPaletteSpecimen, CommandTokens, ComponentsTokens, DiffTokens, FeedbackToneTokens,
    InputTokens, ModalTokens, OverlayTokens, PaneTokens, SeverityBand, SheetTokens, SurfacesTokens,
    SwatchRole, ViewTokens,
};

/// `Copy` is load-bearing, not an aesthetic choice. The transcript renders with a
/// **lifted** copy of this value while it owns the keyboard (ADR-0145,
/// `Theme::pane_lifted`), so a frame copies it once; `Copy` is what makes that free
/// and — more importantly — it is what keeps it free *in the future*. Every field is a
/// `Copy` scalar, so the constraint costs nothing today, and the moment someone adds a
/// heap-owned field (`String`, `Vec`, a map) the derive stops compiling. That is a
/// build error on the day the cost would be introduced, instead of a performance
/// regression discovered after it lands.
#[derive(Clone, Copy)]
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
    /// The transient **interaction** hue: the color a collapsed
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
    /// Slightly raised background for footer/option bars.
    pub element_bg: Color,
    /// Background for menus / suggestion popups.
    pub menu_bg: Color,
    /// Background for transient notification toast bubbles (Layer 3 overlay).
    pub toast_bg: Color,
    /// Background for the floating which-key scene namespace card (Layer 3 corner float).
    pub which_key_bg: Color,
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
    /// Command-card band. A slash/shell command row is a *card*,
    /// not flat prose: `body_surface` is its idle band, `body_hover` the
    /// band while the row is under the pointer or keyboard focus — the same
    /// element_bg → input_bg_active hover ladder the notice card uses. The
    /// pair is derived (not two more literals) so every scheme gets a
    /// coherent step for free; see [`Theme::command_surface`].
    pub command_band_bg: Color,
    pub command_band_bg_hover: Color,

    /// The **pane wash**: the tone a transcript pane lifts its whole page to
    /// while it owns the keyboard (ADR-0145, amended).
    ///
    /// The composer has always answered "who owns the keyboard?" in the
    /// **background** channel — a bright `input_bg_active` panel against a
    /// recessed `input_bg_inactive` one. The transcript answered the same
    /// question only in the **foreground** channel (a hue tint on the summary
    /// line), which is the weakest cue available: on an expanded step the tint
    /// is suppressed entirely, and on a collapsed one it has to fight the
    /// disclosure and lifecycle hues for the same few characters.
    ///
    /// This is the tone that gives the transcript the same channel: the pane
    /// lifts its whole page to this value while it owns the keyboard, so
    /// `Shift-Tab` swaps one surface state for another — the composer's panel
    /// recedes and the pane the keyboard arrives in comes up.
    ///
    /// It is deliberately a **small** step: it lands on every cell of the
    /// viewport, so the cue is carried by *area*, not by per-cell contrast. The
    /// earlier gutter-rail + full-width header band was the opposite trade (loud
    /// on a handful of cells, absent everywhere else), which read as two stray
    /// edge bars plus a highlighted component rather than as "this pane is
    /// live".
    ///
    /// Whether the value is *usable* is not a matter of taste and is not decided
    /// here: [`Theme::pane_focus()`] derives that from the palette's own
    /// channel headroom (see [`Theme::has_pane_wash`]).
    pub pane_focus_bg: Color,

    /// The **entry marker**: the tone of the single cell the highlighted entry's
    /// header row carries in its reserved gutter (ADR-0145, amended).
    ///
    /// The pane wash above says *the keyboard is in this pane*. This says *and it
    /// is on this entry*.
    ///
    /// The cell renders a quiet vertical accent bar ([`Theme::pane_marker_glyph`])
    /// rather than a kinetic arrow or full-width band. That keeps the cue outside the row's
    /// text: a header row's own tone is the disclosure × lifecycle × interaction composition,
    /// so a marker in the gutter leaves the row's composition intact.
    ///
    /// Deliberately *not* the generic `selected_bg` family: a keyboard-ownership
    /// cue must never read as a selection the user made. See
    /// [`Theme::pane_marker_style`] for the archetype-aware style.
    pub pane_marker_fg: Color,

    /// The **in-flight** hue (ADR-0110 `[INV-TUI-RUNNING-01]`, amended).
    ///
    /// One token, one job: "work is happening on this row right now". It is
    /// derived away from the scheme's *identity* hue ([`Self::info`], which
    /// headers, links, and the info severity band use) because a running step
    /// and a `› turn 3` header both being info-tinted is the ambiguity this
    /// token removes — the header is structure, the running row is liveness, and
    /// they must not be the same color. It is likewise kept clear of
    /// `success` / `warning` / `error`, which are *outcomes*.
    pub running_fg: Color,

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
    /// Typography and glyph set (ADR-0053).
    pub glyphs: nuotc::GlyphSet,
    /// Visual elevation archetype governing container framing and spatial insets (ADR-0053).
    pub elevation: ElevationArchetype,
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
            affordance_fg: Color::Rgb(185, 192, 181),
            panel_bg: Color::Rgb(14, 15, 15),
            input_bg_active: Color::Rgb(26, 28, 27),
            input_bg_inactive: Color::Rgb(16, 17, 17),
            user_panel_bg: Color::Rgb(17, 22, 19),
            element_bg: Color::Rgb(21, 23, 22),
            menu_bg: Color::Rgb(17, 19, 18),
            toast_bg: Color::Rgb(36, 40, 38),
            which_key_bg: Color::Rgb(28, 31, 30),
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
            // Keyboard ownership of the transcript pane is a *page* fact, so it
            // is one small step of the whole page (area carries the cue) plus one
            // neutral marker tone on the pointed-at row (luminance carries the
            // marker, so it can never be mistaken for a semantic hue). The step
            // is deliberately *below* `user_panel_bg` and `element_bg`, so a
            // sent-message panel inside the pane still separates from its page.
            pane_focus_bg: Color::Rgb(13, 14, 14),
            pane_marker_fg: Color::Rgb(142, 161, 145),
            // Liveness owns the one semantic hue nothing else claims: structure
            // is cool (info/brand), outcomes are green/amber/red, and the neutral
            // axis belongs to muted/dim. See `RUNNING_HUE`.
            running_fg: Color::Rgb(56, 222, 235),

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
            elevation: ElevationArchetype::Chromatic,
        }
    }
}

/// Semantic accessors (ADR-0001 P4): renderers reference intent
/// (surface / body / raised / ok / err / …) rather than the raw palette field
/// names, so the palette can be retuned in one place. The fields stay `pub`
/// for `Theme::default()` construction; new rendering code should prefer these.
impl Theme {
    /// High-contrast 16-color ANSI theme for ECMA-48 terminals and Linux virtual console (ADR-0053).
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
            element_bg: Color::Reset,
            menu_bg: Color::Reset,
            toast_bg: Color::Reset,
            which_key_bg: Color::Reset,
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
            // ANSI-16 has no page-wash latitude (`Reset` surfaces are the
            // terminal's own background, which the pane cannot lift), so the
            // transcript announces ownership with the marker alone — the
            // brightest named slot, bold, so it never fuses into a semantic hue.
            pane_focus_bg: Color::Reset,
            pane_marker_fg: Color::White,
            running_fg: Color::LightMagenta,

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
            elevation: ElevationArchetype::Hybrid,
        }
    }

    /// High-contrast monochrome theme for DEC VT100, physical serial lines, and getty sessions (ADR-0053).
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
            element_bg: Color::Reset,
            menu_bg: Color::Reset,
            toast_bg: Color::Reset,
            which_key_bg: Color::Reset,
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
            // No color latitude on a VT100: the pane cannot be lifted, so the
            // entry marker is reverse video ([`Theme::pane_marker_style`])
            // and the stored tones stay `Reset` so nothing emits a color escape.
            pane_focus_bg: Color::Reset,
            pane_marker_fg: Color::Reset,
            running_fg: Color::Reset,

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
            elevation: ElevationArchetype::Structured,
        }
    }

    /// Resolve theme with adaptive fallback to terminal capability profile when using default settings.
    pub fn resolve_with_profile(
        name: &str,
        custom: &ColorSchemeConfig,
        files: &[ThemeFile],
        profile: &nuotc::TerminalProfile,
    ) -> Self {
        let trimmed = name.trim().to_ascii_lowercase();
        // If user explicitly chose a scheme other than default/zen, honor their choice.
        let mut theme = if !trimmed.is_empty() && trimmed != "zen" && trimmed != "default" {
            Self::from_color_scheme_from(name, custom, files)
        } else {
            // Adaptive default based on profile
            match profile.color_standard {
                nuotc::ColorStandard::Monochrome => Self::monochrome(),
                nuotc::ColorStandard::Ansi16 => Self::ansi16(),
                nuotc::ColorStandard::Indexed256 | nuotc::ColorStandard::DirectColor => {
                    Self::from_color_scheme_from(name, custom, files)
                }
            }
        };
        theme.glyphs = nuotc::GlyphSet::for_standard(profile.charset_standard);
        theme.elevation = ElevationArchetype::for_profile(profile);
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

    /// Return all built-in color schemes (no external theme files).
    pub fn available_color_schemes() -> Vec<ColorSchemePreset> {
        Self::available_color_schemes_from(&[])
    }

    /// Return all available color schemes: built-ins plus the supplied external
    /// theme files. Pure projection — the caller loads `files`, this never touches IO.
    pub fn available_color_schemes_from(files: &[ThemeFile]) -> Vec<ColorSchemePreset> {
        let mut list = Vec::new();
        // 1. Built-in presets (Zen, Midnight, Nord, Catppuccin, Paper)
        for preset in &COLOR_SCHEMES {
            list.push(preset.clone());
        }
        // 2. Custom files provided by the caller.
        for file in files {
            if !list
                .iter()
                .any(|item| item.id.eq_ignore_ascii_case(&file.id))
                && !file.id.eq_ignore_ascii_case("custom")
            {
                list.push(ColorSchemePreset {
                    id: Cow::Owned(file.id.clone()),
                    label: Cow::Owned(file.name.clone()),
                    description: Cow::Owned(file.description.clone()),
                    custom: false,
                    is_file: true,
                });
            }
        }
        list
    }

    /// Canonicalize a persisted scheme id. Unknown and empty ids use Zen.
    pub fn normalize_color_scheme(name: &str) -> String {
        Self::normalize_color_scheme_from(name, &[])
    }

    pub fn normalize_color_scheme_from(name: &str, files: &[ThemeFile]) -> String {
        let name = name.trim();
        let schemes = Self::available_color_schemes_from(files);
        schemes
            .iter()
            .find(|scheme| scheme.id.eq_ignore_ascii_case(name))
            .map(|scheme| scheme.id.to_string())
            .unwrap_or_else(|| "zen".to_string())
    }

    pub fn color_scheme_index(name: &str) -> usize {
        Self::color_scheme_index_from(name, &[])
    }

    pub fn color_scheme_index_from(name: &str, files: &[ThemeFile]) -> usize {
        let name = name.trim();
        let schemes = Self::available_color_schemes_from(files);
        schemes
            .iter()
            .position(|scheme| scheme.id.eq_ignore_ascii_case(name))
            .unwrap_or(0)
    }

    pub fn color_scheme_label(name: &str) -> String {
        Self::color_scheme_label_from(name, &[])
    }

    pub fn color_scheme_label_from(name: &str, files: &[ThemeFile]) -> String {
        let name = name.trim();
        let schemes = Self::available_color_schemes_from(files);
        schemes
            .iter()
            .find(|scheme| scheme.id.eq_ignore_ascii_case(name))
            .map(|scheme| scheme.label.to_string())
            .unwrap_or_else(|| "Zen".to_string())
    }

    /// Build a complete renderer theme from a preset id or custom semantics
    /// (built-ins only — no external theme files).
    pub fn from_color_scheme(name: &str, custom: &ColorSchemeConfig) -> Self {
        Self::from_color_scheme_from(name, custom, &[])
    }

    /// Build a complete renderer theme from a preset id, a supplied external
    /// theme file, or custom semantics. Pure projection — no IO.
    pub fn from_color_scheme_from(
        name: &str,
        _custom: &ColorSchemeConfig,
        files: &[ThemeFile],
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
                if let Some(found) = files.iter().find(|t| t.id.eq_ignore_ascii_case(other)) {
                    Self::from_theme_file(found)
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
    pub fn apply_surfaces_overrides(&mut self, overrides: &Option<SurfacesThemeConfig>) {
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
        if let Some(ref overlay) = surfaces.overlay {
            if let Some(val) = overlay.toast_bg.as_deref().and_then(Self::color_from_hex) {
                self.toast_bg = val;
            }
            if let Some(val) = overlay
                .which_key_bg
                .as_deref()
                .and_then(Self::color_from_hex)
            {
                self.which_key_bg = val;
            }
        }
    }

    /// Apply component-specific overrides onto an existing theme.
    pub fn apply_component_overrides(&mut self, overrides: &Option<ComponentThemesConfig>) {
        let Some(components) = overrides else { return };
        if let Some(ref pane) = components.pane {
            if let Some(val) = pane.wash_bg.as_deref().and_then(Self::color_from_hex) {
                self.pane_focus_bg = val;
            }
            if let Some(val) = pane.marker_fg.as_deref().and_then(Self::color_from_hex) {
                self.pane_marker_fg = val;
            }
        }
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
            // The affordance hue tints toward the accent token while guaranteeing a
            // luminous contrast step above resting muted text so hover and focus cues
            // are unmistakably legible across terminal displays.
            affordance_fg: mix(accent, text, if light { 0.45 } else { 0.60 }),
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
            element_bg: mix(surface, text, if light { 0.035 } else { 0.05 }),
            menu_bg,
            toast_bg: mix(surface, text, if light { 0.08 } else { 0.12 }),
            which_key_bg: mix(surface, text, if light { 0.05 } else { 0.08 }),
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
            // Command card: derive the pair from the body/raised
            // surfaces so the card inherits each scheme's elevation ladder
            // rather than a fixed literal. Idle is the menu surface lifted a
            // hair off the page; hover is the same scheme-wide active tone
            // (`input_bg_active`) the notice card hovers to, so "an
            // interactive card is lit up" reads identically everywhere.
            command_band_bg: mix(background, surface, 0.72),
            command_band_bg_hover: mix(surface, text, if light { 0.06 } else { 0.08 }),
            // The pane wash is a deliberately *small* lift of the page toward the
            // scheme's own text pole. It covers the whole viewport, so area
            // carries the cue and per-cell contrast must stay quiet: a step the
            // size of `input_bg_active` would wash out every tone the renderers
            // resolved for the page. Light schemes need a slightly larger step to
            // register at all (dark ink on paper reads softer than the reverse).
            pane_focus_bg: mix(background, text, if light { 0.04 } else { 0.03 }),
            // The entry marker is a neutral luminance rung — never a hue — so it
            // can be mistaken for neither a lifecycle accent nor a text
            // selection. Half-way to the text pole keeps it unmistakably a
            // deliberate mark while still quieter than body text; the chroma is
            // then stripped, because a tinted scheme's text tone would otherwise
            // lend the marker a hue (lavender on catppuccin) and make it read as
            // a state.
            pane_marker_fg: desaturate(
                mix(background, text, if light { 0.55 } else { 0.50 }),
                0.85,
            ),
            // Liveness (ADR-0110 `[INV-TUI-RUNNING-01]`): a saturated teal that is
            // a hue step away from `info` (which owns structure — turn headers,
            // links, the info severity band) and from every outcome tone. Nudged
            // toward the scheme's text so it stays legible on light palettes,
            // exactly as the listing hues are.
            running_fg: mix(RUNNING_HUE, text, if light { 0.20 } else { 0.10 }),
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
            elevation: ElevationArchetype::Chromatic,
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
    /// Tier 1: Client Global TabBar background (ADR-0062).
    /// Subtly recessed base (~0.8% luminance offset in dark mode),
    /// acting as a calm window shell that elevates active tab pills.
    pub fn tabbar_bg(&self) -> Color {
        match (self.app_bg, self.text) {
            (Color::Rgb(..), Color::Rgb(..)) => {
                let light = luminance(self.app_bg) > 150.0;
                mix(self.app_bg, self.text, if light { 0.006 } else { 0.008 })
            }
            _ => self.element_bg,
        }
    }
    /// Tier 2: Polymorphic SceneBar background (ADR-0062).
    /// The primary focal plane of the top chrome (~8.0% luminance offset in dark mode),
    /// drawing immediate optical attention to the active task identity and drill-in stack.
    pub fn scenebar_bg(&self) -> Color {
        match (self.app_bg, self.text) {
            (Color::Rgb(..), Color::Rgb(..)) => {
                let light = luminance(self.app_bg) > 150.0;
                mix(self.app_bg, self.text, if light { 0.055 } else { 0.080 })
            }
            _ => self.element_bg,
        }
    }
    /// Tier 3: Sovereign ThreadContextBar background (ADR-0062).
    /// Subordinate execution boundaries and posture (~4.0% luminance offset in dark mode),
    /// gently transitioning down toward the main content surface.
    pub fn contextbar_bg(&self) -> Color {
        match (self.app_bg, self.text) {
            (Color::Rgb(..), Color::Rgb(..)) => {
                let light = luminance(self.app_bg) > 150.0;
                mix(self.app_bg, self.text, if light { 0.028 } else { 0.040 })
            }
            _ => self.element_bg,
        }
    }
    /// Modal / sheet surface.
    pub fn panel(&self) -> Color {
        self.panel_bg
    }
    /// A **sunken** content surface: one rung *below* the body, so a content
    /// pane reads as distinctly recessed from the chrome above it. Used where a
    /// region must separate from the `raised`/`body` chrome even though both
    /// sit in the same pane (the Settings center's detail body under its head
    /// band). Derived from the scheme's own base and panel tones — never a
    /// fixed literal — so every palette gets a coherent, in-family step that
    /// stays clearly darker than `panel()`.
    pub fn pane_sunken(&self) -> Color {
        match (self.app_bg, self.panel_bg) {
            (Color::Rgb(..), Color::Rgb(..)) => mix(self.app_bg, self.panel_bg, 0.3),
            // Reset-based archetypes (ANSI-16 / monochrome) have no RGB ladder
            // to deepen; keep the surface they already render.
            _ => self.app_bg,
        }
    }
    /// Floating overlay / notification toast surface (Layer 3 overlay).
    pub fn toast_bg(&self) -> Color {
        self.toast_bg
    }
    /// Floating which-key card surface for armed scene namespace (Layer 3 corner float).
    pub fn which_key_bg(&self) -> Color {
        self.which_key_bg
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

    /// Style for a selectable item/row given its selected state (ADR-0053).
    /// - Under `Structured` (Monochrome/VT100/Linux VT): applies DEC VT100 `Modifier::REVERSE` so the
    ///   selected/focused item is unmistakably visible without relying on background colors.
    /// - Under `Hybrid`: applies standard 16-color highlight and bold.
    /// - Under `Chromatic`: applies `theme.selected_bg` background tint.
    pub fn selection_style(&self, selected: bool) -> Style {
        if !selected {
            return Style::default();
        }
        match self.elevation {
            ElevationArchetype::Structured => Style::default().add_modifier(Modifier::REVERSE),
            ElevationArchetype::Hybrid => Style::default()
                .bg(self.selected_bg)
                .fg(self.text)
                .add_modifier(Modifier::BOLD),
            ElevationArchetype::Chromatic => Style::default().bg(self.selected_bg),
        }
    }

    /// The background band for a *hovered* (or keyboard-cursor) selectable row
    /// (ADR-0053). Derived from the active palette — never a fixed
    /// gray literal — so the band is a family member of the scheme it dresses.
    ///
    /// `swatches` are the preview colors the row itself advertises (the
    /// Appearance page's palette chips). The band must *stage* those chips, not
    /// swallow them: if the scheme's own accent tint would land too close to any
    /// swatch, the band flips to the palette's **inverse** pole (a light fill on
    /// a dark scheme, a dark fill on a light one) so every chip still reads
    /// against the highlight instead of fusing into it.
    pub fn row_hover_band(&self, swatches: &[Color]) -> Color {
        match self.elevation {
            // Monochrome has no background latitude: the hover cue is reverse
            // video, applied by the caller (`band_text` keeps the fg as-is).
            ElevationArchetype::Structured => Color::Reset,
            // ANSI-16 owns a named highlight token; use it verbatim rather than
            // mixing it into an RGB truecolor that the terminal cannot render
            // as one of its 16 slots.
            ElevationArchetype::Hybrid => self.selected_bg,
            ElevationArchetype::Chromatic => {
                let light = luminance(self.app_bg) > 150.0;
                // Candidate A: the scheme's own active/selection tint, lifted
                // off the resting surface toward the accent.
                let tinted = self.selected_bg;
                // Candidate B: the inverse pole — the resting surface pushed
                // toward the opposite end of the palette's own text tone.
                let inverse = mix(
                    self.app_bg,
                    if light { Color::Black } else { self.text },
                    0.82,
                );
                // Keep whichever band leaves the most room around the closest
                // swatch, so the chips stay legible; A wins a tie because it is
                // the subtler, more "in-family" highlight.
                if min_swatch_distance(tinted, swatches) >= min_swatch_distance(inverse, swatches) {
                    tinted
                } else {
                    inverse
                }
            }
        }
    }

    /// Pick the foreground a row paints on its hover band: `preferred` (the
    /// row's semantic tone — `fg()`, `brand()`, …) when it still separates from
    /// the band, otherwise the band's polar contrast color so the text never
    /// disappears into its own highlight.
    pub fn band_text(&self, band: Color, preferred: Color) -> Color {
        if band == Color::Reset {
            return preferred;
        }
        if (luminance(band) - luminance(preferred)).abs() >= 60.0 {
            preferred
        } else {
            crate::primitives::contrast_fg(band)
        }
    }

    /// Style for an interactive element with focus (ADR-0053).
    pub fn focus_style(&self, focused: bool) -> Style {
        if !focused {
            return Style::default();
        }
        match self.elevation {
            ElevationArchetype::Structured => Style::default().add_modifier(Modifier::REVERSE),
            ElevationArchetype::Hybrid => Style::default().add_modifier(Modifier::BOLD),
            ElevationArchetype::Chromatic => Style::default()
                .fg(self.brand())
                .add_modifier(Modifier::BOLD),
        }
    }

    /// The **pane wash**: the surface a transcript pane lifts its page to while it
    /// owns the keyboard (ADR-0145, amended).
    ///
    /// This is the transcript's peer to the composer's `input_surface()` /
    /// `input_surface_inactive()` pair — the two planes own the keyboard one at a
    /// time, so they announce it in the *same* channel (background). The difference
    /// is **area**: the composer's panel is a four-row block, the transcript's page
    /// is the whole viewport, so the step is deliberately small. Area carries the
    /// cue; per-cell contrast does not.
    ///
    /// ## Why this is a computation and not just the token
    ///
    /// The declared tone ([`Self::pane_focus_bg`]) is a *wish*. Whether it is
    /// usable is decided by the palette's own **channel headroom**: the pane lift is
    /// additive (every band inside the pane shifts by the same delta, so the
    /// document's elevation ladder survives — see `nuo_view::focus_plane`), and an
    /// additive lift can only preserve a band's elevation while the band has room to
    /// move in every channel. A wash whose delta would clamp the sent-message panel
    /// against 255 does not produce a lifted pane; it produces a flattened one.
    ///
    /// So the effective tone is the declared wish **scaled back** to the largest
    /// step the palette can absorb without clamping any band the transcript builds.
    /// `Reset` on the archetypes that have no page latitude at all, where the pane
    /// cannot be lifted and [`Self::pane_focus_edge_style`] carries the cue instead.
    ///
    /// This is why a hand-authored theme cannot flatten its own transcript by
    /// choosing a careless `pane.wash_bg`: the override moves the wish, and the
    /// headroom clamp decides the outcome.
    pub fn pane_focus(&self) -> Color {
        let Some(delta) = self.pane_focus_delta() else {
            return Color::Reset;
        };
        let Some((r, g, b)) = rgb_opt(self.app_bg) else {
            return Color::Reset;
        };
        let shift = |c: u8, d: i16| (c as i16 + d).clamp(0, 255) as u8;
        Color::Rgb(shift(r, delta[0]), shift(g, delta[1]), shift(b, delta[2]))
    }

    /// The effective per-channel lift of the pane wash, or `None` when this palette
    /// cannot carry a whole-pane cue at all.
    ///
    /// Three gates, each of which is a *measured* property of the palette rather
    /// than a declared archetype:
    ///
    /// 1. The page must have an RGB value to shift (`Reset` cannot be lifted).
    /// 2. The declared wash must be a non-zero step, or the cue is nominal.
    /// 3. The step must survive the bands: it is scaled back to the largest
    ///    magnitude that keeps every background the transcript builds inside the
    ///    channel range. Note that the **sign** is preserved — the lift always moves
    ///    away from the page, so a light scheme (where "raised" is darker) stays
    ///    raised.
    pub fn pane_focus_delta(&self) -> Option<[i16; 3]> {
        let (pr, pg, pb) = rgb_opt(self.app_bg)?;
        let (wr, wg, wb) = rgb_opt(self.pane_focus_bg)?;
        let wish = [
            wr as i16 - pr as i16,
            wg as i16 - pg as i16,
            wb as i16 - pb as i16,
        ];
        if wish == [0, 0, 0] {
            return None;
        }
        let mut scale = 1.0f32;
        for band in self.pane_bands() {
            let Some((br, bg, bb)) = rgb_opt(band) else {
                continue;
            };
            for (page_c, band_c, delta) in [(pr, br, wish[0]), (pg, bg, wish[1]), (pb, bb, wish[2])]
            {
                let _ = page_c;
                if delta == 0 {
                    continue;
                }
                // Headroom in the direction the lift travels. A band on the far side
                // of the page from the lift has no headroom at all — the first band
                // sets the scale to zero — which is correct: shifting it would move
                // it *toward* the page and invert the ladder.
                let room = if delta > 0 {
                    255 - band_c as i16
                } else {
                    band_c as i16
                };
                scale = scale.min(room as f32 / delta.unsigned_abs() as f32);
            }
        }
        if scale <= 0.0 {
            return None;
        }
        let scaled = |d: i16| (d as f32 * scale.min(1.0)).round() as i16;
        let out = [scaled(wish[0]), scaled(wish[1]), scaled(wish[2])];
        (out != [0, 0, 0]).then_some(out)
    }

    /// Every background the transcript pane can build on top of its page.
    ///
    /// This is the set the pane lift has to keep distinguishable, so it is also the
    /// set [`Self::pane_focus_delta`] measures headroom against.
    ///
    /// ## Why this list is a `const` and not a `match`
    ///
    /// A hand-written list of "the surfaces that matter" drifts the moment someone
    /// adds one. The defence is not discipline, it is the **exhaustive
    /// destructure**: every field of `Theme` is named in `Self::background_fields`
    /// exactly once, so adding a field is a compile error until the author says
    /// whether it is a background. Fields marked `false` are foregrounds or
    /// non-surface metadata — the enum arm is where that judgement is recorded, and it
    /// is recorded *once*, next to the field list, instead of in a doc comment nobody
    /// re-reads.
    pub fn pane_bands(&self) -> Vec<Color> {
        self.background_fields()
            .into_iter()
            .filter_map(|(field, value, is_background)| is_background.then_some((field, value)))
            .map(|(_, value)| value)
            .collect()
    }

    /// Classify **every** field of this struct: is it a background the *transcript
    /// pane* can draw?
    ///
    /// The destructure is exhaustive on purpose. Adding a field to `Theme` breaks this
    /// function at compile time, which is the only way to guarantee that a new
    /// background is a deliberate decision about the pane lift rather than an omission
    /// that silently clamps.
    ///
    /// "The pane can draw" is the operative scope, and it is narrower than "is a
    /// background": composer panels, menus, toasts, and the modal backdrop are surfaces
    /// of *other* regions. Counting them here would shrink the pane's own headroom for
    /// no reason, because the lift never reaches them — they are painted from the
    /// unlifted theme, outside the transcript rect.
    ///
    /// `deny(unused_variables)` is what closes the last hole: adding a field to the
    /// destructure without classifying it leaves the binding unused, which is an error
    /// here rather than a warning nobody reads. The array's length is inferred for the
    /// same reason — a hand-maintained count is one more thing to forget.
    #[deny(unused_variables)]
    fn background_fields(&self) -> Vec<(&'static str, Color, bool)> {
        let Self {
            user_fg,
            error_fg,
            system_fg,
            code_fg,
            code_bg,
            heading_fg,
            quote_fg,
            dim_fg,
            selected_bg,
            app_bg,
            text,
            text_muted,
            text_hover,
            affordance_fg,
            panel_bg,
            input_bg_active,
            input_bg_inactive,
            user_panel_bg,
            element_bg,
            menu_bg,
            toast_bg,
            which_key_bg,
            backdrop,
            primary,
            warning,
            success,
            info,
            diff_add_bg,
            diff_del_bg,
            diff_add_hl,
            diff_del_hl,
            command_band_bg,
            command_band_bg_hover,
            pane_focus_bg,
            pane_marker_fg,
            running_fg,
            caret_fg,
            input_selection_bg,
            input_placeholder_fg,
            crate_fg,
            crate_bg,
            keycap_fg,
            keycap_bg,
            keycap_label_fg,
            keycap_accent_fg,
            keycap_warn_fg,
            modal_dim_factor,
            glyphs,
            elevation,
        } = self;
        let _ = (modal_dim_factor, glyphs, elevation);
        [
            ("user_fg", *user_fg, false),
            ("error_fg", *error_fg, false),
            ("system_fg", *system_fg, false),
            ("code_fg", *code_fg, false),
            ("code_bg", *code_bg, true),
            ("heading_fg", *heading_fg, false),
            ("quote_fg", *quote_fg, false),
            ("dim_fg", *dim_fg, false),
            ("selected_bg", *selected_bg, true),
            ("app_bg", *app_bg, true),
            ("text", *text, false),
            ("text_muted", *text_muted, false),
            // A hover *tone* on text, not a surface.
            ("text_hover", *text_hover, false),
            ("affordance_fg", *affordance_fg, false),
            ("panel_bg", *panel_bg, false),
            ("input_bg_active", *input_bg_active, false),
            ("input_bg_inactive", *input_bg_inactive, false),
            ("user_panel_bg", *user_panel_bg, true),
            ("element_bg", *element_bg, true),
            ("menu_bg", *menu_bg, true),
            ("toast_bg", *toast_bg, false),
            ("which_key_bg", *which_key_bg, false),
            ("backdrop", *backdrop, false),
            ("primary", *primary, false),
            ("warning", *warning, false),
            ("success", *success, false),
            ("info", *info, false),
            ("diff_add_bg", *diff_add_bg, true),
            ("diff_del_bg", *diff_del_bg, true),
            // Per-word tints layered *on* a row band: inside the pane, so they must
            // survive the lift.
            ("diff_add_hl", *diff_add_hl, true),
            ("diff_del_hl", *diff_del_hl, true),
            ("command_band_bg", *command_band_bg, true),
            ("command_band_bg_hover", *command_band_bg_hover, true),
            ("pane_focus_bg", *pane_focus_bg, false),
            // The marker is a *mark*, not a surface: it holds a fixed absolute tone on
            // purpose so the cue reads identically whichever pane owns the keyboard. Its
            // requirement is "stays distinguishable from the page", not "keeps its
            // elevation", and `the_entry_marker_stays_distinguishable_from_the_lifted_page`
            // is where that separate property is asserted.
            ("pane_marker_fg", *pane_marker_fg, false),
            ("running_fg", *running_fg, false),
            ("caret_fg", *caret_fg, false),
            ("input_selection_bg", *input_selection_bg, false),
            ("input_placeholder_fg", *input_placeholder_fg, false),
            ("crate_fg", *crate_fg, false),
            ("crate_bg", *crate_bg, false),
            ("keycap_fg", *keycap_fg, false),
            ("keycap_bg", *keycap_bg, false),
            ("keycap_label_fg", *keycap_label_fg, false),
            ("keycap_accent_fg", *keycap_accent_fg, false),
            ("keycap_warn_fg", *keycap_warn_fg, false),
            // Not colors: recorded so the destructure is exhaustive.
            ("modal_dim_factor", Color::Reset, false),
            ("glyphs", Color::Reset, false),
            ("elevation", Color::Reset, false),
        ]
        .to_vec()
    }

    /// This theme with the **pane lift applied to every background**, ready to hand
    /// to a transcript renderer.
    ///
    /// This is the whole-pane cue done at the source instead of afterwards. The
    /// renderers already paint the page tone, the code bands, the cards and the
    /// selection through this struct; lifting the struct means they paint the lifted
    /// pane directly, so the cue costs **one struct copy per frame** rather than a
    /// per-cell restyle over the viewport (measured at ~5.8 ms on a 400x120 pane,
    /// versus ~0 for this).
    ///
    /// Only *backgrounds* move. Foregrounds are semantic — a running hue or a muted
    /// rung means the same thing at either page level — and shifting them would change
    /// the document's meaning rather than its elevation.
    ///
    /// The lift is the same delta [`Self::pane_focus_delta`] computed, so every band
    /// keeps its exact separation from the page: a translation, not a rescale. The
    /// equivalence with a per-cell restyle is asserted by
    /// `pane_lifted_theme_matches_a_cell_wise_restyle`, which is what keeps the field
    /// list below honest — a background that is not lifted there fails that test.
    pub fn pane_lifted(&self) -> Self {
        let Some(delta) = self.pane_focus_delta() else {
            return *self;
        };
        let lift = |c: Color| -> Color {
            match rgb_opt(c) {
                Some((r, g, b)) => {
                    let shift = |c: u8, d: i16| (c as i16 + d).clamp(0, 255) as u8;
                    Color::Rgb(shift(r, delta[0]), shift(g, delta[1]), shift(b, delta[2]))
                }
                None => c,
            }
        };
        // Exhaustive, and deliberately **not** `..*self`. The struct-update form is the
        // hazard: with it a new field is silently carried verbatim, so a background
        // added to the transcript simply stops being lifted and nothing says so.
        // Naming every field makes the omission a compile error and puts the decision
        // (`lift`, or carry verbatim) in the diff where a reviewer sees it.
        //
        // The `lift(..)` set here must equal the `true` set in `background_fields` —
        // that is what the headroom measurement counted, so a field lifted here but not
        // counted there (or vice versa) would either truncate a band or shrink the pane's
        // headroom for nothing. `pane_lifted_lifts_exactly_the_classified_backgrounds`
        // holds the two in agreement.
        Self {
            // Foregrounds and identity tones: verbatim. They are the document's meaning,
            // not its elevation — a running hue means the same thing at either page level.
            user_fg: self.user_fg,
            error_fg: self.error_fg,
            system_fg: self.system_fg,
            code_fg: self.code_fg,
            heading_fg: self.heading_fg,
            quote_fg: self.quote_fg,
            dim_fg: self.dim_fg,
            text: self.text,
            text_muted: self.text_muted,
            text_hover: self.text_hover,
            affordance_fg: self.affordance_fg,
            primary: self.primary,
            warning: self.warning,
            success: self.success,
            info: self.info,
            pane_marker_fg: self.pane_marker_fg,
            running_fg: self.running_fg,
            caret_fg: self.caret_fg,
            input_placeholder_fg: self.input_placeholder_fg,
            crate_fg: self.crate_fg,
            keycap_fg: self.keycap_fg,
            keycap_label_fg: self.keycap_label_fg,
            keycap_accent_fg: self.keycap_accent_fg,
            keycap_warn_fg: self.keycap_warn_fg,
            // Surfaces of *other* regions: verbatim. The composer panel, menus, toasts and
            // the modal backdrop are painted from the unlifted theme, outside the
            // transcript rect, so lifting them would move chrome the cue does not own —
            // and it would charge the pane's headroom for surfaces the lift never reaches.
            panel_bg: self.panel_bg,
            input_bg_active: self.input_bg_active,
            input_bg_inactive: self.input_bg_inactive,
            toast_bg: self.toast_bg,
            which_key_bg: self.which_key_bg,
            backdrop: self.backdrop,
            input_selection_bg: self.input_selection_bg,
            crate_bg: self.crate_bg,
            keycap_bg: self.keycap_bg,
            // Not colors.
            modal_dim_factor: self.modal_dim_factor,
            glyphs: self.glyphs,
            elevation: self.elevation,
            // Backgrounds the transcript pane draws: lifted by the delta.
            app_bg: lift(self.app_bg),
            code_bg: lift(self.code_bg),
            selected_bg: lift(self.selected_bg),
            user_panel_bg: lift(self.user_panel_bg),
            element_bg: lift(self.element_bg),
            menu_bg: lift(self.menu_bg),
            diff_add_bg: lift(self.diff_add_bg),
            diff_del_bg: lift(self.diff_del_bg),
            diff_add_hl: lift(self.diff_add_hl),
            diff_del_hl: lift(self.diff_del_hl),
            command_band_bg: lift(self.command_band_bg),
            command_band_bg_hover: lift(self.command_band_bg_hover),
            // The declared wash tone is the *wish*, not a surface: it is what
            // `pane_focus_delta` measured against, and the lifted theme's page already
            // carries the result. Lifting it again would imply the wish had a second
            // reading, so it stays put like every other non-surface token.
            pane_focus_bg: self.pane_focus_bg,
        }
    }

    /// Whether this palette can carry a **whole-pane** focus wash.
    ///
    /// Derived, never declared: a theme that cannot absorb the step (no RGB page, or
    /// no headroom above its own bands) reports `false` and the transcript uses
    /// [`Self::pane_focus_edge_style`]. That makes "which cue does this profile get"
    /// a property of the palette a user actually loaded, rather than of the
    /// archetype it was classified into — a hand-authored `Chromatic` theme on a
    /// near-white page now gets a correct answer instead of a washed-out one.
    pub fn has_pane_wash(&self) -> bool {
        self.pane_focus_delta().is_some()
    }

    /// The **browse cue** for palettes that cannot lift the page: the pane's gutter
    /// columns in a quiet tone.
    ///
    /// This is the one place the design keeps an edge cue, and the reason is
    /// structural rather than aesthetic. A whole-pane wash needs channel headroom;
    /// a profile without it still needs *some* pane-scoped background change, and the
    /// gutter is the largest region it can afford. (On a `Reset`-page console the
    /// alternative — tinting the whole viewport with a named slot — drowns the
    /// document.)
    ///
    /// `Structured` (VT100 / monochrome) has no color at all, so it degrades to
    /// reverse video — the same vocabulary every other focus and selection cue on
    /// that archetype uses (ADR-0053).
    pub fn pane_focus_edge_style(&self) -> Style {
        match self.elevation {
            ElevationArchetype::Structured => Style::default().add_modifier(Modifier::REVERSE),
            ElevationArchetype::Hybrid => Style::default().bg(self.selected_bg),
            // Reachable on a Chromatic palette whose headroom is gone. It must **not**
            // borrow `pane_marker_fg`: the two answer different questions ("this pane is
            // live" vs "the keyboard is on this entry"), and a shared tone makes a
            // browse-focused pane indistinguishable from an entry-marked one — the exact
            // conflation the marker/band split exists to prevent. The gutter is a surface,
            // so it uses the scheme's raised tone, which is the rung the composer's panel
            // also occupies on this archetype.
            ElevationArchetype::Chromatic => Style::default().bg(self.element_bg),
        }
    }

    /// The style of the **entry marker**: the single cell the pointed-at entry's
    /// header row carries in its reserved gutter (ADR-0145, amended).
    ///
    /// A filled cell rather than a glyph, so it needs no glyph-set entry and cannot
    /// drift between UTF-8 and ASCII terminals. On the `Structured` archetype it
    /// degrades to reverse video, that archetype's only vocabulary (ADR-0053).
    ///
    /// The fill is a **neutral luminance rung** on the color-carrying archetypes, so
    /// it can be mistaken for neither a lifecycle hue nor a text selection — and it
    /// is the same tone the wash-less fallback uses, which is why the two never
    /// disagree about what "the keyboard is here" looks like.
    pub fn pane_marker_style(&self) -> Style {
        match self.elevation {
            ElevationArchetype::Structured => Style::default().add_modifier(Modifier::REVERSE),
            ElevationArchetype::Hybrid | ElevationArchetype::Chromatic => {
                Style::default().fg(self.pane_marker_fg).bg(self.surface())
            }
        }
    }

    /// The glyph used to mark the active transcript entry in the gutter.
    pub fn pane_marker_glyph(&self) -> &'static str {
        if self.glyphs.corner_tl == "+" {
            "|"
        } else {
            "▎"
        }
    }

    /// The **in-flight** hue: the one tone that means "work is happening on this row
    /// right now" (ADR-0110 `[INV-TUI-RUNNING-01]`, amended).
    pub fn running(&self) -> Color {
        self.running_fg
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
    /// The transient interaction hue: the collapsed summary's
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

    /// The severity-band tint pair for a notice card surface (ADR-0139).
    ///
    /// Severity is conveyed by **color alone** on cards: a subtle background
    /// wash derived from the severity accent plus a tone foreground, never a
    /// raw glyph or a bracketed text badge. `accent` is the already
    /// severity-resolved tone (`err()`/`warn()`/`ok()`/`info()`), so the band
    /// needs no second severity enum and retunes with the palette.
    ///
    /// On profiles with no RGB ladder (`app_bg == Reset`, i.e. ANSI-16 /
    /// monochrome) the band falls back to the raised surface with the accent
    /// foreground transposed onto the theme text — the severity survives as a
    /// tone difference without inventing a color the terminal cannot paint.
    ///
    /// `marker` is the color-independent severity token the caller associates
    /// with `accent` (`"error"`, `"warn"`, …); it is emitted **only** when the
    /// theme cannot distinguish severities by color, so a monochrome terminal
    /// keeps a real, textual severity signal after the glyph removal (ADR-0139).
    pub fn severity_band(&self, accent: Color, marker: &'static str) -> SeverityBand {
        let color_distinguishable = self.severity_is_color_distinguishable();
        match self.app_bg {
            Color::Rgb(..) if color_distinguishable => SeverityBand {
                bg: mix(accent, self.app_bg, 0.86),
                fg: mix(accent, self.text, 0.30),
                marker: "",
            },
            _ => SeverityBand {
                bg: self.raised(),
                fg: if color_distinguishable {
                    accent
                } else {
                    self.text
                },
                marker: if color_distinguishable { "" } else { marker },
            },
        }
    }

    /// Whether this theme can distinguish severities by color at all.
    ///
    /// `false` on profiles where every accent collapses to the terminal default
    /// (`Color::Reset`) — monochrome/VT100 and the reset-based ANSI-16 presets.
    /// On such a profile a color-only severity channel carries **no**
    /// information, so the notice surfaces must fall back to a color-independent
    /// marker (ADR-0139). This is the single predicate that decision reads, so
    /// "does color carry severity here?" cannot drift between the dialog, the
    /// transcript head and the toast.
    pub fn severity_is_color_distinguishable(&self) -> bool {
        !matches!(self.err(), Color::Reset)
            || !matches!(self.warn(), Color::Reset)
            || !matches!(self.ok(), Color::Reset)
            || !matches!(self.info(), Color::Reset)
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
    /// The **command prompt**: the `$ ` on a command's invocation row.
    ///
    /// Stated in the brand hue, because it names the *act* ("this ran") rather
    /// than the content, and it is the one cell of the block that must be
    /// findable at a glance when scanning a long transcript. On a monochrome
    /// profile the hue degrades to the content tone; the `$` glyph still says
    /// what it is.
    ///
    /// It is the **only** mark a command body carries per row. A body has no
    /// left track: a column of `│` down 30 rows of stdout reads as a frame, and
    /// — because the terminal's own shift-drag selection cannot know which cells
    /// are chrome — every line the user copied out of the terminal arrived with
    /// a border glued to its front. Structure is carried by the `$`, by the
    /// block's surface, and by indentation; nothing else claims a column
    /// (ADR-0150).
    pub fn command_prompt(&self) -> Color {
        if self.is_monochrome() {
            self.text
        } else {
            self.primary
        }
    }
    /// The folded block's **gutter mark**: the single column of `▎` that fronts
    /// the two rows a fold paints.
    ///
    /// It is a *bounded* mark — two rows, not a track — so it can never bleed
    /// into copied output. Its job is to answer the one question the folded
    /// region raises: *is this program output or is this the tool talking?* A
    /// fold states a count and two verbs, none of which the process printed, so
    /// it is inset from the body's own left edge and tagged with this column;
    /// everything outside it is verbatim stdout.
    ///
    /// The tone is the structural hue (`info`) pulled toward the code surface,
    /// because the fold is structure at content weight. Reset-based archetypes
    /// (ANSI-16 / monochrome) have no RGB ladder to step and fall back to the
    /// plain structural hue — the column still separates the block from the
    /// rows around it, which is the part that matters.
    pub fn command_fold_gutter(&self) -> Color {
        match self.code_bg {
            Color::Rgb(..) => mix(self.info, self.code_bg, 0.30),
            _ => self.info,
        }
    }
    /// The pane marker and the fold gutter are the same mark: both say "this
    /// row is structure, not content". Stating the fold's glyph through the
    /// theme keeps the two from drifting into two vocabularies for one idea.
    pub fn command_fold_gutter_glyph(&self) -> &'static str {
        bash_fold_gutter_glyph(self.glyphs.corner_tl == "+")
    }
    /// Background band of a search-result **file title** row: one subtle step
    /// above the code surface so a file heading reads as a layered header band
    /// over the match rows beneath it (the "title vs content" contrast a search
    /// block otherwise lacks when every row shares one flat surface). Derived
    /// from the code surface — never a literal — and collapsed to that surface
    /// itself on Reset-based archetypes (ANSI-16 / monochrome), which have no
    /// RGB ladder to step.
    pub fn match_title_surface(&self) -> Color {
        match self.code_bg {
            Color::Rgb(..) => mix(self.code_bg, self.text, 0.055),
            _ => self.code_bg,
        }
    }
    /// Background band of a search-result **count row** — the `Found N matches`
    /// summary at the top of a match block. Tinted toward the brand hue so the
    /// summary reads as the block's strongest header, a notch above the neutral
    /// [`Theme::match_title_surface`]. Same Reset-based fallback: an archetype
    /// with no color steps keeps the plain code surface, so the band still
    /// renders (bold + brand foreground) without a background tier it cannot
    /// express.
    pub fn match_count_surface(&self) -> Color {
        match self.code_bg {
            Color::Rgb(..) => mix(self.code_bg, self.primary, 0.16),
            _ => self.code_bg,
        }
    }
    /// Foreground for one [`ListingClass`] in a directory listing, matching the
    /// shell's `ls` convention: **blue** for a directory, **green** for an
    /// executable, **cyan** for a symbolic link, and the scheme's ordinary
    /// content foreground ([`Theme::code_text`]) for a plain file.
    ///
    /// The three `ls` hues are canonical constants, then nudged toward the
    /// scheme's own code tone on a light surface so they stay readable (a pure
    /// blue/green/cyan washes out against a bright background). On a Reset-based
    /// archetype (ANSI-16 / monochrome) the class uses the terminal's own named
    /// slots (ANSI-16) or collapses to the plain content tone (monochrome, which
    /// must emit no hue at all — the type is then carried structurally by the
    /// directory trailing `/`).
    pub fn listing_color(&self, class: ListingClass) -> Color {
        let hue = match class {
            ListingClass::File => return self.code_text(),
            ListingClass::Dir => LISTING_DIR_HUE,
            ListingClass::Exec => LISTING_EXEC_HUE,
            ListingClass::Link => LISTING_LINK_HUE,
        };
        if self.is_monochrome() {
            return self.code_text();
        }
        // A 16-color console has no RGB ladder to blend into; use the named
        // slots that mirror the scheme's own blue / green / cyan tokens so the
        // class survives as one of the terminal's own ANSI colors.
        if self.is_ansi16() {
            return match class {
                ListingClass::Dir => Color::LightBlue,
                ListingClass::Exec => Color::LightGreen,
                ListingClass::Link => Color::LightCyan,
                ListingClass::File => self.code_text(),
            };
        }
        if luminance(self.app_bg) > 150.0 {
            mix(hue, self.code_text(), 0.35)
        } else {
            hue
        }
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
    /// Command-card band: the full-width surface a command row
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
                toast_bg: self.toast_bg,
                which_key_bg: self.which_key_bg,
                shadow: Color::Black,
            },
        }
    }

    // Feedback Severity Tokens
    pub fn feedback(&self, severity: NoticeSeverity) -> FeedbackToneTokens {
        match severity {
            NoticeSeverity::Info => FeedbackToneTokens {
                container: mix(self.app_bg, self.info, 0.15),
                border: self.info,
                text: self.text,
            },
            NoticeSeverity::Warning => FeedbackToneTokens {
                container: mix(self.app_bg, self.warning, 0.18),
                border: self.warning,
                text: self.text,
            },
            NoticeSeverity::Error => FeedbackToneTokens {
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
            pane: PaneTokens {
                wash_bg: self.pane_focus_bg,
                marker_fg: self.pane_marker_fg,
            },
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
