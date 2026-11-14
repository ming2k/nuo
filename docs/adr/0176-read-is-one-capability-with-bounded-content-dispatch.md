---
id: ADR-0176
title: "Read Is One Capability with Bounded Content Dispatch"
status: accepted
date: 2026-11-13
scope: tools/filesystem, protocol/tool-identity, cognition/context-budget, presentation/tool-cards
superseded_by: null
negative_knowledge: true
---

# 0176. Read Is One Capability with Bounded Content Dispatch

- Status: Accepted
- Date: 2026-11-13
- Deciders: Nuo Architecture Working Group
- Consulted: Tool Runtime, Filesystem Tools, Context Lifecycle, Presentation, Security
- Supersedes: the split `read_text` / `read_image` model-facing read surface, and the
  unbounded `read_text` whole-file default
- Complements: [ADR-0093](0093-ephemeral-session-spooling-and-tail-heavy-output-folding.md),
  [ADR-0153](0153-effective-input-ceiling-tool-output-retention-and-ingest-image-normalization.md),
  [ADR-0008](0008-single-tool-contract.md)

---

## Context and Problem Statement

Reading a path is one intent. The harness exposed it as three unrelated
capabilities: `read_text` for text, `read_image` for images, and `find_files`
for directories. The model had to know the kind of a path *before* reading it;
when it guessed wrong it paid a failed call and a second round trip. That is a
turn tax on exactly the phase where an agent explores an unfamiliar tree.

The split also produced three concrete defects:

1. **`read_text` was unbounded.** `limit: None` read the whole file. A generated
   file, a vendored source tree, or a lockfile could put hundreds of thousands
   of tokens into the live window in one call. ADR-0153 established that prompt
   size is the dominant local latency term; an unbounded read is a direct path
   back to that state.
2. **`read_image` was a ghost.** The implementation was deleted in the
   workspace split (`nuo-harness/src/tools/read_image.rs`), but the enum, role
   allowlists, presenter, guardrails, and `ToolOutput::Image` remained. The
   capability was silently filtered out of the live toolset because no factory
   produced it. An advertised capability with no implementation is worse than
   an absent one.
3. **Binary content reached the model as text.** `fs::read_to_string` failed on
   non-UTF-8 bytes, and the error did not distinguish "binary file" from
   "permission denied" or "not found". A model reading an image or a PDF through
   the text tool got a decode failure, then guessed again.

OpenCode's `read` demonstrates the correct model-facing shape: one tool, one
schema, internal dispatch by filesystem kind and magic bytes, bounded output,
and typed results. Nuo already has the pieces — typed `ToolOutput`, a VFS
resolver, image normalization in `nuo-host`, and a capability/variant resolver
— so the split is no longer justified by implementation isolation.

## Decision

### 1. One capability, one schema, no aliases

`BuiltinTool::Read` is the only model-facing read capability. `read_text` and
`read_image` are removed from the enum, the role allowlists, the guardrails,
the jail, and the TUI registry. Old transcripts that name them are unknown
tools: they render through the fallback presenter and do not gain a
compatibility lane. The break is intentional; a dual surface would preserve the
exact type-guessing the change exists to remove.

The schema is:

```json
{
  "path": "string",
  "offset": "integer?",
  "limit": "integer?"
}
```

`offset` is 1-based for text lines and directory entries. A negative offset
reads the tail of a text file. `limit` is capped by the tool, not trusted from
the caller.

### 2. Dispatch by filesystem kind, then content, never by extension alone

The tool resolves the path, then:

1. directory → one-level listing;
2. regular file → read a bounded prefix and classify by magic bytes;
3. virtual path → the existing `VirtualFileResolver` text channel;
4. missing / wrong kind → an error that names the actual kind.

Extension is used only for the `lang` hint on a text result. It never decides
whether bytes are an image, because a misnamed file is common and a decoder
must see the bytes anyway.

### 3. Text is bounded and honest

- Default and hard cap: 2,000 lines, 50 KiB, 2,000 characters per line.
- UTF-8 only. Invalid UTF-8 is an error that says so; it is not lossily
  decoded into the transcript.
- UTF-8 BOM is stripped once; CRLF is normalized for matching and preserved in
  the frame, not rewritten in the file.
- A NUL byte before the page boundary is a binary-file error, not text.
- A page carries `start_line` and a suffix naming the next offset when more
  lines remain. The model never has to guess where the page ended.
