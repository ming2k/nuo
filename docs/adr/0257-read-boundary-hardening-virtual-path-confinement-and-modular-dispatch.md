---
id: ADR-0257
title: "Read Boundary Hardening, Virtual-Path Confinement, and Modular Content Dispatch"
status: accepted
date: 2026-11-27
scope: tools/filesystem, tool/vfs, engine/context, security/confinement
superseded_by: null
amends:
  - "[ADR-0176](0176-read-is-one-capability-with-bounded-content-dispatch.md) (codifies the single-object content-rendering boundary, abolishes hidden tail caps, adds magic-byte binary diagnostics, and upgrades contract wording)"
  - "[ADR-0193](0193-pdf-reads-are-bounded-text-extraction-through-a-host-document-seam.md) (caches extracted PDF text by file version and maps line windows to PDF page spans in headers)"
  - "[ADR-0196](0196-model-paths-are-confined-at-the-engine-gate.md) (closes virtual-path parent-traversal bypass and confines physical spool fallback to owned spool roots)"
  - "[ADR-0213](0213-read-is-file-only-and-find-files-owns-directory-discovery.md) (rejects `pages` on non-PDF files instead of silently ignoring it and removes stale directory wording)"
negative_knowledge: true
---

# 0257. Read Boundary Hardening, Virtual-Path Confinement, and Modular Content Dispatch

- **Status**: Accepted
- **Date**: 2026-11-27
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0176](0176-read-is-one-capability-with-bounded-content-dispatch.md), [ADR-0193](0193-pdf-reads-are-bounded-text-extraction-through-a-host-document-seam.md), [ADR-0196](0196-model-paths-are-confined-at-the-engine-gate.md), [ADR-0213](0213-read-is-file-only-and-find-files-owns-directory-discovery.md)
- **Related**: [ADR-0064](0064-orthogonal-agent-tool-surface-meta-tool-pruning-and-mutation-topology.md), [ADR-0209](0209-read-pages-run-off-thread.md), [ADR-0216](0216-the-prompt-branches-on-declared-tool-capability-not-tool-names.md)

---

## Context and Problem Statement

[ADR-0176](0176-read-is-one-capability-with-bounded-content-dispatch.md), [ADR-0193](0193-pdf-reads-are-bounded-text-extraction-through-a-host-document-seam.md), and [ADR-0213](0213-read-is-file-only-and-find-files-owns-directory-discovery.md) converged `read` into a single file-only capability dispatching over UTF-8 text, images, and text-layer PDFs. An architectural review of the resulting surface and implementation uncovered six concrete defects:

1. **Virtual-path confinement bypass**: `tool::is_virtual_path` checked raw string prefixes without rejecting `..` traversal segments or enforcing a trailing separator on `spool_root_dir()`. Because the engine's confinement gate ([ADR-0196](0196-model-paths-are-confined-at-the-engine-gate.md)) skips workspace checks whenever `is_virtual_path` returns `true`, and `ReadTool` fell back to raw `Path::new(raw_path)` on a resolver miss, a path such as `/tmp/nuo/spool/../../../../etc/passwd` or `.nuo/virtual/../../../../etc/passwd` bypassed workspace confinement, and relative logical prefixes resolved against the server process working directory.
2. **Hidden tail cap and refusal on large files**: Negative offsets were silently clamped by an undocumented `TAIL_LINES = 100` constant (and `page_from_text` omitted the 50 KiB byte bound on tails), while files larger than `MAX_READ_SCAN_BYTES` (64 MiB) refused tail reads outright — rejecting the exact "show the end of this large log" use case.
3. **Silent parameter coercion and ignored format arguments**: `offset = 0` was silently coerced to `1`, and `pages` was silently ignored for text, images, and virtual paths, hiding caller mistakes instead of teaching the contract.
4. **Redundant I/O and re-parsing**: `read_path_sync` hashed every file `<= 1 MiB` via `file_version` before classifying its content, causing small text files to be read up to three times from disk and causing images and rejected binaries to pay unused SHA-256 reads. Paginating through an extracted PDF re-read and re-parsed the entire PDF on every turn, and headers did not map the returned line window back to PDF page numbers.
5. **Under-specified contract wording and monolithic implementation**: `ReadTool::description` omitted line numbering, the 50 KiB / 2000-char caps, `version` tokens for `edit_text`/`write_file`, virtual spool paths, supported image formats, and the no-OCR PDF boundary; stale directory wording remained in `read.rs`; and all six sub-concerns lived in one 1,424-line file alongside `file_version` and a non-LRU `LineIndexCache`.
6. **System prompt capability drift**: `CodingBaselineGuidance` unconditionally instructed models on `todo` even when `BuiltinTool::Todo` was absent from the role surface (contradicting [ADR-0216](0216-the-prompt-branches-on-declared-tool-capability-not-tool-names.md)), and `ExplorationProtocolGuidance` advertised `CodeQuery` as `(outline, symbol, references)` instead of its real modes `(outline, symbol, find)`.

