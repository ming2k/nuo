//! Buffer primitives: 2D character grid, cell representation,
//! style attributes, grapheme clustering, and glyph sets.

pub mod cell;
pub mod glyph;
pub mod grid;
pub mod text;

pub use cell::{Cell, Color, CompactSymbol, Modifier, Style};
pub use glyph::{ASCII_GLYPHS, GlyphSet, UNICODE_GLYPHS};
pub use grid::{BandRotation, Fit, Grid, Pos};
pub use text::{
    cursor_column, floor_grapheme_boundary, grapheme_width, graphemes, inclusive_grapheme_end,
    prohibited_line_end, prohibited_line_start, str_len, str_len_w, str_width, wrap,
};
