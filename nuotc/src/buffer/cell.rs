//! Display primitives: [`Color`], [`Style`], [`Modifier`], and [`Cell`].
//!
//! The palette is RGB-first. muta's curated near-black surface and accent
//! tokens are all `Color::Rgb`, so the diff/dim math only needs to handle the
//! RGB case; the named variants exist for interop and the terminal default
//! (`Color::Reset`). This mirrors the approach the old `primitives.rs`
//! `scale_color` took, lifted here as the canonical definition.
//!
//! A [`Cell`] is the unit a grid stores per (row, col). It carries one
//! grapheme cluster (which may be multi-codepoint, e.g. `e\u{0301}`) plus its
//! display width so the grid never has to re-measure. Wide (CJK) glyphs own a
//! trailing cell that is a [`Cell::wide_continuation`] — it carries the head's
//! background so a stale multiplexer grid can never ghost it (ADR-0038). This
//! is the fix ratatui cannot express because its `set_stringn` `reset()`s the
//! trailing cell to `Color::Reset`.

use std::fmt;

/// A terminal color.
///
/// `Reset` means "the terminal's default" and is intentionally distinct from
/// any RGB value: it is what blank padding and cleared cells resolve to when
/// the terminal (or multiplexer) paints them. The application paints a custom
/// near-black surface everywhere it *renders content*, so `Reset` only appears
/// in genuinely empty cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    Reset,
    Rgb(u8, u8, u8),
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
}

impl Color {
    /// The RGB triple if this is an RGB color, else `None`.
    pub fn as_rgb(self) -> Option<(u8, u8, u8)> {
        match self {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        }
    }

    /// Scale an RGB color's channels toward black by `factor`
    /// (0.0 = black, 1.0 = unchanged). Non-RGB colors pass through untouched
    /// so the dim is additive rather than lossy where they appear. This is
    /// the recess-backdrop dim math, lifted from the old `primitives.rs`.
    pub fn scale(self, factor: f32) -> Color {
        let f = factor.clamp(0.0, 1.0);
        match self {
            Color::Rgb(r, g, b) => Color::Rgb(
                (r as f32 * f).round() as u8,
                (g as f32 * f).round() as u8,
                (b as f32 * f).round() as u8,
            ),
            other => other,
        }
    }

    /// Relative luminance in `[0, 255]`, used to pick a contrast foreground
    /// for a colored fill (dark text on light backgrounds). Named colors map
    /// to their conventional approximate RGB so the recede/contrast heuristic
    /// still works for any stray named color.
    pub fn luminance(self) -> f32 {
        let (r, g, b) = self.to_rgb_approx();
        0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
    }

    /// Approximate RGB triple for any color variant. Named colors use the
    /// xterm-256 defaults so contrast/dim heuristics work on them too.
    fn to_rgb_approx(self) -> (u8, u8, u8) {
        match self {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Reset | Color::Black => (0, 0, 0),
            Color::Red | Color::LightRed => (224, 108, 117),
            Color::Green | Color::LightGreen => (127, 216, 143),
            Color::Yellow | Color::LightYellow => (229, 192, 123),
            Color::Blue | Color::LightBlue => (137, 180, 250),
            Color::Magenta | Color::LightMagenta => (203, 166, 247),
            Color::Cyan | Color::LightCyan => (86, 182, 194),
            Color::Gray => (128, 128, 128),
            Color::DarkGray => (64, 64, 64),
            Color::White => (255, 255, 255),
        }
    }

    /// Black on light fills, white on dark fills.
    pub fn contrast_fg(self) -> Color {
        if self.luminance() > 140.0 {
            Color::Black
        } else {
            Color::White
        }
    }

