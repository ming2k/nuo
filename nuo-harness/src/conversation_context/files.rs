//! Implicit file-content context injected when the latest visible user text
//! references a path via `@file:` / `@files:`.
//!
//! This is the file analogue of [`super::skills`]: a mention of
//! `@file:src/main.rs` reads that file (subject to a workspace sandbox and a
//! size cap) and appends its contents as a hidden user message, so the model
//! sees the referenced source without an explicit `read_text` call.
//!
//! ## Reference semantics (ADR-0288)
//!
//! `@file:` is an *address*, not content. This module resolves the address and
//! emits exactly one canonical envelope per mention (`[INV-REF-02]`):
//!
//! ```text
//! <file path="src/main.rs" ref="@file:src/main.rs" bytes="10">…</file>
//! <file ref="@file:missing.rs" status="rejected" reason="…"/>
//! ```
//!
//! The canonical address travels in `ref=`, outcome in `status=`, and size
//! facts as attributes, so the model reads the asset (or the structured reason
//! it has none) instead of parsing prose. `mentions::canonicalize_addresses`
//! applies the matching rewrite to the visible prompt at request-projection
//! time (`[INV-REF-03]`/`[INV-REF-04]`).
//!
//! ## Safety model
//!
//! Every candidate path is resolved against the agent's workspace root and
//! canonicalized before it is read. A path is rejected if:
//! - it is absolute or contains a parent component (`..`) before resolution,
//! - its canonicalized form is not **inside** the workspace root (after
//!   symlink hardening), or
//! - it is a directory, a binary file, or larger than the size cap.
//!
//! Rejections are surfaced as a single hidden envelope (one per file) so the
//! model learns *why* the file was not loaded and can recover (switch to
//! `list_dir`, ask the user, etc.) instead of looping on the same path.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::{InjectionKind, Message, Role};

/// Hard upper bound on the number of bytes injected for a single `@file:`
/// reference. Mirrors the read tool's pagination page size; larger files are
/// truncated with a clear marker so the model knows there is more.
const MAX_FILE_BYTES: usize = 50 * 1024;

/// How many distinct `@file:` references a single round may inject. Caps the
/// worst case (a prompt full of file mentions) so one user turn cannot blow
/// the context budget before the model has even started.
const MAX_FILES_PER_ROUND: usize = 10;

/// Resolve `@file:` / `@files:` references in the latest visible user text and
/// append each file's contents as a hidden user message.
///
/// `workspace_root` is the base against which relative paths are resolved and
/// the sandbox they must stay inside; the caller (the agent) supplies the
/// persisted project root. Already-loaded files (a prior hidden
/// `[File '...' loaded]` note in this conversation) are skipped so a repeated
/// reference does not re-inject the body on every turn.
pub(crate) async fn inject_mentioned_files(
    workspace_root: Option<&Path>,
    messages: &mut Vec<Message>,
) {
    let Some(root) = workspace_root else {
        // No persisted project root → file injection is disabled. Skill
        // injection still runs independently. Surfacing nothing (rather than
        // guessing `cwd`) keeps the sandbox deterministic and auditable.
        return;
    };

    let text = latest_visible_user_text(messages);
    if text.is_empty() {
        return;
    }

    let referenced = parse_file_refs(&text);
    if referenced.is_empty() {
        return;
    }

    // Deduplication ledger: the set of canonical addresses already resolved in
    // this conversation. Legacy sessions persisted `[File '…' loaded]` prose
    // notes, so both shapes are read (`[INV-REF-07]`).
    let already_attempted: HashSet<String> = messages
        .iter()
        .filter(|message| message.role == Role::User && message.hidden)
        .filter_map(|message| {
            if let Some(address) = extract_file_envelope(&message.content) {
                return Some(address);
            }
            // Legacy `[File '<path>' …]` marker.
            let prefix = "[File '";
            let start = message.content.find(prefix)? + prefix.len();
            let end = message.content[start..].find('\'')?;
            Some(message.content[start..start + end].to_string())
        })
        .collect();

    let mut injected = 0usize;
    for address in referenced {
        if already_attempted.contains(&address) {
            continue;
        }
        if injected >= MAX_FILES_PER_ROUND {
            push_rejected(messages, &address, DEFERRED_REASON);
            injected += 1;
            continue;
        }
        // Sandboxed resolution + read is real filesystem work (canonicalize,
        // sniff, read): run it on the blocking pool so turn preparation never
        // blocks the executor. Injection happens once per prompt; later
        // requests and estimates reuse these bytes from the live window.
        let task_root = root.to_path_buf();
        let task_address = address.clone();
        match tokio::task::spawn_blocking(move || load_sandboxed(&task_root, &task_address))
            .await
            .unwrap_or_else(|error| Err(reason_string(&format!("injection task aborted: {error}"))))
        {
            Ok(loaded) => {
                let display = path_display(root, &loaded.canonical);
                messages.push(super::hidden_user_with_reason(
                    InjectionKind::ImplicitFile,
                    &display,
                    render_file(&display, &loaded),
                ));
                injected += 1;
            }
            Err(reason) => {
                push_rejected(messages, &address, &reason);
                injected += 1;
            }
        }
    }
}

