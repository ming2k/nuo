//! Color scheme presets and default palette constants.
use nuotc::Color;
use std::borrow::Cow;

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
pub(crate) const LISTING_DIR_HUE: Color = Color::Rgb(86, 132, 214);
pub(crate) const LISTING_EXEC_HUE: Color = Color::Rgb(86, 176, 106);
pub(crate) const LISTING_LINK_HUE: Color = Color::Rgb(74, 172, 194);

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
pub(crate) const RUNNING_HUE: Color = Color::Rgb(56, 222, 235);
