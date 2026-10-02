//! In-house terminal rendering engine: a retained-mode cell grid with
//! write-marks-dirty tracking, a back/front grid diff, and a crossterm
//! backend that emits the minimal escape-code delta per frame.
//!
//! # Why this exists (ADR-0038)
//!
//! ratatui's model is *immediate mode*: every frame the entire UI is rebuilt
//! into a back buffer, then a cell-level diff against the previous frame
//! decides what bytes reach the terminal. That double-buffering optimizes
//! *transmission* but not *rebuilding* — layout, wrapping, and widget
//! construction still run for every cell every frame, even when nothing
//! changed. It also represents a double-width (CJK) glyph as a head cell plus
//! a trailing cell that it `reset()`s to `Color::Reset`, and its diff never
//! re-emits that trailing column, so through a multiplexer (tmux) the glyph's
//! background spillover goes stale and shows up as gray "ghost" blocks.
//!
//! This engine takes the vim/nvim approach instead:
//!
//! - A retained [`Grid`] is the single source of truth for what the
//!   application wants on screen. Writes mark the touched line dirty
//!   ([per-line `dirty_col`], vim's `ScreenGrid` model) at write time — no
//!   full-frame rescan.
//! - Each frame, [`diff`][crate::render::diff::diff] compares the back grid (desired) against the
//!   front grid (what the terminal currently shows) and emits a stream of
//!   [`Draw`] commands: run-length packed cell runs with SGR-merged styles
//!   and cursor jumps over unchanged cells. Unchanged lines emit nothing.
//! - The application owns cell contents directly: a wide glyph's trailing
//!   column is filled with the glyph's own background by the writer, so
//!   ghost cells cannot occur regardless of terminal or multiplexer.
//! - When the terminal advertises `bce` (back-color-erase), line/region
//!   clears inherit the current background and the backend emits a single
//!   `clr_eol` (`\x1b[K`) instead of writing per-cell spaces — the cheap
//!   path vim and tmux both take.
//!
//! The engine is intentionally free of any application vocabulary: it knows
//! about cells, styles, and grids, never about transcripts, messages, or
//! tool steps. That keeps it independently testable (feed two grids, assert
//! the diff) and reusable.
//!
//! # Architecture Overview
//!
//! The engine is structured into 6 decoupled, highly cohesive subsystems:
//!
//! - **[`buffer`]**: 2D discrete cell buffer ([`Cell`], [`Grid`]), styling ([`Style`], [`Color`]), text metrics, and glyphs.
//! - **[`layout`]**: Spatial layout engines — geometry ([`Rect`], [`Layout`]), flexbox ([`Flex`]), and anchoring ([`compute_anchored_rect`]).
//! - **[`render`]**: Retained differential rendering pipeline ([`diff`]), recording canvas ([`Canvas`], [`RenderNode`]), terminal escape drivers, and capability profiles.
//! - **[`terminal`]**: Terminal lifecycle, raw-mode backend ([`Backend`]), and [`Frame`] execution.
//! - **[`widgets`]**: Declarative UI primitives ([`Block`], [`Paragraph`], [`Clear`], [`Line`], [`Span`]) and layout composition ([`Column`], [`Row`], [`Stack`], [`Container`]).
//! - **[`ui`]**: Retained scene graph ([`ui::Scene`]) and component lifecycle runtime ([`ui::UiRuntime`]).
//!
//! [per-line `dirty_col`]: Grid
//! [`diff`]: render::diff::diff
//! [`Draw`]: Draw

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

// Primary domain subsystems
pub mod buffer;
pub mod layout;
pub mod render;
pub mod terminal;
pub mod ui;
pub mod widgets;

// Module aliases for seamless backward compatibility
pub use crate::buffer as core;
pub use crate::buffer::cell;
pub use crate::buffer::glyph;
pub use crate::buffer::grid;
pub use crate::buffer::text;
pub use crate::layout::anchor;
pub use crate::layout::flex;
pub use crate::render::diff;
pub use crate::render::driver;
pub use crate::render::profile;
pub use crate::render::record;
pub use crate::terminal::backend;
pub use crate::terminal::frame;

// Re-export buffer primitives
pub use crate::buffer::{
    ASCII_GLYPHS, Cell, Color, CompactSymbol, Fit, GlyphSet, Grid, Modifier, Pos, Style,
    UNICODE_GLYPHS,
};

// Re-export layout primitives
pub use crate::layout::{
    AlignItem, AnchorAlignment, AnchorConstraints, AnchorPlacement, AnchorTarget, AnchoredBox,
    Basis, Constraint, Direction, Flex, FlexDirection, FlexItem, Justify, Layout, Margin, Rect,
    SolvedFlex, compute_anchored_rect,
};

// Re-export render primitives
pub use crate::render::{
    Ansi16Driver, Canvas, CharsetStandard, ColorStandard, DirectColorDriver, DisplayList, Draw,
    DrawCmd, DrawOp, ElevationArchetype, EscapeEmitter, MonochromeDriver, Rasterizer, RenderNode,
    SpatialCost, TerminalDriver, TerminalProfile,
};

// Re-export terminal primitives
pub use crate::terminal::{Backend, Bce, CursorState, Frame, Terminal, TestTerminal, Widget};

// Re-export widgets primitives
pub use crate::widgets::{
    Alignment, AnyWidget, Block, BorderType, Borders, BoxedWidget, Clear, Column, Container,
    Divider, Line, Padding, Paragraph, Row, Spacer, Span, Stack, WidgetExt, Wrap,
};
