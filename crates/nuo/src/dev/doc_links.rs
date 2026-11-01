//! Markdown documentation hyperlink integrity checker.
//!
//! Rule source: `docs/dev/standards/style.md` §1.
//!
//! Enforces:
//!   1. Target existence (style.md §1.3)
//!   2. No absolute locators (style.md §1.2)
//!   3. Anchor existence (style.md §1.4)
//!   4. Audience firewall (taxonomy.md §2)
//!   5. User-owned surface exclusion (taxonomy.md §3, ADR-0141)

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const EXTERNAL: &[&str] = &["http://", "https://", "mailto:", "tel:", "data:"];

/// Root-level documentation files that are **not** repository documentation.
///
/// `AGENTS.md` is user-authored agent guidance: it is a runtime *input* the
/// product loads behind a trust gate (`nuo_host::security::PROJECT_RULE_FILES`),
/// not a governed documentation artifact. It is therefore excluded from every
/// link/anchor/style rule here, so a user may restructure it freely without a
/// repository gate judging the result (ADR-0141, taxonomy.md §3).
///
/// This is the **single** definition of the boundary, shared by the link gate
/// and the ADR lineage gate. Two parallel lists is exactly the drift that let
/// `AGENTS.md` acquire documentation authority in the first place.
pub const USER_OWNED_SURFACES: &[&str] = &["AGENTS.md"];

/// Public documentation surfaces (taxonomy.md §2). A relative link originating
/// in one of these must never resolve into the internal maintainer space.
const PUBLIC_SURFACES: &[&str] = &[
    "docs/tutorials/",
    "docs/how-to/",
    "docs/reference/",
    "docs/explanation/",
];

/// Internal maintainer space (taxonomy.md §2).
const INTERNAL_SPACE: &str = "docs/dev/";

fn is_external(target: &str) -> bool {
    EXTERNAL.iter().any(|prefix| target.starts_with(prefix))
}

fn github_slug(text: &str, inline_code_re: &Regex, link_re: &Regex) -> String {
    let lower = text.trim().to_lowercase();
    let no_code = inline_code_re.replace_all(&lower, "$1");
    let no_link = link_re.replace_all(&no_code, "$1");

    let mut slug = String::with_capacity(no_link.len());
    for c in no_link.chars() {
        if c.is_alphanumeric() || c.is_whitespace() || c == '-' || c == '_' {
            slug.push(c);
        }
    }
    slug.trim().replace(' ', "-")
}

fn heading_anchors(
    path: &Path,
    fence_re: &Regex,
    heading_re: &Regex,
    inline_code_re: &Regex,
    link_re: &Regex,
) -> HashSet<String> {
    let mut anchors = HashSet::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return anchors,
    };

    let mut in_fence = false;
    for line in content.lines() {
        if fence_re.is_match(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(caps) = heading_re.captures(line) {
            if let Some(heading_text) = caps.get(2) {
                let base = github_slug(heading_text.as_str(), inline_code_re, link_re);
                let count = seen.entry(base.clone()).or_insert(0);
                let anchor = if *count == 0 {
                    base
                } else {
                    format!("{base}-{count}")
                };
                *count += 1;
                anchors.insert(anchor);
            }
        }
    }

    anchors
}

fn find_markdown_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != "target" && name != ".git" && name != "node_modules"
        })
        .flatten()
    {
        if entry.file_type().is_file() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                if let Ok(rel) = path.strip_prefix(root) {
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    if USER_OWNED_SURFACES.contains(&rel_str.as_str()) {
                        continue;
                    }
                    if rel_str.starts_with("docs/") || !rel_str.contains('/') {
                        files.push(path.to_path_buf());
                    }
                }
            }
        }
    }
    files.sort();
    files
}

/// Normalize path components (resolve `.` and `..` without requiring the path to exist).
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut components = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                components.pop();
            }
            c => components.push(c),
        }
    }
    components.iter().collect()
}

