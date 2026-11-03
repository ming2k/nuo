//! Color palette used across the renderer.
use std::borrow::Cow;

use nuotc::{Color, Modifier, Style};

use crate::color_scheme::{
    ColorSchemeConfig, ComponentThemesConfig, NoticeSeverity, SurfacesThemeConfig, ThemeFile,
};
use crate::elevation::ElevationArchetype;

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

/// The `ls(1)`-style class of a filesystem entry, used to colour a directory
/// listing the way the shell's `ls` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListingClass {
    /// A directory — `ls` renders it blue.
    Dir,
    /// An executable file — `ls` renders it green.
    Exec,
    /// A symbolic link — `ls` renders it cyan.
    Link,
    /// Any other file — rendered in the scheme's normal content foreground.
    File,
}

/// Canonical `ls(1)` listing hues: the blue / green / cyan a shell listing marks
/// a directory, an executable, and a symbolic link with. Tuned for a dark
/// surface; [`Theme::listing_color`] nudges each toward the scheme's own content
/// tone so it stays legible on light palettes too.
const LISTING_DIR_HUE: Color = Color::Rgb(86, 132, 214);
const LISTING_EXEC_HUE: Color = Color::Rgb(86, 176, 106);
const LISTING_LINK_HUE: Color = Color::Rgb(74, 172, 194);

/// The **in-flight** hue: the one saturated tone that means "work is happening
/// on this row right now" (ADR-0110 `[INV-TUI-RUNNING-01]`).
///
/// A vivid cyan, and a constant for the same reason the listing hues are: it is a
/// *role*, not a palette member.
///
/// The hue is a check, not a taste call. Structure in this product is a **muted,
/// desaturated cool** (`info` / `brand`: turn headers, `round N`, links, the info
/// band), outcomes own green / amber / red, and the neutral axis belongs to
/// `muted` / `dim`. Liveness is the same cool family pushed all the way to
/// saturation — which is exactly what makes it read as *live* rather than as
/// *structural*, and why the two cannot be confused even though they share a
/// neighborhood of the wheel: a running row is the only vivid thing on the page.
/// Reusing `info` itself, as the earlier code did, made the running row and the
/// `› turn N` header above it literally the same color.
///
/// Tinted toward each scheme's own text pole by [`Theme::running`]'s derivation
/// so it stays legible on light palettes.
const RUNNING_HUE: Color = Color::Rgb(56, 222, 235);

/// Styles used during rendering.
///
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
    /// The cell renders a quiet vertical accent bar ([`crate::design::PANE_MARKER_GLYPH`])
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
    /// destructure**: every field of `Theme` is named in [`Self::background_fields`]
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
    pub which_key_bg: Color,
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

/// The severity-band tint pair for notice card surfaces (ADR-0139).
///
/// Produced by [`Theme::severity_band`]; consumed by the notice-center dialog
/// (topic pill + selection rail) and the transcript notice head (left rail).
///
/// When the theme cannot distinguish severities by color
/// ([`Theme::severity_is_color_distinguishable`] is `false` — monochrome/VT100),
/// the band's `marker` carries a **color-independent** severity token so the
/// signal survives where the physical color does not. On a color-capable theme
/// `marker` is empty and severity rides the tint alone. This is what keeps the
/// glyph removal from being a regression on the very profile where the deleted
/// `!`/`▲`/`ℹ` was the only working marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeverityBand {
    /// The background wash: the severity accent mixed toward the app
    /// background (subtle), or the raised surface on ANSI-16/monochrome.
    pub bg: Color,
    /// The on-band foreground: the accent pulled toward the text tone so the
    /// topic label stays readable on `bg`.
    pub fg: Color,
    /// A color-independent severity token (`""` on a color-capable theme;
    /// `"error"`/`"warn"`/`"ok"`/`"info"` when color cannot carry it). ASCII, so
    /// it needs no glyph-set fallback.
    pub marker: &'static str,
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

/// The transcript pane's keyboard-ownership pair (ADR-0145).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneTokens {
    pub wash_bg: Color,
    pub marker_fg: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentsTokens {
    pub input: InputTokens,
    pub pane: PaneTokens,
    pub diff: DiffTokens,
    pub command: CommandTokens,
    pub crate_tag: Color,
    pub crate_badge: Color,
}

/// A named design token swatch with label and contrast data (ADR-0147).
#[derive(Debug, Clone, PartialEq)]
pub struct SwatchRole {
    pub name: &'static str,
    pub description: &'static str,
    pub color: Color,
    pub contrast_ratio: f32,
}

/// A structured palette specimen containing the complete design system color tokens (ADR-0147).
#[derive(Debug, Clone, PartialEq)]
pub struct ColorPaletteSpecimen {
    pub theme_id: String,
    pub is_dark: bool,
    pub surfaces: Vec<SwatchRole>,
    pub text_ladder: Vec<SwatchRole>,
    pub signals: Vec<SwatchRole>,
    pub components: Vec<SwatchRole>,
}