    /// Linearly interpolate between two colors in RGB space.
    ///
    /// `t = 0.0` returns `self` unchanged, `t = 1.0` returns `other` unchanged,
    /// values in between blend each channel. Named colors are mapped to their
    /// xterm-256 approximations first, so the blend is well defined for any
    /// palette token. `Color::Reset` (no color) is treated as a hole rather
    /// than black: if exactly one side is `Reset`, the other side's color is
    /// returned (i.e. there is nothing to blend with); if both are `Reset`,
    /// `Reset` is returned. `t` is clamped to `[0, 1]`.
    ///
    /// Used by the step state machine to carry a lifecycle accent's **hue**
    /// while still letting the disclosure × interaction **weight** channel
    /// brighten/darken it — so a running step (steady accent) still shows a
    /// visible hover/focus affordance instead of a flat color.
    pub fn blend(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        // Identity endpoints: t=0 → self, t=1 → other. Checked before the
        // Reset-hole special case so the blend contract holds exactly.
        if t == 0.0 {
            return self;
        }
        if t == 1.0 {
            return other;
        }
        // Holes: Reset has no color to interpolate against, so a blend that
        // leans partway into a Reset side falls back to the colored side.
        if self == Color::Reset && other == Color::Reset {
            return Color::Reset;
        }
        if self == Color::Reset {
            return other;
        }
        if other == Color::Reset {
            return self;
        }
        let (r1, g1, b1) = self.to_rgb_approx();
        let (r2, g2, b2) = other.to_rgb_approx();
        Color::Rgb(
            (r1 as f32 + (r2 as f32 - r1 as f32) * t).round() as u8,
            (g1 as f32 + (g2 as f32 - g1 as f32) * t).round() as u8,
            (b1 as f32 + (b2 as f32 - b1 as f32) * t).round() as u8,
        )
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Color::Reset => f.write_str("reset"),
            Color::Rgb(r, g, b) => write!(f, "#{r:02X}{g:02X}{b:02X}"),
            Color::Black => f.write_str("#000"),
            Color::White => f.write_str("#FFF"),
            other => write!(f, "{:?}", other),
        }
    }
}

impl From<(u8, u8, u8)> for Color {
    fn from((r, g, b): (u8, u8, u8)) -> Self {
        Color::Rgb(r, g, b)
    }
}

bitflags::bitflags! {
    /// Text attributes. Matches the SGR (Select Graphic Rendition) attribute
    /// bits a terminal can render. Kept as `bitflags` so styles compose with
    /// `|` and a diff can test "which bits changed" with `^`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct Modifier: u16 {
        const BOLD          = 1 << 0;
        const DIM           = 1 << 1;
        const ITALIC        = 1 << 2;
        const UNDERLINE     = 1 << 3;
        /// Alias matching ratatui's naming, so migrated code reads identically.
        const UNDERLINED    = Self::UNDERLINE.bits();
        const REVERSE       = 1 << 4;
        const STRIKETHROUGH = 1 << 5;
    }
}

/// The full presentation of one cell: foreground/background color and
/// attributes. Two styles diff to "what SGR sequence turns the old into the
/// new", which is what the backend emits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub add: Modifier,
}

impl Style {
    /// Style with only a foreground color.
    pub fn fg(self, fg: Color) -> Self {
        Self { fg, ..self }
    }
    /// Style with only a background color.
    pub fn bg(self, bg: Color) -> Self {
        Self { bg, ..self }
    }
    /// Add attributes. Named `add_modifier` to match ratatui's API so
    /// migrated code is a pure import-path swap.
    pub fn add_modifier(self, add: Modifier) -> Self {
        Self {
            add: self.add | add,
            ..self
        }
    }
    /// The default "nothing rendered" style: default fg, default bg, no attrs.
    /// A cell styled this way reads as the terminal's default for both colors.
    pub const RESET: Style = Style {
        fg: Color::Reset,
        bg: Color::Reset,
        add: Modifier::empty(),
    };

    /// Convenience: a style that only sets the background (fg stays default).
    pub fn fill(bg: Color) -> Self {
        Style { bg, ..Style::RESET }
    }
}