/// Reason attached to the per-round cap rejection. A constant so the dedup
/// ledger and the rendered attribute cannot drift.
const DEFERRED_REASON: &str = "per-round file-injection limit reached";

/// A successfully resolved file: its canonical workspace path, the bytes as
/// injected (possibly truncated to [`MAX_FILE_BYTES`]), and the pre-truncation
/// length so the envelope can disclose that it holds a partial asset.
#[derive(Debug)]
struct Loaded {
    canonical: PathBuf,
    bytes: Vec<u8>,
    total: usize,
}

/// Extract the canonical address from an already-injected file envelope
/// (`ref="@file:…"`). Returns `None` for every other message. This is the
/// canonical half of the dedup reader.
fn extract_file_envelope(content: &str) -> Option<String> {
    if !content.starts_with("<file ") {
        return None;
    }
    let start = content.find("ref=\"@file:")? + "ref=\"@file:".len();
    let end = content[start..].find('"')?;
    Some(content[start..start + end].to_string())
}

/// Normalize a raw load failure into a single wire-safe attribute value:
/// attribute quotes are replaced, newlines flattened, and the trailing period
/// dropped so the envelope never depends on prose phrasing.
fn reason_string(reason: &str) -> String {
    reason
        .replace('"', "'")
        .replace(['\n', '\r'], " ")
        .trim()
        .trim_end_matches('.')
        .to_string()
}

/// The newest non-empty visible user message, joined if a round carries
/// several. Mirrors [`super::skills`]'s definition of "the prompt that
/// mentions".
fn latest_visible_user_text(messages: &[Message]) -> String {
    messages
        .iter()
        .filter(|message| message.role == Role::User && !message.hidden)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract `@file:{path}` / `@files:{path}` references from `text`, in order,
/// deduplicated. A thin filter over the shared grammar kernel
/// ([`nuo_contracts::mention::scan_references`]): escaped references are
/// literal text and skipped here (the canonicalizer, not the injector, consumes
/// the escape), and only file-namespace references are kept.
fn parse_file_refs(text: &str) -> Vec<String> {
    use nuo_contracts::mention::{Namespace, scan_references};

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for reference in scan_references(text) {
        if reference.escaped || reference.namespace != Namespace::File {
            continue;
        }
        if seen.insert(reference.target.to_string()) {
            out.push(reference.target.to_string());
        }
    }
    out
}

/// Resolve `raw` against `root`, sandbox it, and read it. Returns the
/// canonicalized path, the file bytes (already capped to [`MAX_FILE_BYTES`]),
/// and the pre-truncation length. Returns `Err(reason)` for every reject case
/// so the caller can surface a single actionable note.
fn load_sandboxed(root: &Path, raw: &str) -> Result<Loaded, String> {
    // Reject anything that is not a plain relative path *before* touching the
    // filesystem: an absolute path (`/etc/passwd`) or a parent traversal
    // (`../secret`) cannot live under the workspace by construction.
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return Err(
            "absolute paths are not allowed — reference a path relative to the workspace root"
                .to_string(),
        );
    }
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("`..` traversal is not allowed — stay within the workspace root".to_string());
    }
    if raw.is_empty() || raw == "." {
        return Err("empty path".to_string());
    }

    let joined = root.join(candidate);
    let canonical = joined.canonicalize().map_err(|e| {
        format!(
            "could not resolve '{}' under the workspace root: {}",
            raw, e
        )
    })?;

    // Symlink-hardened containment: the canonicalized path must start with the
    // canonicalized root. This catches a relative path that resolves through a
    // symlink out of the workspace.
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("workspace root is not resolvable: {}", e))?;
    if !canonical.starts_with(&canonical_root) {
        return Err("path escapes the workspace root".to_string());
    }

    if canonical.is_dir() {
        return Err(format!(
            "'{}' is a directory — use `list_dir` to inspect its contents",
            path_display(root, &canonical)
        ));
    }

    // Read the head first so an oversized binary is refused before the whole
    // file is buffered. Mirrors the read tool's sniff-then-read discipline.
    let mut head = [0u8; 4096];
    {
        use std::io::Read;
        let mut file = std::fs::File::open(&canonical)
            .map_err(|e| format!("could not open '{}': {}", path_display(root, &canonical), e))?;
        let n = file
            .read(&mut head)
            .map_err(|e| format!("could not read '{}': {}", path_display(root, &canonical), e))?;
        if is_binary_content(&head[..n]) {
            return Err(format!(
                "'{}' looks like a binary file and will not be injected",
                path_display(root, &canonical)
            ));
        }
    }

    let bytes = std::fs::read(&canonical)
        .map_err(|e| format!("could not read '{}': {}", path_display(root, &canonical), e))?;
    let total = bytes.len();
    if total > MAX_FILE_BYTES {
        // Truncate rather than refuse: a large source file is still useful in
        // context; the model just needs to know it is partial. The disclosure
        // travels as envelope attributes (`truncated`/`total`), not prose.
        return Ok(Loaded {
            canonical,
            bytes: bytes[..MAX_FILE_BYTES].to_vec(),
            total,
        });
    }
    Ok(Loaded {
        canonical,
        bytes,
        total,
    })
}

