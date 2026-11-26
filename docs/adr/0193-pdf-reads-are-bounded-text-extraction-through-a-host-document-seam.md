---
id: ADR-0193
title: "PDF Reads Are Bounded Text Extraction Through a Host Document Seam"
status: accepted
date: 2026-11-15
scope: tools/filesystem, host/documents, cognition/context-budget, server/composition
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0193. PDF Reads Are Bounded Text Extraction Through a Host Document Seam

- Status: Accepted
- Date: 2026-11-15
- Amends: [ADR-0176](0176-read-is-one-capability-with-bounded-content-dispatch.md)
- Amended: [ADR-0257](0257-read-boundary-hardening-virtual-path-confinement-and-modular-dispatch.md)
  (versioned LRU extraction cache and line-window page-span headers)
- Complements: [ADR-0153](0153-effective-input-ceiling-tool-output-retention-and-ingest-image-normalization.md),
  [ADR-0179](0179-one-capability-one-producer-and-no-ghost-builtins.md),
  [ADR-0183](0183-composition-root-owns-capability-linkage.md)

---

## Context and Problem Statement

ADR-0176 made `read` the only model-facing read capability and deliberately left
PDF as a named gap: a file beginning with `%PDF-` returned an error telling the
model to convert it with `pdftotext` first. That error is honest, but expensive.
It costs a round trip, assumes a host tool that may not exist, and pushes an
out-of-band conversion outside every bound the harness owns.

The obvious alternatives are all wrong for this codebase:

- **Provider-native PDF input** needs a document capability axis, a typed
  `ToolOutput::Document`, per-provider lowering, a renderer, and a retention
  policy. OpenAI Chat Completions tool messages are strings; routes without the
  capability would silently degrade. Raw PDF bytes also replay on every turn.
- **A second `read_pdf` tool** recreates the type-guessing surface ADR-0176
  removed: the model would again have to know a path's encoding before reading.
- **Automatic `pdftotext` execution inside `read`** hides a subprocess in a
  read-only capability and makes platform and sandbox behavior part of `read`.
- **Lossy UTF-8 decoding or raw-byte text** is not a fallback; it is corruption.

Most agent PDF usage is text extraction from a document that has a text layer:
papers, specifications, contracts, invoices. The harness can extract that text
locally, bound it, and return the existing `ToolOutput::Code` shape. That closes
the ADR-0176 gap without inventing a second model-facing read surface and
without pretending that a scanned page is text.

## Architectural Decisions

### 1. PDF remains a dispatch branch inside `read`

`read` detects a PDF by searching for `%PDF-` inside the first 1 KiB of the
file. The PDF specification allows leading bytes before the header; an extension
is never evidence about content. A valid UTF-8 prose prefix keeps the path on
the text branch, so a document that merely mentions the marker is not turned
into a parse failure; the header must be at offset zero, follow only whitespace
or a BOM, or follow non-UTF-8 bytes. A PDF keeps the one schema
(`path`, `offset`, `limit`) and the one tool name. There is no `read_pdf`, no
alias, and no role-list change.

### 2. Extraction is a host-owned capability behind a tool contract

`nuo-tool` gains a `DocumentExtractor` seam, analogous to `ImageNormalizer`:

```text
nuo-tool-fs (read) --depends on--> nuo-tool (DocumentExtractor trait)
                                         ^
                                         |
                                  nuo-host (PDF parser implementation)
                                         ^
                                         |
                                  nuo-server (composition root injects it)
```

The filesystem capability crate must not link a PDF parser. The server
installs `HostDocumentExtractor` in the session `SystemToolContext`; an
embedding without one receives an explicit "no document text extractor is
installed" error, never a silent empty result. The parser is a pure-Rust,
MIT-licensed library; no native library, subprocess, or shell dependency is
introduced.

### 3. Extraction is bounded before, during, and after parsing

- Source ceiling: 64 MiB per PDF.
- Page ceiling: 1,000 pages.
- Extracted-text ceiling: 4 MiB.
- Wall-clock ceiling: 30 seconds.
- Parsing runs on a blocking thread and panics are caught and converted to a
  typed parse failure; a malformed PDF cannot take down the turn or the runtime.

The existing text paging caps still apply after extraction: 2,000 lines, 50 KiB,
and 2,000 characters per line. Extraction truncation is visible in the result
framing; it is never a silent end-of-file. A page that the parser cannot decode
fails the document read; only pages that parse and yield no text are skipped.

### 4. PDF text is the existing `Code` result with page provenance

The tool returns `ToolOutput::Code` with `lang: None`, line numbering, and the
normal `offset`/`limit` continuation semantics. Page boundaries are rendered as
`[page N]` markers so the model can cite a page. The framing names the page
count and says when extraction stopped at a bound.

PDF line numbers are line numbers of the extracted text, not coordinates in the
original page. This is explicit in the framing; pretending otherwise would be
false provenance.

### 5. Encrypted and scanned PDFs are named failures, not degraded content

- An encrypted PDF that cannot be opened with the empty user password returns
  an "encrypted" error.
- A PDF whose pages contain no non-whitespace text returns a "no extractable
  text" error with the number of pages inspected.
- OCR is not part of `read`. A scanned PDF stays a named gap, with guidance to
  run OCR externally or read page images through the vision channel.

