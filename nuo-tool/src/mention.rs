//! Shared lexical grammar for `@`-references in prompt text (ADR-0288, ADR-0291).
//!
//! `@` is a **reference operator**, not content: `@file:{path}` and
//! `@skill:{name}` name an asset, and the asset (not the bare token) is what a
//! consumer acts on. This module is the **single owner** of that grammar: code
//! -span masking, the delimiter alphabets, the word-boundary/escape guard, the
//! `@`-scanner, and the `skill://` scanner. Everywhere else — the agent's file
//! and skill injectors, the request-projection canonicalizer, the skills
//! resolver, and the composer completion engine — is a *filter over*
//! [`scan_references`] or a call to [`mention_range_at`], never a second copy of
//! the scan.
//!
//! ## Why this lives in `muta-contracts` (ADR-0057)
//!
//! The admission rule requires that an item is exchanged by *multiple
//! independent workspace layers* or *breaks a dependency cycle*. The grammar is
//! consumed by `muta-agent`, `muta-skills`, `muta-runtime`, and the terminal
//! app, and it cannot live in any one of them: `muta-skills` must not depend on
//! `muta-agent` (that would invert the `agent → skills` edge, ADR-0059), and
//! both runtime and the terminal need the same guard. This module is therefore
//! the only acyclic, single-source home. It stays **pure and I/O-free** — it
//! recognizes and segments references, and resolves nothing.
//!
//! Resolution (loading a file, matching a skill) and rewriting (canonicalizing
//! an address for the request projection, ADR-0288 `[INV-REF-03]`) are
//! *policy* and stay with their owners; only the shared *grammar* is here.

/// Which asset namespace a reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    File,
    Skill,
}

/// The surface form a reference was written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// `@file:{path}` / `@files:{path}` / `@skill:{name}` / `@skills:{name}`.
    Qualified,
    /// Bare `@{name}` — a skill mention with no namespace (skills only).
    Bare,
    /// `skill://{name}` or `skill://{name/path}`.
    Uri,
}

/// The verdict of the reference-start guard at a byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Preceded by a backslash: literal text, not a reference — unless the
    /// consumer is the canonicalizer, which consumes the escape (ADR-0288
    /// `[INV-REF-04]`).
    Escaped,
    /// Start of text, or preceded by whitespace or an open delimiter.
    Boundary,
    /// Preceded by a word character (e.g. `user@host`): not a reference.
    Rejected,
}

/// One recognized `@`-reference, borrowed from the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reference<'a> {
    pub namespace: Namespace,
    pub form: Form,
    /// The target token: a relative path (`File`), a skill name (`Skill`), or a
    /// `skill://` name/path. A `File` target has trailing sentence periods
    /// trimmed so "see @file:src/main.rs." reads cleanly.
    pub target: &'a str,
    /// Byte offset of the `@` (or of the leading `s` of `skill://`).
    pub start: usize,
    /// Byte offset one past the consumed target token. For a `File` target with
    /// a trailing sentence period this excludes the period, so a rewriting
    /// consumer can preserve it.
    pub end: usize,
    /// Whether a backslash guarded the reference.
    pub escaped: bool,
}