/// A single grid cell.
///
/// `symbol` is the grapheme cluster to render (a `String` because clusters
/// may be multi-codepoint; compared by equality for diffing). `width` is its
/// display width in columns (0, 1, or 2), cached at write time so the diff
/// never re-measures. `style` is its presentation.
///
/// A wide glyph (width 2) occupies this cell as the head; the column to its
/// right is a [`Cell::wide_continuation`] carrying the same background, so the
/// glyph's background spillover is always correct even if a multiplexer
/// repaints that column on its own. This is the property ratatui's buffer
/// cannot provide (it `reset()`s the trailing cell to `Color::Reset`), and the
/// reason this engine exists.
/// An inline-first compact string buffer for terminal cell symbols.
///
/// Stores grapheme clusters up to 22 bytes inline without heap allocation,
/// covering ASCII, CJK glyphs, emoji modifiers, and common ZWJ sequences.
/// Falls back to `Box<str>` only for unusually long multi-codepoint sequences.
#[derive(Clone, Eq, Hash)]
pub enum CompactSymbol {
    Inline { buf: [u8; 22], len: u8 },
    Heap(Box<str>),
}

impl CompactSymbol {
    pub const fn new() -> Self {
        Self::Inline {
            buf: [0; 22],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Inline { buf, len } => {
                let bytes = &buf[..*len as usize];
                // Safety: CompactSymbol is only constructed from valid UTF-8 strings.
                unsafe { std::str::from_utf8_unchecked(bytes) }
            }
            Self::Heap(s) => s.as_ref(),
        }
    }

    pub fn clear(&mut self) {
        match self {
            Self::Inline { len, .. } => *len = 0,
            Self::Heap(_) => {
                *self = Self::new();
            }
        }
    }

    pub fn push_str(&mut self, s: &str) {
        let current = self.as_str();
        let total_len = current.len() + s.len();
        if total_len <= 22 {
            let mut buf = [0u8; 22];
            let cur_bytes = current.as_bytes();
            buf[..cur_bytes.len()].copy_from_slice(cur_bytes);
            buf[cur_bytes.len()..total_len].copy_from_slice(s.as_bytes());
            *self = Self::Inline {
                buf,
                len: total_len as u8,
            };
        } else {
            let mut next = String::with_capacity(total_len);
            next.push_str(current);
            next.push_str(s);
            *self = Self::Heap(next.into_boxed_str());
        }
    }
}

impl std::ops::Deref for CompactSymbol {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for CompactSymbol {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for CompactSymbol {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for CompactSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for CompactSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl From<&str> for CompactSymbol {
    fn from(s: &str) -> Self {
        let bytes = s.as_bytes();
        if bytes.len() <= 22 {
            let mut buf = [0u8; 22];
            buf[..bytes.len()].copy_from_slice(bytes);
            Self::Inline {
                buf,
                len: bytes.len() as u8,
            }
        } else {
            Self::Heap(s.into())
        }
    }
}

impl From<String> for CompactSymbol {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<&String> for CompactSymbol {
    fn from(s: &String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<char> for CompactSymbol {
    fn from(c: char) -> Self {
        let mut buf = [0u8; 22];
        let len = c.encode_utf8(&mut buf[..4]).len() as u8;
        Self::Inline { buf, len }
    }
}

impl Default for CompactSymbol {
    fn default() -> Self {
        Self::from(" ")
    }
}

impl<T: AsRef<str> + ?Sized> PartialEq<T> for CompactSymbol {
    fn eq(&self, other: &T) -> bool {
        self.as_str() == other.as_ref()
    }
}

/// A single character on the grid: its grapheme cluster (symbol), display width, and style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub symbol: CompactSymbol,
    pub width: u8,
    pub style: Style,
    /// Foreground color (mirrors `style.fg` for ratatui `cell.fg` compatibility).
    pub fg: Color,
    /// Background color (mirrors `style.bg` for ratatui `cell.bg` compatibility).
    pub bg: Color,
}

impl Cell {
    /// A blank cell: one space column, default style. This is what an empty
    /// grid is filled with and what `clr_eol`/`clr_eos` conceptually write.
    pub fn blank() -> Self {
        Cell {
            symbol: CompactSymbol::from(" "),
            width: 1,
            style: Style::RESET,
            fg: Color::Reset,
            bg: Color::Reset,
        }
    }

    /// A blank cell pre-styled with a background — used to fill a band with a
    /// surface color (the app-bg fill, a panel background).
    pub fn blank_styled(style: Style) -> Self {
        Cell {
            symbol: CompactSymbol::from(" "),
            width: 1,
            fg: style.fg,
            bg: style.bg,
            style,
        }
    }

