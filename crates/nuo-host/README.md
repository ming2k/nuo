# nuo-host

Host machine execution environment, platform abstraction layer (PAL), sandboxing, and canonical host tools for Nuo.

## Overview

`nuo-host` provides low-level operating system integration, process tree supervision, filesystem containment, and native capability tools conforming to `nuo-tool::Tool`. It ensures all host interactions adhere to strict fail-closed security and cross-platform fidelity across Linux, macOS, and Windows.

## Capabilities

- **`tools`**: Canonical host OS capability tools:
  - `read_text`: Paginated file reader with line-offset navigation.
  - `write_file`: Atomic file writer with parent directory auto-creation.
  - `edit_text`: Surgical exact-match text replacer with optimistic concurrency checks.
  - `list_dir`: Directory child enumerator.
  - `find_files`: Glob-based recursive file finder with `.gitignore` filtering.
  - `search_text`: High-speed regex and literal text search.
  - `execute_command`: Bounded asynchronous shell command execution with StreamGuard flood protection.
- **`workspace_sandbox`**: Multi-driver workspace isolation HAL ensuring operations remain strictly confined within project boundaries.
- **`process` & `supervised`**: Kernel-enforced process tree containment (Unix process groups and Windows Job Objects with `KILL_ON_JOB_CLOSE`).
- **`paths`**: Standardized directory resolution compliant with Linux XDG, macOS standard paths, and Windows Known Folders.
- **`ipc`**: Local inter-process communication over Unix Domain Sockets and Windows Named Pipes.
- **`lock` & `secure_file`**: Advisory cross-process file locking (`flock` / `LockFileEx`) and secure atomic file creation (`0600`/`0700` DACL).
- **`clipboard`**: Async system clipboard reader and writer supporting Wayland, X11, macOS AppKit, Windows, and OSC 52 terminal sequences.
- **`opener`**: Cross-platform URL and document opener with OSC 8 terminal hyperlink fallbacks in headless environments.
- **`shell`**: Dialect-aware shell invocation (`sh`/`bash`/`pwsh`) and UTF-8 console output enforcement.