/// Mask out inline (`` `...` ``) and fenced (```` ```...``` ````) code blocks
/// with spaces, preserving exact UTF-8 byte lengths and newlines so that byte
/// offsets align with the original text. A mention inside code is a quotation,
/// not a reference, so masking makes the `@`/`skill://` scans blind to it
/// without a second code path.
pub fn mask_code_spans(text: &str) -> String {
    let mut bytes = text.as_bytes().to_vec();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if i + 2 < n && bytes[i] == b'`' && bytes[i + 1] == b'`' && bytes[i + 2] == b'`' {
            let start = i;
            i += 3;
            while i + 2 < n && !(bytes[i] == b'`' && bytes[i + 1] == b'`' && bytes[i + 2] == b'`') {
                i += 1;
            }
            let end = if i + 2 < n { i + 3 } else { n };
            for b in &mut bytes[start..end] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
            i = end;
        } else if bytes[i] == b'`' {
            let start = i;
            i += 1;
            while i < n && bytes[i] != b'`' && bytes[i] != b'\n' {
                i += 1;
            }
            if i < n && bytes[i] == b'`' {
                let end = i + 1;
                for b in &mut bytes[start..end] {
                    *b = b' ';
                }
                i = end;
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| text.to_string())
}

/// Characters permitted inside a raw `@file:` reference. Relative paths may
/// contain path separators and the usual filename alphabet; whitespace, quotes,
/// commas, and other sentence punctuation terminate the reference so prose like
/// "see @file:src/main.rs." reads cleanly.
pub fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '\\' | '_' | '-' | '.' | '+' | '~' | '@')
}

/// Characters allowed inside a skill name (e.g. `rust-expert`, `code_review`,
/// `v1.2`). A `skill://` source path additionally permits `/`, which callers
/// add explicitly — a bare name never contains one.
pub fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// The literal namespace spellings accepted after `@` for each namespace, in
/// canonical-then-plural order. The single definition of the namespace
/// vocabulary: the scanner, the completion query filter, and any future
/// consumer read these rather than re-typing the strings.
pub const FILE_NAMESPACES: [&str; 2] = ["file:", "files:"];
pub const SKILL_NAMESPACES: [&str; 2] = ["skill:", "skills:"];

/// Strip a leading file-namespace spelling from `rest`, returning the target.
pub fn strip_file_namespace(rest: &str) -> Option<&str> {
    FILE_NAMESPACES.iter().find_map(|ns| rest.strip_prefix(ns))
}

/// Strip a leading skill-namespace spelling from `rest`, returning the target.
pub fn strip_skill_namespace(rest: &str) -> Option<&str> {
    SKILL_NAMESPACES.iter().find_map(|ns| rest.strip_prefix(ns))
}

/// The word-boundary/escape guard, evaluated at the byte offset of an `@`.
/// Single owner of the "is this `@` a reference starter, an escaped literal, or
/// a word-internal `@`" decision that the agent, skills, runtime, and terminal
/// all depend on.
pub fn reference_start(text: &str, at: usize) -> Start {
    if at > 0 && text.as_bytes().get(at - 1) == Some(&b'\\') {
        return Start::Escaped;
    }
    if at == 0 {
        return Start::Boundary;
    }
    let prev = text[..at].chars().next_back().unwrap_or(' ');
    if prev.is_whitespace() || matches!(prev, '(' | '[' | '{' | '"' | '\'' | '<') {
        Start::Boundary
    } else {
        Start::Rejected
    }
}

/// Scan `text` for every `@`-reference and `skill://` URI, in source order.
///
/// This is the one true scanner. A reference is recognized when its `@` (or
/// `skill://` scheme) passes [`reference_start`] against the **code-span-masked**
/// view. Recognized shapes:
///
/// - `@file:{path}` / `@files:{path}` → [`Namespace::File`], [`Form::Qualified`]
/// - `@skill:{name}` / `@skills:{name}` → [`Namespace::Skill`], [`Form::Qualified`]
/// - `@{name}` → [`Namespace::Skill`], [`Form::Bare`] (no namespace prefix)
/// - `skill://{name|path}` → [`Namespace::Skill`], [`Form::Uri`]
///
/// Escaped references are yielded with [`Reference::escaped`] set; the caller
/// decides whether to honor (skip) or consume (unescape) the guard. Returns
/// every reference; consumers filter by namespace/form.
pub fn scan_references(text: &str) -> Vec<Reference<'_>> {
    let masked = mask_code_spans(text);
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < masked.len() {
        let rest = &masked[i..];
        let at_rel = rest.find('@');
        let uri_rel = rest.find("skill://");
        match (at_rel, uri_rel) {
            (None, None) => break,
            (Some(a), Some(u)) if a <= u => i = scan_at(text, i + a, &mut out),
            (Some(a), None) => i = scan_at(text, i + a, &mut out),
            (Some(_), Some(u)) | (None, Some(u)) => {
                let start = i + u;
                let after_scheme = start + "skill://".len();
                let mut end = after_scheme;
                while let Some(ch) = text[end..].chars().next() {
                    if !(is_name_char(ch) || ch == '/') {
                        break;
                    }
                    end += ch.len_utf8();
                }
                if end > after_scheme {
                    out.push(Reference {
                        namespace: Namespace::Skill,
                        form: Form::Uri,
                        target: &text[after_scheme..end],
                        start,
                        end,
                        escaped: false,
                    });
                }
                i = end.max(start + 1);
            }
        }
    }
    out
}

