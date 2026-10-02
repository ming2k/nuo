//! Terminal protocol escape emission drivers (ADR-0180).
//!
//! # Structural Design
//!
//! Rather than mixing capability branches inside a monolithic backend, each
//! terminal protocol standard is encapsulated in a dedicated, zero-compromise
//! driver:
//!
//! - [`DirectColorDriver`]: ITU-T T.416 24-bit TrueColor, DEC Mode 2026 sync updates, full SGR.
//! - [`Ansi16Driver`]: ECMA-48 16-color ANSI output, color quantization LUT, italic-to-underline mapping.
//! - [`MonochromeDriver`]: DEC VT100 physical terminal standard for getty/serial lines.
//!   Zero color codes emitted, visual hierarchy strictly expressed via `SGR 7` (Reverse Video),
//!   `SGR 4` (Underline), and `SGR 1` (Bold).

use std::io::{self, Write};

use crossterm::{
    QueueableCommand,
    style::{Attribute, Color as CtColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate},
};

use crate::profile::{ColorStandard, TerminalProfile, quantize_to_ansi16};
use crate::{Color, Modifier, Style};

/// Trait defining the terminal escape emission contract.
pub trait EscapeEmitter {
    /// Emit escape sequences to transition from current terminal state to `want`.
    fn apply_style<W: Write>(&mut self, want: Style, out: &mut W) -> io::Result<()>;
    /// Open a synchronized update envelope (if supported).
    fn begin_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()>;
    /// Close a synchronized update envelope (if supported) and flush.
    fn end_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()>;
    /// Invalidate style tracking by resetting terminal SGR state.
    fn invalidate<W: Write>(&mut self, out: &mut W) -> io::Result<()>;
    /// Whether this driver's target terminal supports mouse capture.
    fn supports_mouse(&self) -> bool;
}

/// Dynamic driver dispatch over standard terminal capability profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalDriver {
    DirectColor(DirectColorDriver),
    Ansi16(Ansi16Driver),
    Monochrome(MonochromeDriver),
}

impl TerminalDriver {
    /// Instantiate the appropriate driver for a [`TerminalProfile`].
    pub fn for_profile(profile: &TerminalProfile) -> Self {
        match profile.color_standard {
            ColorStandard::DirectColor => Self::DirectColor(DirectColorDriver::new()),
            ColorStandard::Ansi16 => Self::Ansi16(Ansi16Driver::new(profile.supports_mouse)),
            ColorStandard::Monochrome => Self::Monochrome(MonochromeDriver::new()),
        }
    }

    /// Return the driver's color standard.
    pub fn color_standard(&self) -> ColorStandard {
        match self {
            Self::DirectColor(_) => ColorStandard::DirectColor,
            Self::Ansi16(_) => ColorStandard::Ansi16,
            Self::Monochrome(_) => ColorStandard::Monochrome,
        }
    }

    /// Whether this driver supports synchronized updates (Mode 2026).
    pub fn supports_sync_update(&self) -> bool {
        matches!(self, Self::DirectColor(_))
    }
}

impl EscapeEmitter for TerminalDriver {
    fn apply_style<W: Write>(&mut self, want: Style, out: &mut W) -> io::Result<()> {
        match self {
            Self::DirectColor(d) => d.apply_style(want, out),
            Self::Ansi16(d) => d.apply_style(want, out),
            Self::Monochrome(d) => d.apply_style(want, out),
        }
    }

    fn begin_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        match self {
            Self::DirectColor(d) => d.begin_sync(out),
            Self::Ansi16(d) => d.begin_sync(out),
            Self::Monochrome(d) => d.begin_sync(out),
        }
    }

    fn end_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        match self {
            Self::DirectColor(d) => d.end_sync(out),
            Self::Ansi16(d) => d.end_sync(out),
            Self::Monochrome(d) => d.end_sync(out),
        }
    }

    fn invalidate<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        match self {
            Self::DirectColor(d) => d.invalidate(out),
            Self::Ansi16(d) => d.invalidate(out),
            Self::Monochrome(d) => d.invalidate(out),
        }
    }

    fn supports_mouse(&self) -> bool {
        match self {
            Self::DirectColor(d) => d.supports_mouse(),
            Self::Ansi16(d) => d.supports_mouse(),
            Self::Monochrome(d) => d.supports_mouse(),
        }
    }
}