/// NUL or a disproportionate run of control bytes ⇒ binary. Same heuristic as
/// the read tool, kept local so this module owns its whole reject path.
fn is_binary_content(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return false;
    }
    if buf.contains(&0) {
        return true;
    }
    let control = buf
        .iter()
        .filter(|b| **b < 0x20 && **b != b'\n' && **b != b'\r' && **b != b'\t')
        .count();
    control * 10 > buf.len()
}

/// Render a resolved file as its canonical envelope (`[INV-REF-02]`). The
/// `ref` attribute is the canonical address the user's prompt named; `bytes`,
/// `total`, and `truncated` are machine-readable size facts.
fn render_file(display_path: &str, loaded: &Loaded) -> String {
    let body = String::from_utf8_lossy(&loaded.bytes);
    if loaded.total > loaded.bytes.len() {
        format!(
            "<file path=\"{display_path}\" ref=\"@file:{display_path}\" bytes=\"{}\" total=\"{}\" truncated=\"true\">\n{body}\n</file>",
            loaded.bytes.len(),
            loaded.total
        )
    } else {
        format!(
            "<file path=\"{display_path}\" ref=\"@file:{display_path}\" bytes=\"{}\">\n{body}\n</file>",
            loaded.bytes.len()
        )
    }
}

/// Append a hidden envelope explaining why a referenced file was not loaded,
/// so the model can recover instead of looping on the same path. `reason` is
/// always a machine-readable attribute (`status="rejected"`/`"deferred"`),
/// never prose the model has to parse.
fn push_rejected(messages: &mut Vec<Message>, raw: &str, reason: &str) {
    let status = if reason == DEFERRED_REASON {
        "deferred"
    } else {
        "rejected"
    };
    messages.push(super::hidden_user_with_reason(
        InjectionKind::ImplicitFile,
        raw,
        format!(
            "<file ref=\"@file:{raw}\" status=\"{status}\" reason=\"{}\"/>",
            reason_string(reason)
        ),
    ));
}