- A negative `offset` reads the tail (at most 100 lines by default), matching
  the "show me the end of the log" intent without a second call.

### 4. Directory listing is one level and paginated

`read` on a directory lists one level: directories first (`name/`), then files,
then symlinks (`name@`). It does not recurse. Recursive discovery remains
`find_files`, because a glob query and a directory listing are different
operations with different result shapes.

The listing is capped at 2,000 entries and carries `total`, `offset`, and
`next` so the model can continue deterministically; `total` is `Some` for a
live listing and `None` only for a restored pre-0176 listing that predates the
coordinates. Hidden entries are included; the caller asked for this directory,
not for a filtered search.

### 5. Media is typed, bounded, and normalized at the boundary

Recognized image magic: PNG, JPEG, GIF, WebP. The ingest cap is 20 MiB. The
result is `ToolOutput::Image { mime, data }`, never base64 text in a code block.

Normalization is a tool-layer seam (`ImageNormalizer` in `nuo-tool`) with the
host implementation injected by the server. PNG/JPEG are downscaled to the
1,568 px edge and re-encoded within the encoded-byte budget (the same policy
as ingest images, ADR-0153). GIF/WebP pass through untouched: re-encoding an
animation would silently change meaning.

A text-only model does not lose `read`; the request projection removes the
image part and leaves the existing placeholder. `read` therefore must not be
`requires_vision` — reading a text file is always useful. The capability is
one tool; media visibility remains a request-projection fact.

PDF is detected by `%PDF-` and rejected with a clear "PDF is not text" error
until the harness has a document capability (a model capability axis plus a
parser/extractor policy). Sending raw PDF bytes to a Chat-Completions tool
message is not a valid fallback, and silently decoding it as UTF-8 is worse.

### 6. The VFS channel survives

Spool files, artifacts, and compacted-history projections already resolve
through `VirtualFileResolver` into text. `read` keeps that branch as the first
text source; media from the offstream artifact registry remains a separate
server concern and is not forced through the filesystem path.

## Invariants

1. **One read capability.** Exactly one model-facing name, `read`. No
   `read_text`/`read_image` aliases in the builtin enum or role lists.
2. **Bounded by construction.** No text result exceeds the line/byte/line-length
   caps; no media result exceeds the ingest cap; no directory page exceeds the
   entry cap.
3. **Binary never becomes text.** A NUL-bearing or invalid-UTF-8 file is
   reported as such, never decoded into the model window.
4. **Media is typed.** Images return `ToolOutput::Image`; base64 is an encoding
   detail of that variant, not text content.
5. **No recursion in `read`.** A directory listing is one level; recursive
   discovery is `find_files`.
6. **Deterministic pagination.** Ordering, `total`, `offset`, and `next` are
   stable for an unchanged directory/file.
7. **No silent capability.** A builtin in a role list must be produced by the
   toolset; the ghost `read_image` state is not representable after this ADR.

## Consequences

- The developer/ops tool schema loses one capability and one type-guessing
  decision. Unknown-path exploration costs one call instead of up to three.
- `read` becomes the first tool with a genuinely polymorphic result. The TUI
  needs a result-kind dispatch on the structured `ToolOutput`, not a static
  per-tool kind; the registry gains one `Auto` classification.
- `nuo-tool-fs` gains a base64 dependency and an injected image normalizer;
  `nuo-host` implements the seam. The text path does not link image codecs.
- Every role, guardrail, jail, presenter, fixture, and test that named
  `read_text`/`read_image` is updated in the same change. There is no
  deprecation window.
- PDF remains a named gap with an explicit error, not a silent misdecode.
  Closing it requires a document capability decision, not another read alias.

## Negative Knowledge

- **Do not** add `read_text`/`read_image` back as aliases "for old sessions".
  The alias is the type-guessing surface this ADR removes.
- **Do not** let `read` recurse into directories. `find_files` owns recursive
  globs; merging them would make one result union carry two unrelated query
  languages.
- **Do not** base64 an image into `ToolOutput::Text`. The typed variant is what
  lets the request projection, token accounting, and renderer agree.
- **Do not** decode invalid UTF-8 lossily. A corrupted source line is worse
  than an explicit error.
- **Do not** make `read` require vision. Text reading must survive on a
  text-only model; media is projected away per request.
- **Do not** send raw PDF bytes through a tool message. That is a provider
  capability, not a read implementation detail.
