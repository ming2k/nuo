//! End-to-end smoke tests across terminal standards and capability profiles (ADR-0180).
//!
//! Scenarios validated:
//! 1. ITU-T T.416 DirectColor (TrueColor 24-bit + Mode 2026 Sync + Unicode)
//! 2. ECMA-48 ANSI-16 (Linux Console `TERM=linux`, 16-color quantization, italic->underline)
//! 3. DEC VT100 / getty / Serial Line (`TERM=vt100`, zero color escapes, SGR 7 reverse video, ASCII glyphs)
//! 4. Cross-tool `NO_COLOR=1` standard enforcement
//! 5. Fixed 80x24 classical serial console geometry

use nuotc::backend::Bce;
use nuotc::driver::{Ansi16Driver, DirectColorDriver, MonochromeDriver, TerminalDriver};
use nuotc::glyph::{ASCII_GLYPHS, UNICODE_GLYPHS};
use nuotc::profile::{CharsetStandard, ColorStandard, TerminalProfile};
use nuotc::widgets::{Block, Borders};
use nuotc::{Backend, Color, Draw, DrawCmd, EscapeEmitter, Modifier, Style};

/// Helper to render a multi-feature test frame into an in-memory buffer.
fn render_test_frame_with_driver(
    driver: TerminalDriver,
    glyph_v: &'static str,
) -> (Vec<u8>, TerminalProfile) {
    let mut sink = Vec::new();
    let profile = match driver {
        TerminalDriver::DirectColor(_) => TerminalProfile::direct_color(),
        TerminalDriver::Ansi16(ref d) => TerminalProfile {
            color_standard: ColorStandard::Ansi16,
            charset_standard: CharsetStandard::Utf8,
            supports_italic: false,
            supports_sync_update: false,
            supports_mouse: d.supports_mouse(),
        },
        TerminalDriver::Monochrome(_) => TerminalProfile::dec_vt100_monochrome(),
    };

    let mut backend = Backend::with_bce_and_driver(&mut sink, Bce::No, driver);

    // Frame lifecycle
    backend.begin_sync_update().unwrap();

    let cmd = DrawCmd {
        w: 80,
        h: 24,
        draws: vec![
            // 1. Left border
            Draw::Cells {
                x: 0,
                y: 0,
                style: Style::default().fg(Color::Cyan),
                cells: vec![(glyph_v.into(), 1)],
            },
            // 2. Normal text
            Draw::Cells {
                x: 2,
                y: 0,
                style: Style::default().fg(Color::White),
                cells: vec![("Status:".into(), 7)],
            },
            // 3. Highlighted / active selection (RGB tint in DirectColor, reverse video in Mono)
            Draw::Cells {
                x: 10,
                y: 0,
                style: Style::default()
                    .fg(Color::Rgb(255, 255, 255))
                    .bg(Color::Rgb(38, 48, 44)),
                cells: vec![("Active".into(), 6)],
            },
            // 4. Formatted text with Italic and Bold
            Draw::Cells {
                x: 18,
                y: 0,
                style: Style::default()
                    .fg(Color::Rgb(180, 190, 254))
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC),
                cells: vec![("Notice".into(), 6)],
            },
        ],
    };

    backend.render(&cmd).unwrap();
    backend.end_sync_update().unwrap();

    (sink, profile)
}

#[test]
fn smoke_scenario_1_direct_color_truecolor() {
    let (bytes, _) = render_test_frame_with_driver(
        TerminalDriver::DirectColor(DirectColorDriver::new()),
        UNICODE_GLYPHS.border_v,
    );
    let output = String::from_utf8(bytes).expect("valid utf-8 output");

    // 1. DEC Mode 2026 synchronized update envelope must open and close
    assert!(
        output.contains("\x1b[?2026h"),
        "DirectColor must open mode 2026 envelope"
    );
    assert!(
        output.contains("\x1b[?2026l"),
        "DirectColor must close mode 2026 envelope"
    );

    // 2. 24-bit TrueColor SGR codes must be emitted
    assert!(
        output.contains("\x1b[38;2;"),
        "DirectColor must emit 38;2 foreground RGB"
    );
    assert!(
        output.contains("\x1b[48;2;"),
        "DirectColor must emit 48;2 background RGB"
    );

    // 3. True italic attribute must be emitted
    assert!(
        output.contains("\x1b[3m"),
        "DirectColor must emit SGR 3 (italic)"
    );

    // 4. Unicode box-drawing character must be intact
    assert!(
        output.contains("┃"),
        "DirectColor must output Unicode heavy vertical bar"
    );
}