/// Recognize one `@`-reference whose `@` sits at `at`, pushing it if valid, and
/// return the byte offset to resume scanning from (past the reference when one
/// was recognized, else past the `@`).
fn scan_at<'a>(text: &'a str, at: usize, out: &mut Vec<Reference<'a>>) -> usize {
    let after_at = at + 1;
    let mut escaped = false;
    match reference_start(text, at) {
        Start::Rejected => return after_at,
        Start::Escaped => escaped = true,
        Start::Boundary => {}
    }

    let rest = text.get(after_at..).unwrap_or("");

    // Qualified `file:` / `files:` → a relative path (trailing period trimmed).
    if let Some(stripped) = strip_file_namespace(rest) {
        let target_start = after_at + (rest.len() - stripped.len());
        let mut end = target_start;
        while let Some(ch) = text[end..].chars().next() {
            if !is_path_char(ch) {
                break;
            }
            end += ch.len_utf8();
        }
        let raw = text[target_start..end].trim_end_matches('.');
        if raw.is_empty() {
            return after_at;
        }
        let clean_end = target_start + raw.len();
        out.push(Reference {
            namespace: Namespace::File,
            form: Form::Qualified,
            target: raw,
            start: at,
            end: clean_end,
            escaped,
        });
        return clean_end;
    }

    // Qualified `skill:` / `skills:` → a skill name; otherwise a bare `@name`.
    let (name_start, qualified) = match strip_skill_namespace(rest) {
        Some(stripped) => (after_at + (rest.len() - stripped.len()), true),
        None => (after_at, false),
    };
    let mut end = name_start;
    while let Some(ch) = text[end..].chars().next() {
        if !is_name_char(ch) {
            break;
        }
        end += ch.len_utf8();
    }
    if end == name_start {
        return after_at;
    }
    out.push(Reference {
        namespace: Namespace::Skill,
        form: if qualified {
            Form::Qualified
        } else {
            Form::Bare
        },
        target: &text[name_start..end],
        start: at,
        end,
        escaped,
    });
    end
}