A PDF that parses but yields only whitespace is not a successful empty read.

### 6. Provider passthrough stays rejected

This decision does not add PDF bytes to any model request. `read` returns text.
If the product later needs page images, layout fidelity, figures, or scanned
documents, that requires a separate multimodal document capability with its own
capability axis, typed output, provider lowering, renderer, and retention
policy. It is not an extension of this seam.

## Invariants

1. **`[INV-PDF-01]` One read capability.** PDF support adds no model-facing tool
   name, alias, or role-list entry.
2. **`[INV-PDF-02]` Bounded extraction.** No PDF read exceeds the source, page,
   text, or wall-clock ceilings; the text page caps still apply afterward.
3. **`[INV-PDF-03]` Binary never becomes text.** A PDF is either parsed into
   text or reported as a typed failure; its bytes are never decoded as UTF-8.
4. **`[INV-PDF-04]` Typed failure, not empty success.** Encrypted, no-text, and
   malformed PDFs are distinguishable errors.
5. **`[INV-PDF-05]` Parser stays in the host.** `nuo-tool-fs` and `nuo-tool` do
   not depend on a PDF parser; the composition root injects the implementation.
6. **`[INV-PDF-06]` No OCR claim.** A scanned PDF is reported as having no text
   layer; the harness never implies it read pixels it did not read.

## Consequences & Trade-offs

- **Positive**: A text-layer PDF becomes one bounded read with page provenance
  and no external `pdftotext` requirement. The model no longer pays a failed
  call plus a conversion call. The filesystem crate stays parser-free.
- **Positive**: The output stays inside the existing `Code` shape, so the
  renderer, token accounting, retention bands, and replay paths need no new
  variant.
- **Negative & Incurred Costs**: `nuo-host` links a PDF parser and its
  dependency graph. Extraction quality is document-dependent: multi-column
  layouts, tables, and unusual encodings may interleave or reorder text. Page
  markers are extraction boundaries, not a layout model. The wall-clock bound
  returns the turn but cannot cancel a parser already inside a blocking call;
  the source and page ceilings are the primary bound.
- **Negative & Incurred Costs**: Extraction runs in-process, so the source,
  page, text, and time ceilings bound the work and the result but not a parser's
  transient allocation. A hostile PDF can still pressure memory; process
  isolation is a future hardening step if the threat model changes.
- **Negative & Incurred Costs**: The parser's font stack includes
  `ttf-parser`, which RustSec marks **unmaintained** (RUSTSEC-2026-0192, no
  fixed upgrade). It is a warning, not a known vulnerability, and the
  dependency is confined to `nuo-host`; replacing the extractor is a contained
  change.
- **Negative & Incurred Costs**: The 33% of real-world PDFs that are scanned
  still fail. That is deliberate; OCR is a different capability with different
  cost and fidelity semantics.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Provider-native PDF document blocks
- **Why considered**: Preserves layout, figures, and scanned pages on models
  with native document input.
- **Why rejected**: Chat Completions tool messages cannot carry a document;
  supporting it would add a capability axis, typed output, per-provider
  lowering, UI rendering, and a second retention path. It would also replay
  base64 bytes on every turn. This is a product-level multimodal decision, not
  a `read` detail.

### Alternative B — A dedicated `read_pdf` tool
- **Why considered**: Keeps the text path untouched and makes PDF handling
  explicit.
- **Why rejected**: It reintroduces exactly the type-guessing tax ADR-0176
  removed. The model would have to know that a path is a PDF before calling.

### Alternative C — Invoke `pdftotext` from `read`
- **Why considered**: Reuses a mature external extractor with no new Rust
  dependency.
- **Why rejected**: It hides subprocess execution inside a read-only tool,
  depends on an unversioned host binary, and makes sandbox policy and error
  mapping platform-specific. The tool would also have to parse the subprocess's
  output and timeouts.

### Alternative D — OCR scanned PDFs
- **Why considered**: Closes the remaining failure mode.
- **Why rejected**: OCR is a separate capability with its own model, language,
  and page-image semantics. Adding it to `read` would turn a bounded text read
  into an unbounded vision pipeline.

## Negative Knowledge

- **Do not** add a `read_pdf`/`read_document` model-facing alias. PDF is one
  branch of `read`.
- **Do not** send raw PDF bytes to a provider from this path. Text extraction
  is the contract; document passthrough needs its own ADR.
- **Do not** silently return an empty string for a scanned or encrypted PDF.
  The failure must name what the harness could not read.
- **Do not** move the parser into `nuo-tool-fs` or `nuo-tool`. The seam exists
  so minimal embeddings do not compile a PDF stack.
- **Do not** call external binaries from `read`. Host capabilities are
  injected; subprocess policy belongs to `execute_command`.
- **Do not** treat extracted line numbers as page coordinates. Page markers are
  provenance; line numbers are text-page coordinates.

## Links

- [ADR-0176](0176-read-is-one-capability-with-bounded-content-dispatch.md)
- [ADR-0153](0153-effective-input-ceiling-tool-output-retention-and-ingest-image-normalization.md)
- [ADR-0179](0179-one-capability-one-producer-and-no-ghost-builtins.md)
- [ADR-0183](0183-composition-root-owns-capability-linkage.md)