// Profile 1: DirectColorDriver (ITU-T T.416)

/// Full 24-bit TrueColor driver with DEC Mode 2026 synchronized updates and full SGR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectColorDriver {
    style: Style,
}

impl Default for DirectColorDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectColorDriver {
    pub const fn new() -> Self {
        Self {
            style: Style::RESET,
        }
    }
}

impl EscapeEmitter for DirectColorDriver {
    fn apply_style<W: Write>(&mut self, want: Style, out: &mut W) -> io::Result<()> {
        if want == self.style {
            return Ok(());
        }
        let have = self.style;
        if want.fg != have.fg {
            out.queue(SetForegroundColor(to_ct_color(want.fg)))?;
        }
        if want.bg != have.bg {
            out.queue(SetBackgroundColor(to_ct_color(want.bg)))?;
        }

        let dropped = have.add & !want.add;
        let added = want.add & !have.add;
        if !dropped.is_empty() {
            out.queue(SetAttribute(Attribute::Reset))?;
            if want.fg != Color::Reset {
                out.queue(SetForegroundColor(to_ct_color(want.fg)))?;
            }
            if want.bg != Color::Reset {
                out.queue(SetBackgroundColor(to_ct_color(want.bg)))?;
            }
            for attr in iter_attrs(want.add) {
                out.queue(SetAttribute(attr))?;
            }
        } else {
            for attr in iter_attrs(added) {
                out.queue(SetAttribute(attr))?;
            }
        }
        self.style = want;
        Ok(())
    }

    fn begin_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.queue(BeginSynchronizedUpdate).map(|_| ())
    }

    fn end_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.queue(EndSynchronizedUpdate)?;
        out.flush()
    }

    fn invalidate<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.queue(SetAttribute(Attribute::Reset))?;
        self.style = Style::RESET;
        Ok(())
    }

    fn supports_mouse(&self) -> bool {
        true
    }
}

// Profile 2: Ansi16Driver (ECMA-48)

/// 16-color ANSI driver. Zero TrueColor codes emitted; unsupported italics map to underline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ansi16Driver {
    style: Style,
    supports_mouse: bool,
}

impl Ansi16Driver {
    pub const fn new(supports_mouse: bool) -> Self {
        Self {
            style: Style::RESET,
            supports_mouse,
        }
    }
}

impl EscapeEmitter for Ansi16Driver {
    fn apply_style<W: Write>(&mut self, mut want: Style, out: &mut W) -> io::Result<()> {
        // Quantize colors to ANSI 16
        want.fg = quantize_to_ansi16(want.fg);
        want.bg = quantize_to_ansi16(want.bg);

        // Map Italic to Underline on legacy consoles
        if want.add.contains(Modifier::ITALIC) {
            want.add.remove(Modifier::ITALIC);
            want.add.insert(Modifier::UNDERLINE);
        }

        if want == self.style {
            return Ok(());
        }
        let have = self.style;
        if want.fg != have.fg {
            out.queue(SetForegroundColor(to_ct_color(want.fg)))?;
        }
        if want.bg != have.bg {
            out.queue(SetBackgroundColor(to_ct_color(want.bg)))?;
        }

        let dropped = have.add & !want.add;
        let added = want.add & !have.add;
        if !dropped.is_empty() {
            out.queue(SetAttribute(Attribute::Reset))?;
            if want.fg != Color::Reset {
                out.queue(SetForegroundColor(to_ct_color(want.fg)))?;
            }
            if want.bg != Color::Reset {
                out.queue(SetBackgroundColor(to_ct_color(want.bg)))?;
            }
            for attr in iter_attrs(want.add) {
                out.queue(SetAttribute(attr))?;
            }
        } else {
            for attr in iter_attrs(added) {
                out.queue(SetAttribute(attr))?;
            }
        }
        self.style = want;
        Ok(())
    }

    fn begin_sync<W: Write>(&mut self, _out: &mut W) -> io::Result<()> {
        Ok(()) // Synchronized update unsupported on 16-color console
    }

    fn end_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.flush()
    }

    fn invalidate<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.queue(SetAttribute(Attribute::Reset))?;
        self.style = Style::RESET;
        Ok(())
    }

    fn supports_mouse(&self) -> bool {
        self.supports_mouse
    }
}