pub fn check_doc_links(root: &Path) -> Result<(), String> {
    let fence_re = Regex::new(r"^\s*(```|~~~)").map_err(|e| e.to_string())?;
    let inline_code_re = Regex::new(r"`([^`]*)`").map_err(|e| e.to_string())?;
    let link_re = Regex::new(r"\[([^\]]*)\]\([^)]*\)").map_err(|e| e.to_string())?;
    let heading_re = Regex::new(r"^(#{1,6})\s+(.*?)\s*#*$").map_err(|e| e.to_string())?;
    let target_link_re =
        Regex::new(r"!?\[[^\]]*\]\(\s*<?([^)\s>]+)>?\s*\)").map_err(|e| e.to_string())?;

    // --- User-owned surface boundary (taxonomy.md §3, ADR-0141) -------------
    //
    // Two orthogonal, decidable rules:
    //
    //   1. **Nomination.** Every mention of a user-owned surface must appear as
    //      an inline code span. This makes the surface unambiguously a *filename*
    //      rather than an authority, and keeps prose grammatical: a bare
    //      `AGENTS.md` in a sentence is indistinguishable from a citation.
    //   2. **Citation.** A mention that is code-styled but is immediately
    //      qualified by a section number is a citation, not a filename, and is
    //      rejected. This is the exact form that rotted (`` (`AGENTS.md` §3) ``):
    //      quoting it as code must not launder an authority claim.
    //
    // Stripping code spans before matching (the obvious implementation) is
    // wrong in both directions: it lets code-styled section citations through,
    // and it punishes code-styled filenames. The rules are therefore evaluated
    // over the raw line plus knowledge of where the code spans are.
    let user_owned_re = Regex::new(&format!(r"\b({})\b", USER_OWNED_SURFACES.join("|")))
        .map_err(|e| e.to_string())?;
    // Inline code span. The `regex` crate has no backreferences, so this models
    // the CommonMark subset that actually occurs here: a single-backtick span
    // whose body contains no backtick. Multi-backtick spans exist only to embed
    // a literal backtick, which no user-owned surface name contains.
    let code_span_re = Regex::new(r"`([^`\n]*)`").map_err(|e| e.to_string())?;
    // `§3`, `§ 3`, `§3.1`, `#3` immediately after a code span.
    let section_qualifier_re = Regex::new(r"^\s*(?:§|#)\s*\d").map_err(|e| e.to_string())?;

    let mut problems = Vec::new();
    let mut checked = 0;
    let mut cached_anchors: HashMap<PathBuf, HashSet<String>> = HashMap::new();

    let files = find_markdown_files(root);

    for path in &files {
        let rel_posix = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let parent_dir = path.parent().unwrap_or(root);
        let mut in_fence = false;

        for (idx, line) in content.lines().enumerate() {
            let line_num = idx + 1;
            if fence_re.is_match(line) {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }

            // Strip inline code to avoid checking syntax examples
            let line_prose = inline_code_re.replace_all(line, "");

            // Rule 2 — citation: a code-styled mention qualified by a section
            // number. Checked first so the message names the real defect.
            let mut cited = false;
            for span in code_span_re.captures_iter(line) {
                let body = span.get(1).map(|m| m.as_str()).unwrap_or_default();
                if user_owned_re.is_match(body) {
                    let rest = &line[span.get(0).map(|m| m.end()).unwrap_or(0)..];
                    if section_qualifier_re.is_match(rest) {
                        cited = true;
                    }
                }
            }
            if cited {
                problems.push(format!(
                    "{rel_posix}:{line_num}: cites a section of the user-owned `AGENTS.md` surface (taxonomy.md §3 — quote the filename as a filename, or cite the standards document that owns the rule; ADR-0141)"
                ));
            } else {
                // Rule 1 — nomination: any mention left in prose is a bare
                // filename, which is indistinguishable from a citation.
                if user_owned_re.is_match(&line_prose) {
                    problems.push(format!(
                        "{rel_posix}:{line_num}: mentions the user-owned `AGENTS.md` surface outside inline code (taxonomy.md §3 — write it as `AGENTS.md`; repository documentation must not bind a file the product treats as untrusted workspace input; ADR-0141)"
                    ));
                }
            }

            for caps in target_link_re.captures_iter(&line_prose) {
                let target = match caps.get(1) {
                    Some(m) => m.as_str(),
                    None => continue,
                };

                if is_external(target) || target.starts_with('#') {
                    continue;
                }

                checked += 1;
                let location = format!("{rel_posix}:{line_num}");

                if target.starts_with('/') || target.starts_with("file://") {
                    problems.push(format!(
                        "{location}: absolute locator `{target}` (style.md §1.2 — use a relative path)"
                    ));
                    continue;
                }

                let (file_part, fragment) = match target.split_once('#') {
                    Some((f, frag)) => (f, Some(frag)),
                    None => (target, None),
                };

                if file_part.is_empty() {
                    continue;
                }

                let target_path = parent_dir.join(file_part);
                let resolved = normalize_path(&target_path);

                if !resolved.exists() {
                    problems.push(format!(
                        "{location}: `{target}` does not resolve (style.md §1.3 — dead link)"
                    ));
                    continue;
                }

                // Audience firewall (taxonomy.md §2): a relative link from a
                // public surface must never resolve into the internal space.
                if PUBLIC_SURFACES
                    .iter()
                    .any(|surface| rel_posix.starts_with(surface))
                {
                    if let Ok(resolved_rel) = resolved.strip_prefix(root) {
                        if resolved_rel
                            .to_string_lossy()
                            .replace('\\', "/")
                            .starts_with(INTERNAL_SPACE)
                        {
                            problems.push(format!(
                                "{location}: `{target}` crosses the audience firewall (taxonomy.md §2 — public docs must not link into {INTERNAL_SPACE})"
                            ));
                        }
                    }
                }

                if let Some(frag) = fragment {
                    let frag_lower = frag.to_lowercase();
                    let anchors = cached_anchors.entry(resolved.clone()).or_insert_with(|| {
                        heading_anchors(
                            &resolved,
                            &fence_re,
                            &heading_re,
                            &inline_code_re,
                            &link_re,
                        )
                    });

                    if !anchors.contains(&frag_lower) {
                        problems.push(format!(
                            "{location}: `{target}` has no matching heading (style.md §1.3/§1.4 — broken anchor)"
                        ));
                    }
                }
            }
        }
    }

    if problems.is_empty() {
        println!(
            "doc links: clean ({checked} relative targets checked in {} files)",
            files.len()
        );
        Ok(())
    } else {
        Err(format!(
            "doc links: {checked} relative targets checked, {} problem(s) in {} files:\n{}",
            problems.len(),
            files.len(),
            problems.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn audience_firewall_flags_public_link_into_dev() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "docs/dev/target.md", "# Target\n\nBody.\n");
        write(
            root,
            "docs/reference/leak.md",
            "# Leak\n\nSee [internals](../dev/target.md).\n",
        );

        let err = check_doc_links(root).unwrap_err();
        assert!(err.contains("audience firewall"), "unexpected error: {err}");
    }

    #[test]
    fn audience_firewall_allows_adr_and_outward_references() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "docs/adr/0001-x.md", "# X\n\nBody.\n");
        write(root, "docs/how-to/guide.md", "# Guide\n\nBody.\n");
        // Public surface linking to an ADR (permitted) and to another public doc.
        write(
            root,
            "docs/reference/ok.md",
            "# Ok\n\nSee [decision](../adr/0001-x.md) and [guide](../how-to/guide.md).\n",
        );
        // Internal surface linking into `docs/dev/**` (permitted: internal to internal).
        write(
            root,
            "docs/dev/note.md",
            "# Note\n\nSee [peer](other.md).\n",
        );
        write(root, "docs/dev/other.md", "# Other\n\nBody.\n");

        check_doc_links(root).unwrap();
    }

    /// ADR-0141 / taxonomy.md §3: `AGENTS.md` is user-authored workspace input,
    /// not repository documentation. A user may shape it freely — including
    /// with links that would be dead or absolute by documentation standards —
    /// without a docs gate judging the result.
    #[test]
    fn user_owned_agents_md_is_outside_doc_governance() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "docs/dev/note.md", "# Note\n\nBody.\n");
        write(
            root,
            "AGENTS.md",
            "# Agent Directives\n\nSee [nowhere](./does-not-exist.md).\n\
             Absolute: [x](/etc/passwd).\n\
             Anchor: [y](docs/dev/note.md#no-such-heading).\n",
        );

        check_doc_links(root)
            .expect("AGENTS.md must not be scanned by the documentation link gate");
    }

    /// The other half of the same boundary: repository documentation must not
    /// cite the user-owned surface, so the coupling cannot silently regress.
    #[test]
    fn docs_must_not_cite_the_user_owned_surface() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "docs/adr/0001-x.md",
            "# X\n\nThe rule is stated in AGENTS.md §3.\n",
        );

        let err = check_doc_links(root).unwrap_err();
        assert!(
            err.contains("user-owned `AGENTS.md`"),
            "unexpected error: {err}"
        );
    }

    /// Quoting the filename inside inline code or a fenced block is discussion
    /// of the mechanism (e.g. the trust gate), not a normative citation.
    #[test]
    fn quoting_agents_md_in_code_is_permitted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "docs/reference/trust.md",
            "# Trust\n\nThe loader reads `AGENTS.md` and `.cursorrules`.\n\n\
             ```text\nAGENTS.md\n```\n",
        );

        check_doc_links(root).unwrap();
    }

    /// Regression: the coupling this gate exists to prevent appeared as
    /// `` (`AGENTS.md` §3) `` — the filename *looked* like code, so stripping
    /// code spans before matching let the authority claim through untouched.
    /// Code styling must not launder a citation.
    #[test]
    fn code_styled_section_citation_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // `[\`AGENTS.md\`](#3)` is deliberately absent: it is already rejected
        // by the dead-anchor rule (style.md §1.3), and a same-page `#3` is not a
        // cross-document section citation, so it is out of this rule's scope.
        for body in [
            "# X\n\n2. **Mechanism over exhortation** (`AGENTS.md` §3).\n",
            "# X\n\nThe rule is in (`AGENTS.md`§3).\n",
            "# X\n\nAnchor-qualified: (`AGENTS.md` §3.1).\n",
        ] {
            write(root, "docs/adr/0001-x.md", body);
            let err =
                check_doc_links(root).expect_err("a code-styled section citation must be rejected");
            assert!(
                err.contains("cites a section"),
                "expected citation diagnosis for {body:?}, got: {err}"
            );
        }
    }

    /// Regression: a bare filename in product prose is a legitimate mention,
    /// but it read as an authority claim because it was not code-styled. The
    /// diagnosis must name the fix (`write it as `AGENTS.md``), not just fail.
    #[test]
    fn bare_mention_is_rejected_with_an_actionable_message() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "docs/how-to/teach-agents.md",
            "# Teach agents\n\nCreate a file named AGENTS.md at the project root.\n",
        );

        let err = check_doc_links(root).unwrap_err();
        assert!(err.contains("outside inline code"), "got: {err}");
        assert!(err.contains("write it as `AGENTS.md`"), "got: {err}");
    }

    /// The filename inside code stays permitted even mid-sentence and in
    /// tables, so ordinary product documentation needs no special casing.
    #[test]
    fn code_styled_filename_is_permitted_in_prose_and_tables() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "docs/reference/files.md",
            "# Files\n\nCreate `AGENTS.md` at the root.\n\n\
             | File | Role |\n| :--- | :--- |\n| `AGENTS.md` | project rules |\n| `.cursorrules` | project rules |\n",
        );

        check_doc_links(root).unwrap();
    }
}