---

## Architectural Decisions

### 1. Explicit Scope Rule for `read`

We codify the single rule against which every future proposal for `read` is judged:

> `read` returns the content of one named object, bounded and rendered for the model. It never discovers, searches, filters, transforms, batches, or fetches.

Text, images, and text-layer PDFs remain unified under `read` because they are renderings of the same single-object operation. The schema stays frozen at four parameters (`path`, `offset`, `limit`, `pages`); format-specific query languages (spreadsheets, SQLite, OCR, symbol search, globbing, batching) are permanently out of scope.

### 2. Virtual-Path Confinement (`[INV-READ-VFS-01]`)

- `tool::is_virtual_path` rejects NUL bytes, rejects any `..` path segment, and matches `spool_root_dir()` with a mandatory path separator boundary.
- `tool::resolve_physical_spool_path` is the sole physical fallback when `VirtualFileResolver` misses: it admits only absolute paths whose canonicalized target is a regular file strictly inside a canonicalized owned spool root (`spool_root_dir()`, `/dev/shm/nuo/spool`, `/tmp/nuo/spool`). Logical prefixes (`.nuo/virtual/...`, `.virtual/...`, `/virtual/...`, `virtual://...`) never fall back to the process working directory.

### 3. Honest Tail Reads, Strict Argument Validation, and Binary Diagnostics

- **No hidden tail cap**: `TAIL_LINES` is deleted (clean break). A negative `offset = -N` requests `min(|N|, limit)` lines bounded by `MAX_READ_LINES` (2,000) and `MAX_READ_BYTES` (50 KiB) across both in-memory and streaming paths.
- **Reverse-seek tails for large files**: Files exceeding 4 MiB satisfy negative offsets by seeking backwards from EOF in 64 KiB chunks under the scan budget (and unbroken single-line files stop after 256 KiB so peak memory stays bounded). When the scan stops before byte 0, `ToolOutput::Code::start_line` is `0` (unknown absolute line) and the header reports `[Last N lines of ~X MiB from \`path\`]`.
- **Strict argument validation (clean break)**: `offset = 0` and `limit = 0` are rejected with explicit errors, and `pages` is rejected with a typed `ToolError` on any non-PDF file (text, image, or virtual path).
- **Magic-byte binary diagnostics**: Binary rejections identify common formats (UTF-16 BOM, SQLite, ZIP/Office/JAR, gzip/zstd/xz/bzip2/TAR, ELF, Mach-O, PE, WebAssembly, fonts, audio), report human-readable file size and accurate location (prefix, line number, or near EOF), and suggest an appropriate inspection command.

### 4. Single-FD Bounded I/O, Versioned PDF Cache, and Modular Decomposition

- `crates/tool-fs/src/version.rs` owns `file_version` and `file_version_from_bytes` as shared primitives across `read`, `edit_text`, and `write_file`.
- `crates/tool-fs/src/read.rs` is decomposed into `crates/tool-fs/src/read/{mod.rs, text.rs, line_index.rs, image.rs, document.rs}`.
- Single open file descriptor + bounded reads (`take(cap + 1)`) eliminate TOCTOU unbounded reads across text, image, and PDF paths. Files `<= 256 KiB` are read once into memory for sniffing, optional SHA-256 hashing, and rendering; images and binary failures never compute SHA-256; virtual-path physical spool resolution runs off the async worker via `spawn_blocking`.
- `LineIndexCache` and `DocumentTextCache` lexically normalize path keys (so `./a.pdf` and `a.pdf` share one slot) and use true LRU eviction. `DocumentTextCache` caches extracted PDF text in `SystemToolContext` keyed by `(path, version, page_range)` (at most 4 documents and 16 MiB total), and PDF headers map the active line window to its inclusive page span (e.g. `showing page 1` / `showing pages 1-2`).