// Profile 3: MonochromeDriver (DEC VT100 / getty)

/// DEC VT100 monochrome physical terminal driver.
///
/// Emits absolutely ZERO color escape sequences. Visual hierarchy is strictly
/// represented using physical VT100 attributes:
/// - `SGR 0`: Normal / Reset
/// - `SGR 1`: Bold / High-intensity
/// - `SGR 4`: Underline
/// - `SGR 7`: Reverse Video (canonical standard for focus, selection, and active elements)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonochromeDriver {
    style: Style,
}

impl Default for MonochromeDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl MonochromeDriver {
    pub const fn new() -> Self {
        Self {
            style: Style::RESET,
        }
    }
}

impl EscapeEmitter for MonochromeDriver {
    fn apply_style<W: Write>(&mut self, mut want: Style, out: &mut W) -> io::Result<()> {
        // Strip all color information outright
        let has_emphasis = want.bg != Color::Reset || want.add.contains(Modifier::REVERSE);
        want.fg = Color::Reset;
        want.bg = Color::Reset;

        if has_emphasis {
            want.add.insert(Modifier::REVERSE);
        }
        // VT100 does not have Dim; map Italic to Underline
        want.add.remove(Modifier::DIM);
        if want.add.contains(Modifier::ITALIC) {
            want.add.remove(Modifier::ITALIC);
            want.add.insert(Modifier::UNDERLINE);
        }

        if want == self.style {
            return Ok(());
        }

        let have = self.style;
        let dropped = have.add & !want.add;
        let added = want.add & !have.add;

        if !dropped.is_empty() {
            out.queue(SetAttribute(Attribute::Reset))?;
            for attr in iter_attrs(want.add) {
                out.queue(SetAttribute(attr))?;
            }
        } else {
            for attr in iter_attrs(added) {
                out.queue(SetAttribute(attr))?;
            }
        }

        self.style = want;
        Ok(())
    }

    fn begin_sync<W: Write>(&mut self, _out: &mut W) -> io::Result<()> {
        Ok(()) // Synchronized update unsupported on VT100/getty
    }

    fn end_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.flush()
    }

    fn invalidate<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.queue(SetAttribute(Attribute::Reset))?;
        self.style = Style::RESET;
        Ok(())
    }

    fn supports_mouse(&self) -> bool {
        false
    }
}

// Shared Helpers

/// Translate an engine [`Color`] to a crossterm color.
pub fn to_ct_color(c: Color) -> CtColor {
    match c {
        Color::Reset => CtColor::Reset,
        Color::Rgb(r, g, b) => CtColor::Rgb { r, g, b },
        Color::Black => CtColor::Black,
        Color::Red => CtColor::DarkRed,
        Color::Green => CtColor::DarkGreen,
        Color::Yellow => CtColor::DarkYellow,
        Color::Blue => CtColor::DarkBlue,
        Color::Magenta => CtColor::DarkMagenta,
        Color::Cyan => CtColor::DarkCyan,
        Color::Gray => CtColor::Grey,
        Color::DarkGray => CtColor::DarkGrey,
        Color::LightRed => CtColor::Red,
        Color::LightGreen => CtColor::Green,
        Color::LightYellow => CtColor::Yellow,
        Color::LightBlue => CtColor::Blue,
        Color::LightMagenta => CtColor::Magenta,
        Color::LightCyan => CtColor::Cyan,
        Color::White => CtColor::White,
    }
}

/// Translate modifier bits into crossterm `Attribute` in stable order.
pub fn iter_attrs(m: Modifier) -> impl Iterator<Item = Attribute> {
    let mut v = Vec::new();
    if m.contains(Modifier::BOLD) {
        v.push(Attribute::Bold);
    }
    if m.contains(Modifier::DIM) {
        v.push(Attribute::Dim);
    }
    if m.contains(Modifier::ITALIC) {
        v.push(Attribute::Italic);
    }
    if m.contains(Modifier::UNDERLINE) {
        v.push(Attribute::Underlined);
    }
    if m.contains(Modifier::REVERSE) {
        v.push(Attribute::Reverse);
    }
    if m.contains(Modifier::STRIKETHROUGH) {
        v.push(Attribute::CrossedOut);
    }
    v.into_iter()
}