impl Theme {
    /// Generate a structured design system color palette specimen (ADR-0147 `[INV-TUI-COLOR-02]`).
    pub fn palette_specimen(&self, theme_id: impl Into<String>) -> ColorPaletteSpecimen {
        let bg_c = self.surface();
        let bg_lum = luminance(bg_c);
        let is_dark = bg_lum < 150.0;

        let make_swatch = |name: &'static str, desc: &'static str, color: Color| -> SwatchRole {
            let col_lum = luminance(color);
            let (l1, l2) = if col_lum >= bg_lum {
                (col_lum, bg_lum)
            } else {
                (bg_lum, col_lum)
            };
            let ratio = (l1 + 5.0) / (l2 + 5.0);
            SwatchRole {
                name,
                description: desc,
                color,
                contrast_ratio: ratio,
            }
        };

        ColorPaletteSpecimen {
            theme_id: theme_id.into(),
            is_dark,
            surfaces: vec![
                make_swatch(
                    "Surface",
                    "Base application viewport background",
                    self.surface(),
                ),
                make_swatch(
                    "Panel",
                    "Recessed sidebars, docks and input panels",
                    self.panel(),
                ),
                make_swatch("Raised", "Elevated header bars and tabs", self.raised()),
                make_swatch(
                    "Sunken",
                    "Detail content and recessed card backgrounds",
                    self.pane_sunken(),
                ),
                make_swatch(
                    "Code",
                    "Flat code block and transcript diff background",
                    self.code_surface(),
                ),
            ],
            text_ladder: vec![
                make_swatch("Text", "High-contrast reading foreground", self.fg()),
                make_swatch(
                    "Muted",
                    "Timestamps, secondary metadata, and hints",
                    self.muted(),
                ),
                make_swatch("Dim", "Inert, disabled, or cancelled states", self.dim()),
                make_swatch("Heading", "Section titles and bold leads", self.heading()),
            ],
            signals: vec![
                make_swatch(
                    "Brand",
                    "Primary selection, active tabs, and identity",
                    self.brand(),
                ),
                make_swatch("Info", "Turn header anchors and structure", self.info()),
                make_swatch(
                    "Success",
                    "Completed steps, git adds, and approvals",
                    self.ok(),
                ),
                make_swatch(
                    "Warning",
                    "Confirmations, security notices, and cautions",
                    self.warn(),
                ),
                make_swatch(
                    "Error",
                    "Command failures, rejections, and denials",
                    self.err(),
                ),
                make_swatch(
                    "Running",
                    "In-flight execution vitality (ADR-0110)",
                    self.running(),
                ),
            ],
            components: vec![
                make_swatch("Diff Add", "Diff inserted row tint", self.diff_add_bg()),
                make_swatch("Diff Del", "Diff deleted row tint", self.diff_del_bg()),
                make_swatch(
                    "Pane Marker",
                    "Gutter keyboard focus accent bar",
                    self.pane_marker_style().fg,
                ),
                make_swatch("Keycap", "Keyboard shortcut badge text", self.keycap_fg()),
            ],
        }
    }
}

