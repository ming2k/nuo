//! Differential rendering engine, escape sequence emitters, and terminal profiles.

pub mod diff;
pub mod driver;
pub mod profile;
pub mod record;

pub use diff::{Draw, DrawCmd, diff};
pub use driver::{
    Ansi16Driver, DirectColorDriver, EscapeEmitter, MonochromeDriver, TerminalDriver,
};
pub use profile::{
    CharsetStandard, ColorStandard, ElevationArchetype, SpatialCost, TerminalProfile,
};
pub use record::{Canvas, DisplayList, DrawOp, Rasterizer, RenderNode};