### 5. Self-Describing Tool Contract & Capability-Gated Prompt Alignment

- `ReadTool::description` and `ReadArgs` descriptions state the full operational contract concisely (line numbering, 2000-line / 50 KiB / 2000-char bounds, `version` token handoff to `edit_text`/`write_file`, virtual paths, image MIME allowlist, no-OCR PDF extraction, negative-offset tail semantics, and strict `pages` rejection on non-PDFs).
- `CodingBaselineGuidance` drops the unconditional `todo` rule; `ExplorationProtocolGuidance` gates it on `surface.has_native(BuiltinTool::Todo)` and corrects `CodeQuery` modes from `(outline, symbol, references)` to `(outline, symbol, find)`.

---

## Invariants

- **`[INV-READ-SCOPE-01]` Single-Object Content Rendering**: `read` only renders the bounded content of one named file or virtual object; it never discovers, searches, filters, transforms, batches, or fetches.
- **`[INV-READ-VFS-01]` Virtual-Path Confinement**: `is_virtual_path` rejects any `..` segment or NUL byte, and physical fallback reads are strictly confined to canonical paths under an owned spool root.
- **`[INV-READ-TAIL-01]` Honest Bounded Tails**: Negative `offset` has no hidden line cap below `limit`/`MAX_READ_LINES`, always enforces `MAX_READ_BYTES`, and reads arbitrarily large files via bounded reverse-seek from EOF.
- **`[INV-READ-ARGS-01]` Strict Parameter Semantics**: `offset = 0` and `limit = 0` are rejected, and `pages` is rejected on every non-PDF target.
- **`[INV-READ-IO-01]` Bounded Single-FD Dispatch & Normalized LRU Caching**: Every disk read is hard-capped with `take(cap + 1)` on one fd, files `<= 256 KiB` are read once, non-text branches do not hash bytes, physical spool fallback runs off-thread, and caches normalize path keys with true LRU eviction.

---

## Consequences & Trade-offs

- **Positive**: Closes a real workspace-confinement bypass; makes tailing multi-gigabyte logs instantaneous; eliminates silent truncation and silent `pages` ignores; cuts small-file read I/O by up to 3x and eliminates repeated PDF parses during pagination; gives models an accurate, compact contract in the schema.
- **Negative & Clean Breaks**: Callers passing `offset: 0` or passing `pages` to non-PDF files now receive typed errors instead of silent coercion/ignore; callers relying on `offset: -500` being silently truncated to 100 lines now receive up to 500 lines (still bounded by `limit` and 50 KiB).

---

## Alternatives Considered & Negative Knowledge

- **Silently transcoding UTF-16 BOM files or rendering `.ipynb` notebooks as cell pseudocode**: Rejected. Both break the `read -> edit_text` round trip because `edit_text` operates on exact UTF-8 bytes on disk; silent transcoding risks corrupting file encoding on `write_file`. UTF-16 BOM files are instead diagnosed accurately as UTF-16 text with an explicit `iconv` hint, and `.ipynb` remains raw JSON text.
- **Adding `paths: Vec<String>` batching or `grep`/symbol parameters to `read`**: Rejected. Parallel tool calls already cover multi-file reads while preserving per-call typing and path confinement; search and structural queries belong to `search_text` and `code_query`.
- **Adding a text note to `ToolOutput::Image` when `pages` is passed**: Rejected. `ToolOutput::Image` carries `{ mime, data }` with no prose channel, and rejecting `pages` on non-PDFs uniformly teaches the model immediately without widening the wire enum.
- **Splitting PDF into a separate `read_pdf` tool**: Rejected for now. Nuo supports ~20 providers without uniform native PDF support, the parser is already isolated behind `DocumentExtractor` in `host`, and caching plus page-span headers eliminate the pagination pain without reintroducing pre-read type guessing.