/// Display path relative to the workspace root when possible (cleaner in
/// context than an absolute path), falling back to the canonical form.
fn path_display(root: &Path, canonical: &Path) -> String {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    canonical
        .strip_prefix(&canonical_root)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| canonical.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_file_ref() {
        assert_eq!(
            parse_file_refs("refactor @file:src/main.rs now"),
            vec!["src/main.rs"]
        );
    }

    #[test]
    fn parses_files_plural_and_dedups() {
        let refs = parse_file_refs("@files:a.rs @files:a.rs and @file:b.rs");
        assert_eq!(refs, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn strips_trailing_punctuation() {
        // A sentence-ending period must not become part of the path.
        assert_eq!(parse_file_refs("see @file:src/lib.rs."), vec!["src/lib.rs"]);
        assert_eq!(parse_file_refs("(@file:x.txt,)",), vec!["x.txt"]);
    }

    #[test]
    fn ignores_bare_at_mention_without_namespace() {
        // `@some_user` is not a file reference — only `@file:`/`@files:` are.
        assert!(parse_file_refs("ping @some_user about @file:real.rs").len() == 1);
        assert_eq!(
            parse_file_refs("ping @some_user about @file:real.rs"),
            vec!["real.rs"]
        );
    }

    #[test]
    fn rejects_absolute_path() {
        let tmp = tempdir();
        let absolute = std::env::temp_dir().join("muta-absolute-path-probe");
        let err = load_sandboxed(tmp.path(), absolute.to_str().unwrap()).unwrap_err();
        assert!(err.contains("absolute paths are not allowed"));
    }

    #[test]
    fn rejects_parent_traversal() {
        let tmp = tempdir();
        let err = load_sandboxed(tmp.path(), "../secret").unwrap_err();
        assert!(err.contains("`..` traversal is not allowed"));
    }

    #[test]
    fn loads_file_inside_root() {
        let tmp = tempdir();
        let target = tmp.path().join("src").join("main.rs");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "fn main() {}").unwrap();
        let loaded = load_sandboxed(tmp.path(), "src/main.rs").unwrap();
        assert!(loaded.canonical.ends_with("main.rs"));
        assert_eq!(loaded.bytes, b"fn main() {}");
        assert_eq!(loaded.total, b"fn main() {}".len());
    }

    #[test]
    fn rejects_symlink_escape() {
        let tmp = tempdir();
        let outside_dir = tempdir();
        let outside = outside_dir.path().join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        // A symlink inside the workspace that points outside.
        if nuo_host::fs::symlink_file(&outside, &tmp.path().join("escape")).is_err() {
            return;
        }

        let err = load_sandboxed(tmp.path(), "escape").unwrap_err();
        assert!(err.contains("escapes the workspace root"), "got: {err}");
    }

    #[test]
    fn rejects_directory() {
        let tmp = tempdir();
        std::fs::create_dir_all(tmp.path().join("dir")).unwrap();
        let err = load_sandboxed(tmp.path(), "dir").unwrap_err();
        assert!(err.contains("directory"));
    }

    #[test]
    fn rejects_binary() {
        let tmp = tempdir();
        std::fs::write(tmp.path().join("blob.bin"), [0u8, 1, 2, 0, 4, 5]).unwrap();
        let err = load_sandboxed(tmp.path(), "blob.bin").unwrap_err();
        assert!(err.contains("binary"));
    }

    #[test]
    fn truncates_oversize_file() {
        let tmp = tempdir();
        // Double the cap, all ASCII so it is not flagged binary.
        let big = "a".repeat(MAX_FILE_BYTES * 2);
        std::fs::write(tmp.path().join("big.txt"), &big).unwrap();
        let loaded = load_sandboxed(tmp.path(), "big.txt").unwrap();
        // The injected bytes are capped; the pre-truncation length is disclosed
        // separately so the envelope can carry it as an attribute (ADR-0288),
        // not as prose baked into the body.
        assert_eq!(loaded.bytes.len(), MAX_FILE_BYTES);
        assert_eq!(loaded.total, MAX_FILE_BYTES * 2);
    }

    #[tokio::test]
    async fn inject_appends_hidden_message_for_file_ref() {
        let tmp = tempdir();
        std::fs::write(tmp.path().join("lib.rs"), "pub fn x() {}").unwrap();
        let mut messages = vec![Message::new(Role::User, "review @file:lib.rs".to_string())];
        inject_mentioned_files(Some(tmp.path()), &mut messages).await;
        assert_eq!(messages.len(), 2);
        assert!(messages[1].hidden);
        // Canonical envelope with the address in `ref=` (ADR-0288).
        assert_eq!(
            messages[1].content,
            "<file path=\"lib.rs\" ref=\"@file:lib.rs\" bytes=\"13\">\npub fn x() {}\n</file>"
        );
    }

    /// A rejected mention produces one structured envelope: success/failure is
    /// an attribute, never prose the model must parse (`[INV-REF-02]`).
    #[tokio::test]
    async fn rejected_mention_emits_structured_envelope() {
        let tmp = tempdir();
        let mut messages = vec![Message::new(
            Role::User,
            "@file:non_existent.rs".to_string(),
        )];
        inject_mentioned_files(Some(tmp.path()), &mut messages).await;
        assert_eq!(messages.len(), 2);
        let note = &messages[1].content;
        assert!(
            note.starts_with("<file ref=\"@file:non_existent.rs\" status=\"rejected\" reason=\""),
            "got: {note}"
        );
    }

    /// The per-round cap is a `deferred` (recoverable) outcome, distinct from a
    /// hard rejection, so the model knows it can read the rest explicitly.
    #[tokio::test]
    async fn over_limit_mention_is_deferred() {
        let tmp = tempdir();
        // 11 files: one past the cap. No file exists, so the first 10 are
        // rejected and the 11th is deferred.
        let refs = (0..=MAX_FILES_PER_ROUND)
            .map(|i| format!("@file:f{i}.rs"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut messages = vec![Message::new(Role::User, refs)];
        inject_mentioned_files(Some(tmp.path()), &mut messages).await;
        let deferred = messages
            .iter()
            .filter(|message| message.content.contains("status=\"deferred\""))
            .count();
        assert_eq!(deferred, 1, "exactly one over-cap note");
    }

    #[tokio::test]
    async fn inject_skips_already_loaded_or_failed_file() {
        let tmp = tempdir();
        std::fs::write(tmp.path().join("lib.rs"), "pub fn x() {}").unwrap();
        // First turn: loads lib.rs, fails non_existent.rs.
        let mut messages = vec![Message::new(
            Role::User,
            "@file:lib.rs and @file:non_existent.rs".to_string(),
        )];
        inject_mentioned_files(Some(tmp.path()), &mut messages).await;
        assert_eq!(messages.len(), 3);
        // Second turn: mention both again — neither must be re-injected.
        messages.push(Message::new(
            Role::User,
            "again @file:lib.rs and @file:non_existent.rs".to_string(),
        ));
        inject_mentioned_files(Some(tmp.path()), &mut messages).await;
        // Only the user message was added; no new hidden injection notes.
        assert_eq!(messages.len(), 4);
    }

    /// `[INV-REF-07]`: sessions persisted by earlier versions carry the legacy
    /// `[File '<path>' …]` marker; dedup must recognize it so those files are
    /// not re-injected after an upgrade.
    #[tokio::test]
    async fn legacy_marker_still_deduplicates() {
        let tmp = tempdir();
        std::fs::write(tmp.path().join("lib.rs"), "pub fn x() {}").unwrap();
        let mut messages = vec![
            crate::conversation_context::hidden_user(
                InjectionKind::ImplicitFile,
                "[File 'lib.rs' loaded]\npub fn x() {}\n[/File]",
            ),
            Message::new(Role::User, "@file:lib.rs".to_string()),
        ];
        inject_mentioned_files(Some(tmp.path()), &mut messages).await;
        // No new envelope: the legacy marker already recorded the address.
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn parses_escaped_and_code_spans() {
        // Escaped with backslash: skipped
        assert!(parse_file_refs(r"look at \@file:escaped.rs").is_empty());
        // Inside inline code block: skipped
        assert!(parse_file_refs("discussing `@file:inline.rs` in backticks").is_empty());
        // Inside fenced code block: skipped
        let fenced = "```\n@file:fenced.rs\n```";
        assert!(parse_file_refs(fenced).is_empty());
        // Non-word-boundary: skipped
        assert!(parse_file_refs("user@file:foo.rs").is_empty());
        // Valid boundary: matched
        assert_eq!(parse_file_refs("(@file:valid.rs)"), vec!["valid.rs"]);
    }

    #[tokio::test]
    async fn inject_without_workspace_root_is_noop() {
        let mut messages = vec![Message::new(Role::User, "@file:lib.rs".to_string())];
        inject_mentioned_files(None, &mut messages).await;
        assert_eq!(messages.len(), 1);
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }
}
