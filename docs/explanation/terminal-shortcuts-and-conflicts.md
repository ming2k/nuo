# Terminal Shortcuts, Escape Sequences, and Host Conflicts

This article explains the design philosophy, architectural trade-offs, and physical constraints behind keyboard shortcuts in the `nuo` terminal interface. It catalogs the conflict domains inherent to terminal emulators, POSIX line disciplines, and modern operating systems, providing operators and developers with a clear mental model for diagnosing keyboard behavior.

For the factual input pipeline reference and action dispatch semantics, consult the [TUI Interaction Reference](../reference/tui/interaction.md).

---

## 1. The Terminal Input Dilemma

In traditional desktop GUI applications, the operating system dispatches discrete window events (`KeyDown`, `KeyUp`) with explicit modifier flags (`Ctrl`, `Alt`, `Meta`, `Shift`). In contrast, a Terminal User Interface (TUI) operates inside a terminal emulator communicating across a character stream (standard input `/dev/tty`).

This abstraction creates a fundamental tension for AI agent tools:

1. **Persistent Text Editing**: Nuo features a rich text composer where users compose detailed prompts, multiline code snippets, and instructions. Standard cursor movement and line editing must remain uncompromised.
2. **Sovereign Workspace Navigation**: Nuo manages multiple thread tabs, full-screen orchestration dashboards, and settings panes.
3. **Fifty Years of Terminal Legacy**: Every keystroke travels through decades of POSIX terminal protocols, kernel line disciplines, and emulator-specific key bindings.

Designing shortcuts requires navigating four distinct collision domains.

---

## 2. The Four Collision Domains

```text
┌─────────────────────────────────────────────────────────────────┐
│ Domain 1: Operating System & Host Window Manager Interception   │
│ (Alt-Tab, Alt-F4, Ctrl-Shift-C, macOS Option composition)       │
├─────────────────────────────────────────────────────────────────┤
│ Domain 2: Kernel TTY Line Discipline & POSIX Signals            │
│ (Ctrl-C SIGINT, Ctrl-Z SIGTSTP, Ctrl-S/Q flow, Ctrl-O VDISCARD) │
├─────────────────────────────────────────────────────────────────┤
│ Domain 3: POSIX Readline & In-Buffer Line Editing               │
│ (Ctrl-A/E home/end, Ctrl-W kill-word, Ctrl-U/K kill-line)       │
├─────────────────────────────────────────────────────────────────┤
│ Domain 4: VT100 Escape Sequence Timing & Ambiguity              │
│ (Alt-X physically transmitted as ESC + 'x')                     │
└─────────────────────────────────────────────────────────────────┘
```

### Domain 1: Kernel TTY Line Discipline & Signals

Before a keystroke reaches an application, the operating system's kernel terminal driver (`termios`) processes the input stream unless disabled by raw mode:

