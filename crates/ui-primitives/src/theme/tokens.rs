//! Structured UI tokens for views, sheets, modals, and components.
use super::Theme;
use super::color_math::luminance;
use nuotc::Color;

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