    /// The trailing cell of a width-2 glyph. It renders nothing of its own
    /// (the head's glyph spills into this column) but owns the head's
    /// background so the column can never ghost.
    pub fn wide_continuation(head_style: Style) -> Self {
        Cell {
            symbol: CompactSymbol::from(" "),
            width: 0,
            fg: Color::Reset,
            bg: head_style.bg,
            style: Style {
                bg: head_style.bg,
                ..Style::RESET
            },
        }
    }

    /// A narrow (width-1) glyph with a style.
    pub fn narrow(symbol: impl Into<CompactSymbol>, style: Style) -> Self {
        Cell {
            symbol: symbol.into(),
            width: 1,
            fg: style.fg,
            bg: style.bg,
            style,
        }
    }

    /// A wide (width-2) glyph with a style.
    pub fn wide(symbol: impl Into<CompactSymbol>, style: Style) -> Self {
        Cell {
            symbol: symbol.into(),
            width: 2,
            fg: style.fg,
            bg: style.bg,
            style,
        }
    }

    /// Whether this cell is the trailing half of a wide glyph.
    pub fn is_wide_continuation(&self) -> bool {
        self.width == 0
    }

    // --- ratatui-compatible mutation API (used by the primitives layer for
    //     in-place scrollbar / dim edits). ---

    /// Set the symbol, recomputing its display width.
    pub fn set_symbol(&mut self, symbol: &str) {
        self.symbol.clear();
        self.symbol.push_str(symbol);
        self.width = crate::text::grapheme_width(symbol);
    }

    /// Override the foreground.
    pub fn set_fg(&mut self, fg: Color) {
        self.style.fg = fg;
        self.fg = fg;
    }

    /// Override the background.
    pub fn set_bg(&mut self, bg: Color) {
        self.style.bg = bg;
        self.bg = bg;
    }

    /// The current symbol (ratatui-compat accessor).
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Foreground color accessor.
    pub fn fg(&self) -> Color {
        self.style.fg
    }

    /// Background color accessor.
    pub fn bg(&self) -> Color {
        self.style.bg
    }

    /// Return this cell's style (ratatui-compat method).
    pub fn style(&self) -> Style {
        self.style
    }

    /// Reset to a blank default cell (ratatui-compat).
    pub fn reset(&mut self) {
        *self = Cell::blank();
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_endpoints_are_identity() {
        let a = Color::Rgb(10, 20, 30);
        let b = Color::Rgb(200, 100, 50);
        assert_eq!(a.blend(b, 0.0), a);
        assert_eq!(a.blend(b, 1.0), b);
    }

    #[test]
    fn blend_midpoint_is_average() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(100, 200, 60);
        assert_eq!(a.blend(b, 0.5), Color::Rgb(50, 100, 30));
    }

    #[test]
    fn blend_clamps_out_of_range_t() {
        let a = Color::Rgb(10, 20, 30);
        let b = Color::Rgb(200, 100, 50);
        assert_eq!(a.blend(b, -1.0), a);
        assert_eq!(a.blend(b, 2.0), b);
    }

    #[test]
    fn blend_named_color_uses_approximate_rgb() {
        // White ≈ (255,255,255), Black ≈ (0,0,0): midpoint is (128,128,128).
        assert_eq!(
            Color::White.blend(Color::Black, 0.5),
            Color::Rgb(128, 128, 128)
        );
    }

    #[test]
    fn blend_treats_reset_as_a_hole() {
        let rgb = Color::Rgb(1, 2, 3);
        // Reset on one side returns the other color (nothing to blend with).
        assert_eq!(Color::Reset.blend(rgb, 0.5), rgb);
        assert_eq!(rgb.blend(Color::Reset, 0.5), rgb);
        // Reset on both sides stays Reset.
        assert_eq!(Color::Reset.blend(Color::Reset, 0.5), Color::Reset);
        // At the endpoints a Reset side is still honored.
        assert_eq!(Color::Rgb(9, 9, 9).blend(Color::Reset, 1.0), Color::Reset);
    }
}