/// The byte range of the `@`-mention token the cursor sits inside, as
/// `(at_byte, cursor_byte)`. Returns `None` when the cursor is not inside a
/// legal mention (no `@` back to the start of the whitespace-delimited token,
/// an escaped `@`, or a `@` preceded by a word character).
///
/// Single owner of the token-range computation shared by the daemon completion
/// engine and the terminal composer (previously duplicated verbatim).
pub fn mention_range_at(input: &str, cursor_byte: usize) -> Option<(usize, usize)> {
    if cursor_byte > input.len() || !input.is_char_boundary(cursor_byte) {
        return None;
    }
    let mut chars_before = input[..cursor_byte].char_indices().collect::<Vec<_>>();
    while let Some((idx, character)) = chars_before.pop() {
        if character.is_whitespace() {
            return None;
        }
        if character == '@' {
            return match reference_start(input, idx) {
                Start::Boundary => Some((idx, cursor_byte)),
                Start::Escaped | Start::Rejected => None,
            };
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_inline_and_fenced_code_preserving_length() {
        let inline = "using `@file:a.rs` here";
        let masked = mask_code_spans(inline);
        assert_eq!(masked.len(), inline.len());
        assert!(!masked.contains("@file:"));
        // Newlines inside a fence survive so offsets and line structure align.
        let fenced = "```\n@file:a.rs\n```";
        let masked = mask_code_spans(fenced);
        assert_eq!(masked.len(), fenced.len());
        assert_eq!(masked.matches('\n').count(), fenced.matches('\n').count());
        assert!(!masked.contains("@file:"));
    }

    #[test]
    fn path_and_name_alphabets_bound_their_runs() {
        assert!(is_path_char('/') && is_path_char('.') && !is_path_char(' '));
        assert!(is_name_char('-') && !is_name_char('/') && !is_name_char(' '));
    }

    #[test]
    fn scans_qualified_file_and_skill_in_order() {
        let refs = scan_references("edit @file:src/main.rs then @skill:rust now");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].namespace, Namespace::File);
        assert_eq!(refs[0].form, Form::Qualified);
        assert_eq!(refs[0].target, "src/main.rs");
        assert_eq!(
            &"edit @file:src/main.rs then @skill:rust now"[refs[0].start..refs[0].end],
            "@file:src/main.rs"
        );
        assert_eq!(refs[1].namespace, Namespace::Skill);
        assert_eq!(refs[1].target, "rust");
    }

    #[test]
    fn plural_namespaces_collapse_to_the_same_namespace() {
        let refs = scan_references("@files:a.rs @skills:b");
        assert_eq!(refs.len(), 2);
        assert_eq!(
            (refs[0].namespace, refs[0].target),
            (Namespace::File, "a.rs")
        );
        assert_eq!((refs[1].namespace, refs[1].target), (Namespace::Skill, "b"));
    }

    #[test]
    fn bare_at_is_a_skill_mention_with_no_namespace() {
        let refs = scan_references("ping @some_user here");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].namespace, Namespace::Skill);
        assert_eq!(refs[0].form, Form::Bare);
        assert_eq!(refs[0].target, "some_user");
    }

    #[test]
    fn skill_uri_is_scanned_with_slash_paths() {
        let refs = scan_references("load skill://skills/rust/SKILL.md please");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].form, Form::Uri);
        assert_eq!(refs[0].target, "skills/rust/SKILL.md");
    }

    #[test]
    fn file_target_trims_a_sentence_period_but_end_excludes_it() {
        let text = "see @file:src/lib.rs.";
        let refs = scan_references(text);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, "src/lib.rs");
        // `end` stops before the period so a rewriter can preserve it.
        assert_eq!(&text[refs[0].start..refs[0].end], "@file:src/lib.rs");
    }

    #[test]
    fn escaped_and_word_internal_references_are_still_classified() {
        let refs = scan_references(r"literal \@file:esc.rs and user@file:real.rs");
        assert_eq!(refs.len(), 1, "word-internal @ is rejected");
        assert_eq!(refs[0].target, "esc.rs");
        assert!(refs[0].escaped, "the escaped one is still yielded, flagged");
    }

    #[test]
    fn code_spans_are_invisible_to_the_scan() {
        assert!(scan_references("quoting `@file:x.rs` here").is_empty());
        assert!(scan_references("```\n@file:x.rs\n```").is_empty());
    }

    #[test]
    fn mention_range_matches_guard_semantics() {
        assert_eq!(mention_range_at("@src", 4), Some((0, 4)));
        assert_eq!(mention_range_at("look at @src", 12), Some((8, 12)));
        assert_eq!(mention_range_at("user@host", 9), None);
        assert_eq!(mention_range_at("@src foo", 8), None);
        assert_eq!(mention_range_at("look @src", 4), None);
        assert_eq!(mention_range_at("look at @co", 11), Some((8, 11)));
    }
}