/// The channels of an RGB color, or `None` for `Reset` / named ANSI slots, which
/// have no defined RGB to compute with.
///
/// Every derivation that has to *measure* a palette — the pane headroom clamp, the
/// marker's neutrality, the swatch-staging band — goes through this rather than
/// [`rgb`]'s placeholder greys, because a `Reset` surface has no channels and
/// pretending it does silently fabricates headroom that is not there.
fn rgb_opt(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// Whether shifting `band` by the per-channel delta from `page` to `wash` stays
/// inside the channel range — i.e. whether the additive pane lift can preserve
/// this surface's elevation without clamping it.
#[cfg(test)]
fn survives_the_lift(page: Color, wash: Color, band: Color) -> bool {
    let (Some((wr, wg, wb)), Some((pr, pg, pb)), Some((br, bgc, bb))) =
        (rgb_opt(wash), rgb_opt(page), rgb_opt(band))
    else {
        return true;
    };
    let ok = |w: u8, p: u8, b: u8| (0..=255).contains(&(b as i16 + (w as i16 - p as i16)));
    ok(wr, pr, br) && ok(wg, pg, bgc) && ok(wb, pb, bb)
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

/// Weighted RGB distance between two colors (the classic "redmean" metric):
/// cheap, and perceptually closer than plain Euclidean RGB, which matters when
/// deciding whether a highlight band would visibly separate from a palette
/// chip or fuse into it. `Color::Reset` (the terminal default) has no defined
/// RGB, so it is treated as maximally distant — a chip we cannot measure is
/// never the reason a band gets rejected.
fn color_distance(a: Color, b: Color) -> f32 {
    if a == Color::Reset || b == Color::Reset {
        return f32::MAX;
    }
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let (dr, dg, db) = (
        ar as f32 - br as f32,
        ag as f32 - bg as f32,
        ab as f32 - bb as f32,
    );
    let rmean = (ar as f32 + br as f32) / 2.0;
    let weight_r = 2.0 + rmean / 256.0;
    let weight_g = 4.0;
    let weight_b = 2.0 + (255.0 - rmean) / 256.0;
    (weight_r * dr * dr + weight_g * dg * dg + weight_b * db * db).sqrt()
}

/// The smallest [`color_distance`] between `band` and every `swatch`. Used to
/// choose the highlight that leaves the advertised palette chips most legible.
fn min_swatch_distance(band: Color, swatches: &[Color]) -> f32 {
    swatches
        .iter()
        .map(|swatch| color_distance(band, *swatch))
        .fold(f32::MAX, f32::min)
}

/// Pull `color` toward the neutral grey of the same mean channel value by
/// `amount` (0.0 = unchanged, 1.0 = fully grey).
///
/// The transcript's keyboard cues must read as *marks*, never as states: every
/// tone a renderer picks for a row carries meaning (disclosure luminance,
/// lifecycle hue), so a cue that borrowed even a little chroma would start to
/// look like one of them. Stripping the chroma makes the marker legible on
/// light and dark palettes alike without claiming a semantic hue.
fn desaturate(color: Color, amount: f32) -> Color {
    let (r, g, b) = rgb(color);
    let grey = (r as f32 + g as f32 + b as f32) / 3.0;
    let amount = amount.clamp(0.0, 1.0);
    let channel = |c: u8| (c as f32 + (grey - c as f32) * amount).round() as u8;
    Color::Rgb(channel(r), channel(g), channel(b))
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

    /// Keyboard ownership of the transcript is carried by **area**, not by
    /// per-cell contrast: the pane wash must be visible in every colour-capable
    /// scheme but must stay a small step, because it lands on every cell of the
    /// viewport (ADR-0145, amended).
    ///
    /// Both halves are load-bearing and they pull in opposite directions:
    ///
    /// - a wash a couple of luminance units off the page is *nominal* — the
    ///   crossing looks like nothing happened, which is the original defect;
    /// - a wash as large as a selection band blinds the transcript, which is the
    ///   other failure the first band shipped.
    ///
    /// So the step is bounded on both sides: a visible floor and a ceiling well
    /// below the `selected` tone.
    #[test]
    fn pane_focus_is_a_small_but_visible_wash_in_every_color_scheme() {
        for name in ["zen", "midnight", "nord", "catppuccin", "paper"] {
            let theme = Theme::from_color_scheme(name, &ColorSchemeConfig::default());
            let wash = theme.pane_focus();
            assert_ne!(
                wash,
                theme.surface(),
                "{name}: the pane wash must not melt into the page"
            );
            let step = (luminance(wash) - luminance(theme.surface())).abs();
            assert!(
                step >= 4.0,
                "{name}: a {step:.1}-unit step is not a visible wash (wash {wash:?}, page {:?})",
                theme.surface()
            );
            assert!(
                step <= 20.0,
                "{name}: a {step:.1}-unit wash is a highlight, not a page state — it would \
                 wash out every tone resolved for the page"
            );
            // A keyboard-ownership cue must never read as a text selection.
            assert_ne!(
                wash,
                theme.selected(),
                "{name}: the pane wash must not equal the selection tone"
            );
            // The lift is applied **additively** to every background in the pane, so
            // each band keeps its elevation above the page and the document's ladder
            // survives. The condition for that is headroom, and the token layer
            // *enforces* it rather than assuming it: `pane_focus()` scales the
            // declared wish back to the largest step this palette can absorb, so no
            // band can clamp. (Asserted structurally below, over hostile palettes as
            // well as the built-ins.)
            let delta = theme
                .pane_focus_delta()
                .unwrap_or_else(|| panic!("{name}: the built-in schemes must carry a pane wash"));
            for band in theme.pane_bands() {
                assert!(
                    survives_the_lift(theme.surface(), theme.pane_focus(), band),
                    "{name}: the derived wash must never clamp {band:?}"
                );
            }
            // And the derived delta is the declared wish, unclamped, on a palette that
            // has room — the clamp must not quietly shrink every scheme.
            let wished = |w: Color, p: Color| -> i16 {
                let (wr, _, _) = rgb(w);
                let (pr, _, _) = rgb(p);
                wr as i16 - pr as i16
            };
            if name != "paper" {
                assert_eq!(
                    delta[0],
                    wished(theme.pane_focus_bg, theme.surface()),
                    "{name}: a palette with headroom keeps the declared step exactly"
                );
            }
            // The page's own foreground must stay legible on it without any
            // re-seat, because the wash covers prose the renderers never touch.
            let text_step = (luminance(theme.fg()) - luminance(wash)).abs();
            assert!(
                text_step >= 100.0,
                "{name}: body text must stay legible on the pane wash ({text_step:.0})"
            );
        }
    }

    /// `pane_lifted()` must lift **exactly** the fields the headroom classification
    /// calls backgrounds — no more, no less.
    ///
    /// This is the test that closes the drift between the two lists. It is *derived*
    /// from `background_fields()` rather than enumerating a handful of fields, because a
    /// hand-picked list is precisely how the original bug survived: a field was added,
    /// classified, and never lifted, and a nine-field assertion had no way to notice the
    /// tenth.
    ///
    /// The two directions:
    /// - every field classified `true` **must** move by the delta;
    /// - every field classified `false` **must** arrive byte-identical.
    ///
    /// The second half matters as much as the first: a foreground that drifted with the
    /// page would change the document's *meaning* (a running hue means the same thing at
    /// either page level), which is a worse failure than a flattened band.
    #[test]
    fn pane_lifted_lifts_exactly_the_classified_backgrounds() {
        let theme = Theme::default();
        let lifted = theme.pane_lifted();
        let delta = theme.pane_focus_delta().expect("zen carries a wash");
        let shift = |c: Color| -> Color {
            match rgb_opt(c) {
                Some((r, g, b)) => {
                    let f = |c: u8, d: i16| (c as i16 + d).clamp(0, 255) as u8;
                    Color::Rgb(f(r, delta[0]), f(g, delta[1]), f(b, delta[2]))
                }
                None => c,
            }
        };

        let before = theme.background_fields();
        let after = lifted.background_fields();
        assert_eq!(before.len(), after.len(), "the field sets must correspond");

        for ((field, was, is_background), (_, now, _)) in before.iter().zip(after.iter()) {
            assert_eq!(field, field, "the two classifications must agree on order");
            if *is_background {
                assert_eq!(
                    now,
                    &shift(*was),
                    "{field}: classified as a pane background but not lifted — its \
                     elevation above the page would be truncated"
                );
            } else {
                assert_eq!(
                    now, was,
                    "{field}: classified as NOT a pane background but it moved — \
                     foregrounds and foreign surfaces must not drift with the page"
                );
            }
        }
    }

    /// Lifting the *theme* must be exactly equivalent to restyling every cell of the
    /// painted pane — that equivalence is what lets the renderer take the cheap path
    /// (`Theme::pane_lifted`) instead of the per-cell one, and it is what keeps the
    /// field list in `pane_lifted` honest: a background that is not lifted there
    /// fails here.
    ///
    /// "Background" is the operative word: foregrounds are semantic and must not
    /// move, or a running hue would change meaning with the keyboard's location.
    #[test]
    fn pane_lifted_theme_matches_a_cell_wise_restyle() {
        let theme = Theme::default();
        let lifted = theme.pane_lifted();
        let delta = theme.pane_focus_delta().expect("zen carries a wash");
        let shift = |c: Color| match rgb_opt(c) {
            Some((r, g, b)) => {
                let f = |c: u8, d: i16| (c as i16 + d).clamp(0, 255) as u8;
                Color::Rgb(f(r, delta[0]), f(g, delta[1]), f(b, delta[2]))
            }
            None => c,
        };

        // Every background the transcript reaches for moves by exactly the delta.
        for (label, before, after) in [
            ("page", theme.app_bg, lifted.app_bg),
            ("body", theme.menu_bg, lifted.menu_bg),
            ("code", theme.code_bg, lifted.code_bg),
            ("raised", theme.element_bg, lifted.element_bg),
            (
                "command card",
                theme.command_band_bg,
                lifted.command_band_bg,
            ),
            ("selection", theme.selected_bg, lifted.selected_bg),
            ("user panel", theme.user_panel_bg, lifted.user_panel_bg),
            ("diff add", theme.diff_add_bg, lifted.diff_add_bg),
            ("diff del", theme.diff_del_bg, lifted.diff_del_bg),
        ] {
            assert_eq!(after, shift(before), "{label} must be lifted by the delta");
        }

        // Foregrounds are untouched.
        assert_eq!(lifted.text, theme.text);
        assert_eq!(lifted.text_muted, theme.text_muted);
        assert_eq!(lifted.running_fg, theme.running_fg);
        assert_eq!(lifted.info, theme.info);
        assert_eq!(lifted.pane_marker_fg, theme.pane_marker_fg);

        // And the lift is a translation: every separation is invariant.
        for band in theme.pane_bands() {
            let before = luminance(band) - luminance(theme.surface());
            let after = luminance(shift(band)) - luminance(lifted.surface());
            assert!(
                (after - before).abs() < 0.01,
                "{band:?} moved relative to the page"
            );
        }
    }

    /// Every reachable palette must land on **one** of the two correct answers, and
    /// never on a third: either a wash that preserves the whole ladder, or the gutter
    /// fallback. This is the sweep the earlier design never ran — it only checked the
    /// five built-in schemes, so a hand-authored theme could produce a wash that
    /// clamped its own bands.
    ///
    /// The palettes covered are exactly the ones a user can reach: the built-ins
    /// through the scheme table (already swept elsewhere), the custom palettes built
    /// from arbitrary hex semantics, and the two `Reset` archetypes.
    #[test]
    fn every_reachable_palette_lands_on_a_correct_pane_cue() {
        let custom = ColorSchemeConfig::default();
        let mut palettes: Vec<(String, Theme)> = Vec::new();
        for scheme in Theme::available_color_schemes() {
            palettes.push((
                scheme.id.to_string(),
                Theme::from_color_scheme(&scheme.id, &custom),
            ));
        }
        // Extremes a custom `colors` block can actually produce: near-white paper, near
        // -black ink, and a page that sits above and below its own surfaces.
        for (label, background, surface, text) in [
            ("near-white", "#fefefe", "#ffffff", "#101010"),
            ("near-black", "#010101", "#050505", "#e0e0e0"),
            ("mid-grey", "#808080", "#909090", "#303030"),
            ("page-above-surface", "#f0f0f0", "#e0e0e0", "#202020"),
            ("page-below-surface", "#202020", "#f0f0f0", "#f8f8f8"),
        ] {
            palettes.push((
                label.to_string(),
                Theme::from_custom(&ColorSchemeConfig {
                    background: background.to_string(),
                    surface: surface.to_string(),
                    text: text.to_string(),
                    ..Default::default()
                }),
            ));
        }

        for (label, theme) in palettes {
            if !theme.has_pane_wash() {
                // The fallback must still be a real cue.
                assert_ne!(
                    theme.pane_focus_edge_style(),
                    Style::default(),
                    "{label}: a palette without a wash needs a fallback cue"
                );
                continue;
            }
            assert!(
                theme.pane_focus_delta().is_some(),
                "{label}: has_pane_wash must imply a delta"
            );
            for band in theme.pane_bands() {
                assert!(
                    survives_the_lift(theme.surface(), theme.pane_focus(), band),
                    "{label}: the derived wash would clamp {band:?}"
                );
            }
            // And the cue is a *visible* step, or it is not a cue.
            let step = (luminance(theme.pane_focus()) - luminance(theme.surface())).abs();
            assert!(step >= 1.0, "{label}: a {step:.2}-unit wash is nominal");
        }
    }

    /// A palette with **no headroom** must lose the wash, not flatten its own
    /// document.
    ///
    /// This is the case the earlier design could not express: it declared the wash
    /// per *archetype* and trusted the literal, so a `Chromatic` theme whose bands
    /// sat at the pole the lift travels toward had no way to say "this palette cannot
    /// carry a whole-pane cue". The clamp now measures the palette and answers
    /// honestly — the transcript takes the gutter form instead of a washed-out page.
    #[test]
    fn a_palette_without_headroom_refuses_the_wash() {
        // A band already sitting on the ceiling the lift travels toward: shifting it
        // would clamp, and clamping it is exactly the flattening this design exists to
        // avoid. There is no legal step, so there is no wash.
        let pinned = Theme {
            app_bg: Color::Rgb(250, 250, 250),
            pane_focus_bg: Color::Rgb(255, 255, 255),
            code_bg: Color::Rgb(255, 255, 255),
            ..Default::default()
        };
        assert!(
            !pinned.has_pane_wash(),
            "a band pinned at the pole leaves no room for the lift"
        );
        assert_eq!(pinned.pane_focus(), Color::Reset);
        // …and the fallback still names a pane-scoped background, so the crossing is
        // not silent on that palette either.
        assert_ne!(pinned.pane_focus_edge_style(), Style::default());

        // The same palette becomes usable the moment the band gives back one unit —
        // the refusal is a measurement, not a blacklist.
        let one_unit = Theme {
            code_bg: Color::Rgb(254, 254, 254),
            ..pinned
        };
        assert!(one_unit.has_pane_wash());
        assert_eq!(one_unit.pane_focus(), Color::Rgb(251, 251, 251));
    }

    /// A wash that *would* clamp is **scaled back**, not clamped. The distinction is
    /// the whole point: clamping truncates the step (and silently flattens the
    /// band it truncated), while scaling preserves the ladder at a smaller
    /// magnitude — the cue stays honest about how much room there is.
    #[test]
    fn an_oversized_wash_is_scaled_back_to_the_headroom() {
        // One band pinned at the top of the range: the lift may travel at most as far
        // as that band's headroom allows.
        let theme = Theme {
            app_bg: Color::Rgb(10, 10, 10),
            code_bg: Color::Rgb(250, 250, 250),
            pane_focus_bg: Color::Rgb(200, 200, 200),
            ..Default::default()
        };
        let delta = theme
            .pane_focus_delta()
            .expect("there is headroom, just little");
        assert_eq!(
            delta[0], 5,
            "the lift is bounded by the band's 5 units of room, not by the declared 190"
        );
        assert!(
            survives_the_lift(theme.surface(), theme.pane_focus(), theme.code_surface()),
            "the derived wash must not clamp the band"
        );
        // The derived tone is the *scaled* step, not the declared wish.
        assert_eq!(theme.pane_focus(), Color::Rgb(15, 15, 15));
    }

    /// A hand-authored override moves the **wish**, never the invariant.
    ///
    /// The configuration surface exists so a theme can state these two tones. It must
    /// not exist as a way to flatten a transcript: an override that lands inside a
    /// band's headroom is scaled like any other wish, and one that points back at the
    /// page is refused outright.
    #[test]
    fn a_theme_override_cannot_flatten_the_pane() {
        use crate::color_scheme::ThemeFile;

        let file = ThemeFile {
            components: Some(crate::color_scheme::ComponentThemesConfig {
                pane: Some(crate::color_scheme::PaneThemeConfig {
                    wash_bg: Some("#ffffff".to_string()),
                    marker_fg: Some("#888888".to_string()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let theme = Theme::from_theme_file(&file);
        assert_eq!(theme.pane_marker_fg, Color::Rgb(136, 136, 136));
        for band in theme.pane_bands() {
            assert!(
                survives_the_lift(theme.surface(), theme.pane_focus(), band),
                "an override must not clamp {band:?}"
            );
        }

        // An override that names the page itself is a no-op, not a silent wash.
        let flat = ThemeFile {
            components: Some(crate::color_scheme::ComponentThemesConfig {
                pane: Some(crate::color_scheme::PaneThemeConfig {
                    wash_bg: Some("#070808".to_string()),
                    marker_fg: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let flat = Theme::from_theme_file(&flat);
        assert!(!flat.has_pane_wash(), "a zero-step wash is not a cue");
        assert_eq!(flat.pane_focus(), Color::Reset);
    }

    /// The entry marker holds a **fixed absolute tone**, so it must stay distinguishable
    /// from the page *after* the page is lifted — not just before.
    ///
    /// This is the property that replaces "keeps its elevation" for a non-surface mark.
    /// It is asserted separately because the two are genuinely different requirements: a
    /// band must preserve its step, a mark must not be swallowed. Classifying the marker
    /// as a background would have implied the former and hidden the latter.
    #[test]
    fn the_entry_marker_stays_distinguishable_from_the_lifted_page() {
        let custom = ColorSchemeConfig::default();
        for scheme in Theme::available_color_schemes() {
            let theme = Theme::from_color_scheme(&scheme.id, &custom);
            let marker = theme.pane_marker_fg;
            for (state, page) in [("blurred", theme.surface()), ("lifted", theme.pane_focus())] {
                if page == Color::Reset || marker == Color::Reset {
                    continue;
                }
                let step = (luminance(marker) - luminance(page)).abs();
                assert!(
                    step >= 40.0,
                    "{}: the marker is only {step:.0} from the {state} page — the cue \
                     would be swallowed",
                    scheme.id
                );
            }
        }
    }

    /// The entry marker is a **neutral luminance rung**: it must separate from
    /// both the page and the entry's own text without borrowing a semantic hue,
    /// so "the keyboard is on this entry" can never be read as an outcome state
    /// or as a selection (ADR-0145, amended).
    #[test]
    fn entry_focus_marker_is_neutral_and_visible() {
        for name in ["zen", "midnight", "nord", "catppuccin", "paper"] {
            let theme = Theme::from_color_scheme(name, &ColorSchemeConfig::default());
            let marker = theme.pane_marker_fg;
            let (r, g, b) = rgb(marker);
            // Neutral: no channel may dominate, so the marker cannot be mistaken
            // for a hue-coded lifecycle state.
            let spread = r.max(g).max(b) as i32 - r.min(g).min(b) as i32;
            assert!(
                spread <= 24,
                "{name}: the entry marker must be a neutral rung, not a hue ({marker:?})"
            );
            assert_ne!(marker, theme.selected(), "{name}: never the selection tone");
            assert_ne!(marker, theme.fg(), "{name}: never the body text tone");
            let step = (luminance(marker) - luminance(theme.surface())).abs();
            assert!(
                step >= 60.0,
                "{name}: a {step:.0}-unit marker is invisible on the page"
            );
        }
    }

    /// Liveness owns its own hue (ADR-0110 `[INV-TUI-RUNNING-01]`, amended).
    ///
    /// The reported ambiguity: a running tool step and the `› turn N` header
    /// above it were both `info`, so "this row is working" and "this is where the
    /// turn starts" read as the same fact. The in-flight tone must therefore be
    /// distinct from every structural and outcome tone in every scheme.
    #[test]
    fn running_hue_is_distinct_from_structure_and_outcomes() {
        for name in ["zen", "midnight", "nord", "catppuccin", "paper", "ansi16"] {
            let theme = Theme::from_color_scheme(name, &ColorSchemeConfig::default());
            let running = theme.running();
            for (label, other) in [
                ("info", theme.info()),
                ("brand", theme.brand()),
                ("ok", theme.ok()),
                ("warn", theme.warn()),
                ("err", theme.err()),
                ("muted", theme.muted()),
            ] {
                assert_ne!(
                    running, other,
                    "{name}: the in-flight hue must not be the {label} tone"
                );
                if matches!(other, Color::Rgb(..)) && matches!(running, Color::Rgb(..)) {
                    assert!(
                        color_distance(running, other) >= 90.0,
                        "{name}: the in-flight hue is too close to {label} ({:?} vs {other:?})",
                        running
                    );
                }
            }
        }
    }

    /// On the no-colour archetypes the marker is carried by DEC reverse video
    /// instead, so no colour escape is emitted (ADR-0053). The cue must still
    /// exist — silence here is the original bug.
    #[test]
    fn entry_focus_is_reverse_video_on_a_structured_profile() {
        // VT100 / getty: no colour latitude at all, so the cue is the modifier.
        let mono = Theme::monochrome();
        assert_eq!(mono.pane_marker_fg, Color::Reset);
        assert_eq!(mono.pane_focus(), Color::Reset);
        assert!(
            mono.pane_marker_style()
                .add
                .contains(nuotc::Modifier::REVERSE),
            "a VT100 says \"the keyboard is on this entry\" with reverse video"
        );

        // ANSI-16 keeps the named slots: the pane cannot be lifted (its page is
        // the terminal's own background) and the marker uses the brightest
        // discrete slot rather than an RGB the console would quantize away.
        let hybrid = Theme::ansi16();
        assert_eq!(hybrid.pane_focus(), Color::Reset);
        assert_eq!(hybrid.pane_marker_fg, Color::White);
        assert!(!matches!(hybrid.pane_marker_fg, Color::Rgb(..)));
        assert_eq!(hybrid.pane_marker_style().fg, Color::White);
        // Liveness is a named slot too — never an RGB.
        assert!(!matches!(hybrid.running(), Color::Rgb(..)));
    }

    /// A listing row's color follows the shell's `ls` convention — blue dir,
    /// green exec, cyan link — while a plain file keeps the scheme's content
    /// tone, and the four are mutually distinct so a type never reads as another.
    #[test]
    fn listing_color_matches_ls_convention() {
        let theme = Theme::default();
        let dir = theme.listing_color(ListingClass::Dir);
        let exec = theme.listing_color(ListingClass::Exec);
        let link = theme.listing_color(ListingClass::Link);
        let file = theme.listing_color(ListingClass::File);
        assert_eq!(file, theme.code_text(), "a plain file is the content tone");
        assert_ne!(dir, exec);
        assert_ne!(dir, link);
        assert_ne!(exec, link);
        // Blue dominates a directory hue: blue channel is the strongest.
        let (r, g, b) = rgb(dir);
        assert!(b > r && b > g, "the directory hue reads as blue");
        // Green dominates the executable hue.
        let (r, g, b) = rgb(exec);
        assert!(g > r && g > b, "the executable hue reads as green");
    }

    /// Monochrome (DEC VT100) must emit no hue at all: every listing class
    /// collapses to the reset content tone so the type is carried structurally
    /// (the directory trailing `/`) rather than by color.
    #[test]
    fn listing_color_is_hueless_under_monochrome() {
        let theme = Theme::monochrome();
        for class in [
            ListingClass::Dir,
            ListingClass::Exec,
            ListingClass::Link,
            ListingClass::File,
        ] {
            assert_eq!(
                theme.listing_color(class),
                theme.code_text(),
                "monochrome listing rows carry no color cue"
            );
        }
    }

    /// ANSI-16 uses the terminal's own named slots (never an RGB the console
    /// cannot render as one of its 16 colors).
    #[test]
    fn listing_color_uses_named_slots_under_ansi16() {
        let theme = Theme::ansi16();
        assert_eq!(theme.listing_color(ListingClass::Dir), Color::LightBlue);
        assert_eq!(theme.listing_color(ListingClass::Exec), Color::LightGreen);
        assert_eq!(theme.listing_color(ListingClass::Link), Color::LightCyan);
    }

    /// The hover band is derived from the palette, never a fixed gray, and is
    /// chosen so the row's own swatches stay legible against it: the picked
    /// band is at least as far from the closest swatch as the palette's
    /// inverse pole would be.
    #[test]
    fn row_hover_band_stages_the_row_swatches() {
        let custom = ColorSchemeConfig::default();
        for scheme in Theme::available_color_schemes() {
            let page = Theme::from_color_scheme(&scheme.id, &custom);
            let preview = Theme::from_color_scheme(&scheme.id, &custom);
            let swatches = [
                preview.body(),
                preview.panel(),
                preview.brand(),
                preview.info(),
                preview.ok(),
                preview.warn(),
            ];
            let band = page.row_hover_band(&swatches);

            // Monochrome owns no background latitude: the band is inert and the
            // caller falls back to reverse video. Everything else must visibly
            // differ from the resting body.
            if page.elevation == ElevationArchetype::Structured {
                assert_eq!(band, Color::Reset);
                continue;
            }
            assert_ne!(
                band,
                page.body(),
                "{}: the hover band must differ from the resting body",
                scheme.id
            );

            // The swatch-staging guarantee is a truecolor concern: ANSI-16 uses
            // its named highlight verbatim, so skip it here.
            if page.elevation != ElevationArchetype::Chromatic {
                continue;
            }

            // The band must not fuse into any swatch: it is at least as legible
            // as the plain inverse pole the palette could have offered.
            let light = luminance(page.app_bg) > 150.0;
            let inverse = mix(
                page.app_bg,
                if light { Color::Black } else { page.text },
                0.82,
            );
            assert!(
                min_swatch_distance(band, &swatches) >= min_swatch_distance(inverse, &swatches),
                "{}: band must not be closer to a swatch than the inverse pole",
                scheme.id
            );

            // Text painted on the band must separate from it (never fg-on-fg).
            let text = page.band_text(band, page.fg());
            assert!(
                (luminance(band) - luminance(text)).abs() >= 60.0,
                "{}: band text must contrast with the band",
                scheme.id
            );
        }
    }

    /// Monochrome owns no background latitude: the band collapses to `Reset`
    /// and the caller falls back to reverse video, so the cue never fabricates
    /// a color the hardware cannot render.
    #[test]
    fn row_hover_band_is_inert_on_monochrome() {
        let mono = Theme::monochrome();
        assert_eq!(mono.row_hover_band(&[]), Color::Reset);
        // On a Reset band the preferred foreground is returned verbatim.
        assert_eq!(
            mono.band_text(Color::Reset, Color::Rgb(1, 2, 3)),
            Color::Rgb(1, 2, 3)
        );
    }

    /// The sunken pane rung is a *deeper* tone than the panel and body it sits
    /// under, so a content pane can separate from the head band above it on
    /// every RGB scheme — while Reset-based archetypes stay inert.
    #[test]
    fn pane_sunken_is_a_deeper_in_family_rung() {
        let custom = ColorSchemeConfig::default();
        for scheme in Theme::available_color_schemes() {
            let theme = Theme::from_color_scheme(&scheme.id, &custom);
            let sunken = theme.pane_sunken();
            if theme.elevation != ElevationArchetype::Chromatic {
                assert_eq!(sunken, Color::Reset, "{}: reset archetype", scheme.id);
                continue;
            }
            assert!(
                luminance(sunken) < luminance(theme.panel()),
                "{}: sunken must be deeper than the panel",
                scheme.id
            );
            assert_ne!(
                sunken,
                theme.raised(),
                "{}: sunken must not equal the head band tone",
                scheme.id
            );
        }
    }

    #[test]
    fn every_preset_has_a_distinct_canonical_index() {
        let schemes = Theme::available_color_schemes();
        for (index, scheme) in schemes.iter().enumerate() {
            assert_eq!(Theme::color_scheme_index(&scheme.id), index);
        }
    }

    #[test]
    fn discovers_external_theme_files_in_available_color_schemes() {
        // Hermetic: the projection takes parsed theme files, so the test parses
        // the TOML directly instead of touching the filesystem (ADR-0073).
        let raw = r##"
id = "monokai-pro"
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
        let files = vec![toml::from_str::<ThemeFile>(raw).expect("theme toml")];

        let schemes = Theme::available_color_schemes_from(&files);
        assert!(schemes.iter().any(|s| s.id == "monokai-pro" && s.is_file));
        let theme =
            Theme::from_color_scheme_from("monokai-pro", &ColorSchemeConfig::default(), &files);
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
                // ADR-0053: Console/Monochrome profiles anchor backgrounds to Reset
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

    /// The command card must read as a *card* in every scheme —
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
                // ADR-0053: Console/Monochrome profiles anchor backgrounds to Reset
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

    /// `[INV-HEAD-06]` The top chrome tiers must exhibit a discrete, in-family micro-elevation
    /// ladder across all preset color schemes.
    #[test]
    fn header_tiers_micro_elevation_is_guaranteed_in_every_scheme() {
        let custom = ColorSchemeConfig::default();
        for scheme in &COLOR_SCHEMES {
            if scheme.id == "ansi16" || scheme.id == "monochrome" {
                continue;
            }
            let theme = Theme::from_color_scheme(&scheme.id, &custom);
            let tabbar = theme.tabbar_bg();
            let scenebar = theme.scenebar_bg();
            let contextbar = theme.contextbar_bg();

            assert_ne!(
                tabbar, scenebar,
                "{scheme:?}: tabbar and scenebar must not share identical fill"
            );
            assert_ne!(
                scenebar, contextbar,
                "{scheme:?}: scenebar and contextbar must not share identical fill"
            );
            assert_ne!(
                tabbar, contextbar,
                "{scheme:?}: tabbar and contextbar must not share identical fill"
            );

            let tab_lum = luminance(tabbar);
            let scene_lum = luminance(scenebar);
            let ctx_lum = luminance(contextbar);
            let base_lum = luminance(theme.surface());

            if base_lum <= 150.0 {
                // Dark mode: SceneBar is the elevated spotlight, ContextBar is intermediate, TabBar is base.
                assert!(
                    scene_lum > ctx_lum,
                    "{scheme:?}: scenebar must have higher luminance than contextbar"
                );
                assert!(
                    ctx_lum > tab_lum,
                    "{scheme:?}: contextbar must have higher luminance than tabbar"
                );
                assert!(
                    scene_lum - tab_lum >= 4.0,
                    "{scheme:?}: scenebar and tabbar must have perceptible contrast"
                );
            } else {
                // Light mode: surfaces mix toward dark text, so elevation steps down in luminance.
                assert!(
                    scene_lum < ctx_lum,
                    "{scheme:?}: scenebar must step darker than contextbar in light mode"
                );
                assert!(
                    ctx_lum < tab_lum,
                    "{scheme:?}: contextbar must step darker than tabbar in light mode"
                );
            }
        }
    }

    #[test]
    fn spatial_surfaces_and_feedback_tokens_contract() {
        let theme = Theme::default();
        let surfaces = theme.surfaces();
        assert_eq!(surfaces.view.canvas, theme.surface());
        assert_eq!(surfaces.modal.surface, theme.panel());
        assert_eq!(surfaces.modal.dim_factor, 0.5);
        assert_eq!(surfaces.overlay.toast_bg, theme.toast_bg());
        assert_eq!(surfaces.overlay.which_key_bg, theme.which_key_bg());

        let fb_warn = theme.feedback(NoticeSeverity::Warning);
        assert_eq!(fb_warn.border, theme.warn());

        let fb_err = theme.feedback(NoticeSeverity::Error);
        assert_eq!(fb_err.border, theme.err());

        let fb_info = theme.feedback(NoticeSeverity::Info);
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

[surfaces.overlay]
toast_bg = "#333333"
which_key_bg = "#282828"
"##;
        let file: ThemeFile = toml::from_str(raw).expect("should parse");
        let theme = Theme::from_theme_file(&file);
        assert_eq!(theme.surfaces().view.canvas, Color::Rgb(1, 1, 1));
        assert_eq!(theme.surfaces().view.header_bg, Color::Rgb(17, 17, 17));
        assert_eq!(theme.surfaces().modal.surface, Color::Rgb(34, 34, 34));
        assert_eq!(theme.surfaces().modal.dim_factor, 0.65);
        assert_eq!(theme.surfaces().overlay.toast_bg, Color::Rgb(51, 51, 51));
        assert_eq!(
            theme.surfaces().overlay.which_key_bg,
            Color::Rgb(40, 40, 40)
        );
        assert_eq!(theme.toast_bg(), Color::Rgb(51, 51, 51));
        assert_eq!(theme.which_key_bg(), Color::Rgb(40, 40, 40));
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
    fn severity_band_tints_toward_the_accent_and_falls_back_on_reset_surfaces() {
        // Dark RGB profile: the band is a subtle wash of the accent over the
        // app background — never the raw accent (too loud for a card band) and
        // never the plain background (severity invisible). The foreground pulls
        // the accent toward text so the label stays readable.
        let dark = Theme::default();
        assert!(
            dark.severity_is_color_distinguishable(),
            "a dark RGB theme carries severity in color"
        );
        let err = dark.severity_band(dark.err(), "error");
        assert_ne!(err.bg, dark.err(), "band bg must not be the raw accent");
        assert_ne!(err.bg, dark.app_bg, "band bg must not be the plain surface");
        assert_ne!(err.fg, dark.err(), "band fg must not be the raw accent");
        assert_ne!(err.bg, err.fg, "bg and fg must stay distinct");
        assert_eq!(err.marker, "", "color carries it; no textual marker needed");

        // The three severity accents produce mutually distinct bands, so
        // color-only severity never collapses two severities into one tint.
        let warn = dark.severity_band(dark.warn(), "warn");
        let ok = dark.severity_band(dark.ok(), "ok");
        let info = dark.severity_band(dark.info(), "info");
        assert_ne!(err.bg, warn.bg);
        assert_ne!(warn.bg, ok.bg);
        assert_ne!(ok.bg, info.bg);

        // ANSI-16: `app_bg` is Reset but the accents are real ANSI colors, so
        // severity is still distinguishable by color; the band uses the raised
        // surface and keeps the accent, and emits no textual marker.
        let ansi = Theme::from_color_scheme("ansi16", &ColorSchemeConfig::default());
        assert_eq!(ansi.app_bg, Color::Reset);
        assert!(ansi.severity_is_color_distinguishable());
        let band = ansi.severity_band(ansi.err(), "error");
        assert_eq!(band.bg, ansi.raised());
        assert_eq!(band.fg, ansi.err());
        assert_eq!(band.marker, "");

        // Monochrome/VT100: every accent is `Reset`, so color carries NOTHING.
        // The band must not pretend a tint exists — it emits a textual marker
        // instead, or severity would be invisible after the glyph removal.
        let mono = Theme::from_color_scheme("monochrome", &ColorSchemeConfig::default());
        assert!(
            !mono.severity_is_color_distinguishable(),
            "monochrome cannot carry severity in color"
        );
        let err = mono.severity_band(mono.err(), "error");
        let warn = mono.severity_band(mono.warn(), "warn");
        assert_eq!(err.marker, "error");
        assert_eq!(warn.marker, "warn");
        assert_ne!(
            err.marker, warn.marker,
            "monochrome severities stay distinct via the textual marker"
        );
    }

    #[test]
    fn test_resolve_with_profile_adapts_defaults() {
        let vt100_prof = nuotc::TerminalProfile::dec_vt100_monochrome();
        let theme =
            Theme::resolve_with_profile("zen", &ColorSchemeConfig::default(), &[], &vt100_prof);
        assert_eq!(theme.app_bg, Color::Reset);
        assert_eq!(theme.text, Color::Reset);

        let linux_prof = nuotc::TerminalProfile::ecma48_ansi16();
        let theme_linux =
            Theme::resolve_with_profile("zen", &ColorSchemeConfig::default(), &[], &linux_prof);
        assert_eq!(theme_linux.app_bg, Color::Reset);
        assert_eq!(theme_linux.text, Color::White);

        // Explicit non-default choice is preserved
        let theme_nord =
            Theme::resolve_with_profile("nord", &ColorSchemeConfig::default(), &[], &vt100_prof);
        assert_ne!(theme_nord.text, Color::Reset);
    }
}
