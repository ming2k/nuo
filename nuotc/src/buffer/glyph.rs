//! Centralized typography and glyph set system (ADR-0180).
//!
//! # Structural Design
//!
//! Rather than hardcoding raw Unicode string literals across UI components,
//! all box-drawing, status indicators, and animated spinners are encapsulated
//! in a canonical [`GlyphSet`]. This decouples UI view code from character set
//! constraints, allowing automatic or explicit fallback to pure 7-bit US-ASCII
//! on VT100 physical terminals, serial getty consoles, and VGA console fonts.

use crate::profile::CharsetStandard;

/// A coherent suite of box-drawing characters, indicators, and animation glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphSet {
    /// Vertical border bar (e.g. `┃` or `|`).
    pub border_v: &'static str,
    /// Horizontal border bar (e.g. `─` or `-`).
    pub border_h: &'static str,
    /// Top-left corner (e.g. `╭` or `+`).
    pub corner_tl: &'static str,
    /// Top-right corner (e.g. `╮` or `+`).
    pub corner_tr: &'static str,
    /// Bottom-left corner (e.g. `╰` or `+`).
    pub corner_bl: &'static str,
    /// Bottom-right corner (e.g. `╯` or `+`).
    pub corner_br: &'static str,
    /// Activity / status indicator dot (e.g. `●` or `*`).
    pub dot: &'static str,
    /// Secondary bullet (e.g. `•` or `*`).
    pub bullet: &'static str,
    /// Affirmative / check indicator (e.g. `✓` or `[OK]`).
    pub check: &'static str,
    /// Error / cross indicator (e.g. `✗` or `[X]`).
    pub cross: &'static str,
    /// Rightward directional arrow (e.g. `→` or `->`).
    pub arrow_right: &'static str,
    /// Ellipsis truncation indicator (e.g. `…` or `...`).
    pub ellipsis: &'static str,
    /// Secret / password mask character (e.g. `•` or `*`).
    pub mask: &'static str,
    /// Animated progress / spinner frames.
    pub spinner_frames: &'static [&'static str],
}

impl Default for GlyphSet {
    fn default() -> Self {
        UNICODE_GLYPHS
    }
}

impl GlyphSet {
    /// Resolve the appropriate [`GlyphSet`] for a [`CharsetStandard`].
    pub const fn for_standard(standard: CharsetStandard) -> Self {
        match standard {
            CharsetStandard::Utf8 => UNICODE_GLYPHS,
            CharsetStandard::Ascii => ASCII_GLYPHS,
        }
    }

    /// Produce the current spinner frame for the given tick phase.
    #[inline]
    pub fn spinner_frame(&self, phase: usize) -> &'static str {
        if self.spinner_frames.is_empty() {
            "*"
        } else {
            self.spinner_frames[phase % self.spinner_frames.len()]
        }
    }

    /// Resolve the matching [`crate::widgets::BorderType`] for this glyph set.
    pub const fn border_type(&self) -> crate::widgets::BorderType {
        if self.border_v.len() == 1 && self.border_v.as_bytes()[0] == b'|' {
            crate::widgets::BorderType::Ascii
        } else {
            crate::widgets::BorderType::Plain
        }
    }
}

/// Standard modern ISO/IEC 10646 (UTF-8) glyph set.
pub const UNICODE_GLYPHS: GlyphSet = GlyphSet {
    border_v: "┃",
    border_h: "─",
    corner_tl: "╭",
    corner_tr: "╮",
    corner_bl: "╰",
    corner_br: "╯",
    dot: "●",
    bullet: "•",
    check: "✓",
    cross: "✗",
    arrow_right: "→",
    ellipsis: "…",
    mask: "•",
    spinner_frames: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
};

/// 7-bit US-ASCII safe fallback glyph set for VT100 / getty / serial consoles.
pub const ASCII_GLYPHS: GlyphSet = GlyphSet {
    border_v: "|",
    border_h: "-",
    corner_tl: "+",
    corner_tr: "+",
    corner_bl: "+",
    corner_br: "+",
    dot: "*",
    bullet: "*",
    check: "[OK]",
    cross: "[X]",
    arrow_right: "->",
    ellipsis: "...",
    mask: "*",
    spinner_frames: &["|", "/", "-", "\\"],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unicode_glyphs_resolve() {
        let g = GlyphSet::for_standard(CharsetStandard::Utf8);
        assert_eq!(g.border_v, "┃");
        assert_eq!(g.dot, "●");
        assert_eq!(g.spinner_frame(0), "⠋");
    }

    #[test]
    fn test_ascii_glyphs_resolve() {
        let g = GlyphSet::for_standard(CharsetStandard::Ascii);
        assert_eq!(g.border_v, "|");
        assert_eq!(g.dot, "*");
        assert_eq!(g.spinner_frame(0), "|");
        assert_eq!(g.spinner_frame(1), "/");
    }
}
