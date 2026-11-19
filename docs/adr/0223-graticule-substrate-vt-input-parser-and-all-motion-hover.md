---
id: ADR-0223
title: "Migrate Terminal Substrate to Graticule (`graticule-canvas` / `graticule-tty`), Replace Crossterm Unix Parser, and Enable All-Motion Hover"
status: accepted
date: 2026-11-19
scope: workspace/substrate, terminal/graticule, shell/input, shell/event_loop
superseded_by: null
supersedes: [ADR-0003]
negative_knowledge: true
---

# 0223. Migrate Terminal Substrate to Graticule (`graticule-canvas` / `graticule-tty`), Replace Crossterm Unix Parser, and Enable All-Motion Hover

- Status: Accepted
- Date: 2026-11-19
- Supersedes: [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md)

## Context and Problem Statement

`nuotc` previously covered only the output rendering canvas (`CellGrid`, `diff`, `Backend`, `widgets`, `ui`), while `nuo` carried raw-mode setup (`shell/terminal.rs`), SGR mouse tracking (`shell/mouse_mode.rs`), and `crossterm::event` input reading (`shell/event_loop/input_reader.rs`) alongside a downstream `SgrLeakGuard` (`shell/input/sgr.rs`).

Three defects forced a clean break:
1. **Drifted contract in ADR-0003**: ADR-0003 §1 claimed `nuotc`'s `terminal` module owned crossterm raw-mode management and event polling, whereas the crate actually owned neither.
2. **Crossterm Unix parser leaks**: `crossterm 0.29` treats `read_count == 1024` as the sole indicator of trailing escape bytes (`mio.rs`), swallows the second `\x1b` in `\x1b\x1b`, wedges on unrecognized `ESC[?...` replies, and clears its buffer on parse errors. Splitting an SGR mouse sequence after `\x1b` emitted a spurious `KeyCode::Esc` followed by printable `[<...M` garbage into the composer. Even with `SgrLeakGuard`, `Esc` still had to be accepted to keep double-`Esc` interrupt alive, and `?1003h` all-motion hover had to be disabled.
3. **Unnecessary hover redraw & completion cost**: `process_one_event` unconditionally set `input_redraw_pending = true` and ran `app.completions()` twice even on no-op `MouseEventKind::Moved` events.

## Architectural Decisions

1. **Rename `nuotc` to `Graticule` and split into role-named layers**:
   - **`graticule-canvas`**: pure-memory cell canvas, `Flex` layout, `diff`, `Driver`, `TerminalProfile`, `widgets`, `ui`, and output `Backend`/`Frame`/`Terminal`. Keeps `#![deny(unsafe_code)]`, zero OS session dependencies, and zero stdin polling. Pure presentation crates (`ui-primitives`, `view`) depend strictly on `graticule-canvas`.
   - **`graticule-tty`**: physical TTY host transport — Graticule-owned `Event`/`KeyEvent`/`MouseEvent` types (zero `crossterm` exposure in public API), incremental timeout-aware `VtParser` (20 ms `ESC` disambiguation, double-`Esc` preservation, bounded CSI/paste buffers, ECMA-48 recovery, SGR-1006 mouse, Kitty CSI-u, bracketed paste, UTF-8), `MouseMode` (`None`, `Button`, `AllMotion`), single-reader `InputReader` (`libc::poll` + `SIGWINCH` self-pipe without `O_NONBLOCK` on `STDIN_FILENO`, `ProbeHandle` for off-frame keyboard enhancement queries, private `#[cfg(windows)]` fallback), and opt-in `TerminalSession` RAII guard.
   - **`graticule`**: single umbrella crate re-exporting `graticule-canvas` and (by default) `graticule-tty`.
2. **Break clean in `nuo`**:
   - Delete `shell/mouse_mode.rs`, `shell/input/sgr.rs` (`SgrLeakGuard`), and `SgrReassemblySink`.
   - Migrate `nuo` from `crossterm::event` and `nuotc` directly to `graticule` (keeping `[INV-ARCH-CLI-LEAN-LINK]` intact with `graticule = { optional = true }` behind `interactive`).
   - Update presentation firewalls (`nuo::dev::firewall`, `nuo::dev::color_purity`) and documentation governance (`[INV-DOC-03]`) to forbid `graticule-tty` / `graticule` in `ui-primitives` and `view` and retire `nuotc`.
3. **Enable `MouseMode::AllMotion` (`?1003h` + `?1006h`) with hover coalescing and short-circuiting**:
   - Coalesce consecutive `MouseEventKind::Moved` events within each drained batch in `event_loop/mod.rs`.
   - Skip `app.completions()` and `anchor_completion_selection` on `MouseEventKind::Moved`, and set `input_redraw_pending = true` only when `(config_hover_index, hovered_step)` actually changes.

## Invariants

- **`[INV-GRATICULE-01]` Zero Domain Vocabulary**: Neither `graticule-canvas` nor `graticule-tty` imports or defines AI agent vocabulary.
- **`[INV-GRATICULE-02]` Pure Canvas Isolation**: `ui-primitives` and `view` depend on `graticule-canvas` only, never `graticule-tty`, `graticule`, or `crossterm`.
- **`[INV-GRATICULE-03]` Single-Reader Input & Time-Based ESC Disambiguation**: Unix TTY input uses one reader thread and a 20 ms `ESC` deadline rather than read-size heuristics.

## Consequences & Trade-offs

- **Positive**: Eliminates SGR split leaks and ghost `Esc` events structurally; unlocks `?1003h` hover highlights on settings rows and collapsed step summaries without redrawing on idle mouse travel; drops direct `crossterm` dependency from `nuo`.
- **Negative & Incurred Costs**: Cross-repo changes to Graticule follow the independent substrate release cycle (published to crates.io as `graticule = "0.0.1"` / `graticule-canvas = "0.0.1"`, with optional local `.cargo/config.toml` `[patch.crates-io]` override during development), preserving `[INV-WS-01]`.

## Discarded Alternatives (Negative Knowledge)

- **Keeping `SgrLeakGuard` + `?1002h`**: Cannot eliminate the ghost `Esc` emitted before `[<...M`, blocks `?1003h` hover, and perpetuates crossterm's Unix parser bugs.
- **Wrapping `crossterm::event` types in `graticule`'s public API**: Preserves coupling to a third-party crate's types and blocks an eventual clean removal of `crossterm` on Windows.

## Links

- [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md)
- [ADR-0187](0187-documentation-governance-is-mechanical.md)