- **`Ctrl-C` (`0x03`, `ETX` / `SIGINT`)**: Sends an immediate interrupt signal to the process group.
- **`Ctrl-Z` (`0x1A`, `SUB` / `SIGTSTP`)**: Suspends the foreground process and returns to the parent shell.
- **`Ctrl-\` (`0x1C`, `FS` / `SIGQUIT`)**: Triggers an abnormal termination with a core dump.
- **`Ctrl-S` / `Ctrl-Q` (`0x13` / `0x11`, `XON` / `XOFF`)**: Software flow control. Historically used to pause slow teletype printers. In modern terminals, pressing `Ctrl-S` freezes screen output entirely, appearing as an application lockup until `Ctrl-Q` unfreezes it.
- **`Ctrl-O` (`0x0F`, `SI` / `VDISCARD`)**: In traditional BSD and POSIX terminal drivers (`c_cc[VDISCARD]`), toggles output discarding. While modern raw mode disables extended input processing (`IEXTEN`), lingering terminal layers or intermediate serial multiplexers can still mishandle `Ctrl-O`.

### Domain 2: POSIX Readline & Text Composer Collisions

When an AI terminal client presents an active composer input box, user expectations are governed by GNU Readline and standard shell editing conventions:

- **`Ctrl-A` / `Ctrl-E`**: Jump to line start / line end.
- **`Ctrl-W`**: Backward kill word (delete preceding word).
- **`Ctrl-U`**: Kill from cursor to line start (or entire line).
- **`Ctrl-K`**: Kill from cursor to line end.
- **`Ctrl-Y`**: Yank (paste previously killed text).
- **`Alt-B` / `Alt-F`**: Jump backward / forward one word.
- **`Alt-D`**: Kill next word.

**The `Ctrl-W` Trap**: In web browsers and GUI operating systems, `Ctrl-W` closes the active tab or window. In terminal interfaces, mapping `Ctrl-W` to "Close Tab" is a severe defect: an operator attempting to delete a mistyped word will inadvertently destroy their running session tab.

### Domain 3: Host Window Manager and Emulator Interceptions

The host terminal emulator (iTerm2, Alacritty, Kitty, Windows Terminal, GNOME Terminal) and the desktop environment intercept keystrokes before they enter the TTY stream:

- **Clipboard Chords**: `Ctrl-Shift-C` and `Ctrl-Shift-V` (Linux terminals) or `Cmd-C` and `Cmd-V` (macOS) are consumed by the emulator for host clipboard synchronization.
- **Window Management**: `Alt-F4` (close window on Windows/Linux), `Alt-Tab` (switch task), and `Alt-Space` (window operations menu) never reach terminal stdin.
- **Tab Switching in Emulators**: Many terminal emulators bind `Ctrl-Tab` or `Ctrl-PageUp`/`PageDown` to switch emulator tabs, making them invisible to internal TUI clients.

### Domain 4: The macOS Option Key and VT100 ESC Ambiguity

Two major hardware and protocol factors complicate modifier handling:

#### The macOS Dead Key / Diacritics Composer
By default, macOS treats the `Option` (Alt) key as an accent and symbol composer rather than a Meta modifier:
- `Option-w` emits `∑` (mathematical sum).
- `Option-c` emits `ç` (c-cedilla).
- `Option-d` emits `∂` (partial differential).
- `Option-t` emits `†` (dagger).
- `Option-e` emits `´` (dead key waiting for the next vowel).

Unless an operator explicitly configures their terminal emulator to treat the Left Option key as `Esc+` (Meta), pressing `Alt` chords in a text field inserts unicode symbols rather than dispatching application commands.

#### The VT100 Escape Prefix Ambiguity
In the standard ANSI/VT100 terminal wire protocol, `Alt-X` is not sent as an atomic packet; it is sent as two sequential bytes:
```text
\x1b (ASCII 27 / ESC)  followed by  'x' (ASCII 120)
```
The terminal parser cannot distinguish purely from byte values whether the user:
1. Pressed `Alt-X` simultaneously, or
2. Pressed the standalone `Esc` key to cancel an overlay, followed 40 milliseconds later by typing `x`.

TUI runtimes resolve this using an escape timeout (typically 25ms–50ms). Over high-latency SSH connections, packet fragmentation can split these bytes, turning an `Alt` navigation chord into an accidental cancel followed by text insertion.

---

## 3. The Orthogonal Layering Architecture

To resolve these contradictions without degrading either text editing or application navigation, Nuo partitions its keyboard space into four strictly isolated layers:

```text
┌─────────────────────────────────────────────────────────────────┐
│ Layer 0: System Lifeline & Emergency Interrupts                 │
│   Ctrl-C (SIGINT / abort turn)  |  Esc (unwind modal / cancel)  │
├─────────────────────────────────────────────────────────────────┤
│ Layer 1: In-Buffer Readline Integrity (Free-Text Composer)      │
│   Ctrl-A/E (home/end), Ctrl-W (kill word), Ctrl-U/K (kill line) │
│   ※ Rule: Application actions NEVER steal these chords          │
├─────────────────────────────────────────────────────────────────┤
│ Layer 2: Viewport & Workspace Navigation (Alt & Function Keys)  │
│   Alt-1..9 (jump tab), Alt-[ / Alt-] (history back/forward)     │
│   Alt-w (close tab), F1 (help & keybindings)                    │
├─────────────────────────────────────────────────────────────────┤
│ Layer 3: Universal Fallback (First-Class Slash Commands)       │
│   /dashboard, /settings, /close, /new, /help, /commands         │
│   ※ Rule: 100% immune to terminal emulator and Option quirks   │
└─────────────────────────────────────────────────────────────────┘
```

### Architectural Principles

1. **Composer Purity**: When the composer has focus, bare printable characters (`t`, `w`, `d`, `s`, `p`) insert text. They are never hijacked for top-level navigation.
2. **`Alt` for Window Management**: Window, tab, and view switching are mapped to `Alt` combinations (`Alt-1..9`, `Alt-w`, `Alt-[`, `Alt-]`) to keep them completely orthogonal to Readline's `Ctrl` editing matrix.
3. **Preserving Word-Skipping (`[INV-NAV-03]`, ADR-0092)**:
   - Early versions considered `Alt-Left` / `Alt-Right` for tab switching. This was permanently retired because standard terminals map `Alt-Left`/`Alt-Right` to `backward-word`/`forward-word`.
   - Tab history navigation was therefore standardized on `Alt-[` and `Alt-]`, fully protecting word skipping in long prompts.
4. **Slash Commands as Sovereign Escapes**: Every viewport navigation and workspace operation has a dedicated slash command (`/dashboard`, `/settings`, `/close`, `/new`). If an operator is working over an iPad SSH client or an unconfigured macOS terminal where `Alt` keys are captured, slash commands provide an unbreakable fallback.

---

## 4. Key Decision Matrix

| Key Chord | Canonical Function | Conflict / Trade-off Addressed | Architectural Rationale |
| :--- | :--- | :--- | :--- |
| **`Ctrl-C`** | Interrupt running turn / cancel | Kernel `SIGINT` standard | Universal terminal convention across 50 years. Never reassigned. |
| **`Esc`** | Dismiss modal / step back | Top-level cancellation | Universal escape hatch. Never used for destructive scene leaves (`[INV-TUI-CLEAN-02]`). |
| **`F1` / `Alt-h`** | Sovereign Help Dialog | Composer collision | `F1` provides zero-conflict discovery; `Alt-h` provides home-row ergonomics (ADR-0131). |
| **`Alt-w`** | Close active tab | Avoids `Ctrl-W` word-kill | Keeps `Ctrl-W` intact for backward word deletion in prompts. |
| **`Alt-1` .. `Alt-9`** | Jump to tab index | Number row ergonomics | Avoids collision with text typing while enabling instant random tab access. |
| **`Alt-[` / `Alt-]`** | Tab history back / forward | Avoids `Alt-Left/Right` word skip | Protects word-skipping in composer while giving symmetric navigation (ADR-0092). |
| **`/dashboard`** | Open / switch to Dashboard | Zero shortcut dependence | Guaranteed routing regardless of terminal emulator key bindings. |
| **`/settings`** | Open / switch to Settings | Zero shortcut dependence | Clean text command avoiding keyboard mapping ambiguities. |
| **`/close`** | Close current tab | Terminal emulator fallback | Textual equivalent of `Alt-w` for restricted keyboards. |

---

## 5. Terminal Emulator Configuration Guide

If modifier chords (`Alt-w`, `Alt-[`, `Alt-1..9`) fail to dispatch commands or produce accented characters, your terminal emulator is not configured to forward the Option key as Meta. Follow these configurations:

### macOS Terminal.app
1. Open **Settings** (`Cmd-,`).
2. Navigate to **Profiles** → **Keyboard**.
3. Check the box: **"Use Option as Meta key"**.

### iTerm2
1. Open **Settings** (`Cmd-,`).
2. Navigate to **Profiles** → **Keys** → **General**.
3. Set **Left Option key** to: **`Esc+`**.
4. (Optional) Set **Right Option key** to: **`Normal`** (retains the ability to type third-level symbols like `€`).

### Alacritty
In `alacritty.toml`:
```toml
[window]
option_as_alt = "Both" # or "OnlyLeft"
```

### Kitty
In `kitty.conf`:
```conf
macos_option_as_alt yes
```

### WezTerm
In `.wezterm.lua`:
```lua
local config = wezterm.config_builder()
config.send_composed_key_when_left_alt_is_pressed = false
return config
```

### Remote SSH & Mobile Environments
When accessing Nuo over constrained environments (e.g., Mosh, web terminals, tablet keyboards):
- Use **Slash Commands** (`/dashboard`, `/settings`, `/close`, `/new`, `/commands`).
- Press **`F1`** at any time to inspect active contextual shortcuts and client domain actions.
