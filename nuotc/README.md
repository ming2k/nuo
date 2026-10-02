# nuotc (Nuo Terminal Canvas)

**nuotc** is a high-performance, retained-mode 2D terminal canvas and diff rendering engine for Rust.

## Why nuotc?

Most Rust terminal UI libraries (such as Ratatui) operate on an **immediate mode** model: every frame the entire widget tree is rebuilt into a back buffer, and a cell-level diff decides what bytes reach the terminal. While double-buffering optimizes transmission, it does not optimize rebuilding: layout computation, text wrapping, and widget construction run for every cell every frame even when nothing changed. Immediate mode also struggles with CJK double-width glyph trailing columns and IME composition windows, often leaving stale "ghost" blocks through multiplexers like `tmux`.

`nuotc` takes the **retained-mode Vim/Neovim ScreenGrid** approach instead:

- **Retained 2D Grid**: The `Grid` is the single source of truth for desired terminal state. Writes mark lines dirty at write time (`dirty_col` line tracking), eliminating full-frame rescans.
- **Minimal Escape-Code Diff Engine**: Each frame, `diff` compares the back grid against the front grid and emits run-length packed cell runs with SGR-merged styles and cursor jumps over unchanged cells. Unchanged lines emit zero bytes.
- **Ghost-Free CJK & IME Handling**: Trailing columns of wide glyphs are owned directly by the writer with identical background color, preventing ghost cells across all terminals and multiplexers.
- **Back-Color-Erase (BCE) Optimization**: When supported, line/region clears inherit the current background color and emit a single `clr_eol` (`\x1b[K`) escape code.
- **Built-in Flexbox Layout & Anchoring**: Includes a native Flexbox solver (`Flex`, `FlexItem`, `SolvedFlex`) and constraint-driven popup/modal anchoring (`AnchorPlacement`, `compute_anchored_rect`).
- **Zero Domain Vocabulary**: Pure terminal graphics — knows only about cells, styles, grids, flexbox, and drivers. Free of any application-specific logic.

## Architecture

`nuotc` is organized into six clear, acyclic subsystems:

- **`buffer`** (`nuotc::buffer`) — Retained 2D `Grid`, `Cell` memory layout, style attributes, grapheme clustering, and glyph sets.
- **`layout`** (`nuotc::layout`) — Geometry primitives (`Rect`), linear split constraints (`Layout`), flexbox solver (`Flex`), and anchored positioning (`compute_anchored_rect`).
- **`render`** (`nuotc::render`) — Differential rendering pipeline (`diff`), recording canvas & render node caching (`Canvas`, `DisplayList`, `RenderNode`), escape emitters, and standards-based terminal profiles (`TerminalProfile`).
- **`terminal`** (`nuotc::terminal`) — Low-level backend I/O (`Backend`), raw mode/alternate screen lifecycle, and `Frame` execution loop.
- **`widgets`** (`nuotc::widgets`) — Declarative TUI primitives (`Block`, `Paragraph`, `Line`, `Span`, `Clear`) and layout containers (`Column`, `Row`, `Stack`, `Container`, `Divider`, `Spacer`).
- **`ui`** (`nuotc::ui`) — Retained scene graph (`Scene`), component lifecycle, and event dispatch runtime (`UiRuntime`).

All common types are re-exported at the crate root for ergonomics, and historical module paths (`nuotc::diff`, `nuotc::flex`, `nuotc::backend`, etc.) are preserved for seamless compatibility.