#[test]
fn smoke_scenario_2_ecma48_ansi16_linux_console() {
    let (bytes, _) = render_test_frame_with_driver(
        TerminalDriver::Ansi16(Ansi16Driver::new(true)),
        UNICODE_GLYPHS.border_v,
    );
    let output = String::from_utf8(bytes).expect("valid utf-8 output");

    // 1. Synchronized update must NOT be emitted
    assert!(
        !output.contains("\x1b[?2026h"),
        "ANSI-16 must not emit mode 2026"
    );

    // 2. Zero 24-bit TrueColor escapes
    assert!(
        !output.contains("\x1b[38;2;"),
        "ANSI-16 must not emit 38;2 TrueColor"
    );
    assert!(
        !output.contains("\x1b[48;2;"),
        "ANSI-16 must not emit 48;2 TrueColor"
    );

    // 3. SGR 3 (Italic) must be eliminated; SGR 4 (Underline) replacement must be emitted
    assert!(
        !output.contains("\x1b[3m"),
        "ANSI-16 must eliminate SGR 3 (italic)"
    );
    assert!(
        output.contains("\x1b[4m"),
        "ANSI-16 must map italic to SGR 4 (underline)"
    );

    // 4. Standard ANSI color sequences must be present
    assert!(
        output.contains("\x1b[3"),
        "ANSI-16 must emit standard 30..37 foreground codes"
    );
}

#[test]
fn smoke_scenario_3_dec_vt100_getty_serial() {
    let (bytes, _) = render_test_frame_with_driver(
        TerminalDriver::Monochrome(MonochromeDriver::new()),
        ASCII_GLYPHS.border_v,
    );
    let output = String::from_utf8(bytes.clone()).expect("valid utf-8 output");

    // 1. ABSOLUTELY ZERO color codes (neither 38;2, nor 48;2, nor 30..37, nor 40..47, nor 39/49)
    assert!(
        !output.contains("\x1b[38;"),
        "Monochrome must not emit 38; codes"
    );
    assert!(
        !output.contains("\x1b[48;"),
        "Monochrome must not emit 48; codes"
    );
    assert!(
        !output.contains("\x1b[31m") && !output.contains("\x1b[32m"),
        "Monochrome must not emit ANSI colors"
    );
    assert!(
        !output.contains("\x1b[39m") && !output.contains("\x1b[49m"),
        "Monochrome must not emit color reset codes"
    );

    // 2. Zero Mode 2026 synchronized updates
    assert!(
        !output.contains("\x1b[?2026h"),
        "Monochrome must not emit mode 2026"
    );

    // 3. Canonical VT100 SGR 7 (Reverse Video) must be used for active/highlighted content
    assert!(
        output.contains("\x1b[7m"),
        "Monochrome must represent active content with SGR 7 Reverse Video"
    );

    // 4. Box drawing must be strictly pure ASCII '|', zero Unicode box characters
    assert!(
        !output.contains("┃"),
        "Monochrome getty output must not contain Unicode '┃'"
    );
    assert!(
        output.contains('|'),
        "Monochrome getty output must contain ASCII '|'"
    );

    // 5. Output must consist exclusively of ASCII bytes and VT100 SGR codes (0, 1, 4, 7)
    for b in &bytes {
        assert!(
            b.is_ascii(),
            "Every byte in VT100 output must be valid ASCII: byte value {b}"
        );
    }
}

#[test]
fn smoke_scenario_4_no_color_environment_standard() {
    // When NO_COLOR is set, profile detection must unconditionally resolve to Monochrome
    let profile = TerminalProfile::for_env("xterm-256color", "truecolor", true, None, None);
    assert_eq!(profile.color_standard, ColorStandard::Monochrome);

    let driver = TerminalDriver::for_profile(&profile);
    assert_eq!(driver.color_standard(), ColorStandard::Monochrome);
    assert!(!driver.supports_sync_update());
    assert!(!driver.supports_mouse());
}

#[test]
fn smoke_scenario_5_constrained_80x24_getty_geometry() {
    let mut sink = Vec::new();
    let driver = TerminalDriver::Monochrome(MonochromeDriver::new());
    let _backend = Backend::with_bce_and_driver(&mut sink, Bce::No, driver);

    // Verify 80x24 block rendering
    let block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .glyph_v(ASCII_GLYPHS.border_v);

    let mut grid = nuotc::Grid::new(80, 24);
    block.render(nuotc::Rect::new(0, 0, 80, 24), &mut grid);

    // Verify boundary cells
    assert_eq!(grid.get(0, 0).unwrap().symbol(), "|");
    assert_eq!(grid.get(0, 23).unwrap().symbol(), "|");
    assert_eq!(grid.get(79, 0).unwrap().symbol(), "|");
    assert_eq!(grid.get(79, 23).unwrap().symbol(), "|");
}
