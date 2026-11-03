//! Markdown documentation hyperlink integrity and audience firewall checker.
//!
//! Rule source: `docs/dev/standards/style.md` §1, `taxonomy.md` §2-§3, ADR-0111, ADR-0141.
//!
//! Enforces:
//!   1. Target existence (style.md §1.3)
//!   2. No absolute locators (style.md §1.2)
//!   3. Anchor existence (style.md §1.4)
//!   4. Audience firewall (taxonomy.md §2)
//!   5. User-owned surface boundary (taxonomy.md §3, ADR-0141)

use crate::config::GovernanceConfig;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

const EXTERNAL_PREFIXES: &[&str] = &["http://", "https://", "mailto:", "tel:", "data:"];

fn is_external(target: &str) -> bool {
    EXTERNAL_PREFIXES
        .iter()
        .any(|prefix| target.starts_with(prefix))
}

pub fn github_slug(text: &str) -> String {
    let lower = text.trim().to_lowercase();
    let mut slug = String::with_capacity(lower.len());
    for c in lower.chars() {
        if c.is_alphanumeric() || c.is_whitespace() || c == '-' || c == '_' {
            slug.push(c);
        }
    }
    slug.split_whitespace().collect::<Vec<_>>().join("-")
}

/// Extract all heading anchor slugs from a markdown file using AST.
pub fn heading_anchors(content: &str) -> HashSet<String> {
    let parser = Parser::new_ext(content, Options::all());
    let mut anchors = HashSet::new();
    let mut seen: HashMap<String, usize> = HashMap::new();

    let mut in_heading = false;
    let mut current_heading = String::new();

    for event in parser {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                in_heading = true;
                current_heading.clear();
            }
            Event::End(TagEnd::Heading(..)) => {
                in_heading = false;
                let base = github_slug(&current_heading);
                if !base.is_empty() {
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
            Event::Text(t) | Event::Code(t) if in_heading => {
                current_heading.push_str(&t);
            }
            _ => {}
        }
    }

    anchors
}

/// Normalize path components (resolve `.` and `..` without requiring the path to exist).
pub fn normalize_path(path: &Path) -> PathBuf {
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

/// Discover markdown files governed by repository documentation rules.
pub fn find_markdown_files(root: &Path, config: &GovernanceConfig) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let user_owned: HashSet<&str> = config
        .docs
        .user_owned_surfaces
        .iter()
        .map(|s| s.as_str())
        .collect();

    // Scan doc roots (e.g. `docs/`)
    for doc_root in &config.docs.doc_roots {
        let full_doc_root = root.join(doc_root);
        if !full_doc_root.exists() {
            continue;
        }

        for entry in WalkDir::new(&full_doc_root)
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
                        if !user_owned.contains(rel_str.as_str()) {
                            files.push(path.to_path_buf());
                        }
                    }
                }
            }
        }
    }

    // Scan specified root files (e.g. `README.md`, `CHANGELOG.md`)
    for root_file in &config.docs.root_files {
        let path = root.join(root_file);
        if path.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                if !user_owned.contains(rel_str.as_str()) && !files.contains(&path) {
                    files.push(path);
                }
            }
        }
    }

    files.sort();
    files
}

pub fn check_doc_links(root: &Path) -> Result<(), String> {
    let config = GovernanceConfig::load_from_workspace(root);
    check_doc_links_with_config(root, &config)
}

pub fn check_doc_links_with_config(root: &Path, config: &GovernanceConfig) -> Result<(), String> {
    let section_qualifier_re =
        Regex::new(r"^\s*(?:§|#)\s*\d").map_err(|e| format!("regex error: {e}"))?;

    let user_owned_regexes: Vec<(String, Regex)> = config
        .docs
        .user_owned_surfaces
        .iter()
        .filter_map(|surface| {
            let escaped = regex::escape(surface);
            Regex::new(&format!(r"\b{escaped}\b"))
                .ok()
                .map(|re| (surface.clone(), re))
        })
        .collect();

    let files = find_markdown_files(root, config);
    let mut cached_anchors: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    let mut problems = Vec::new();
    let mut checked_links = 0;

    for path in &files {
        let rel_posix = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let line_starts: Vec<usize> = std::iter::once(0)
            .chain(content.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let get_line_num = |offset: usize| -> usize {
            match line_starts.binary_search(&offset) {
                Ok(idx) => idx + 1,
                Err(idx) => idx,
            }
        };

        let parent_dir = path.parent().unwrap_or(root);
        let parser = Parser::new_ext(&content, Options::all()).into_offset_iter();
        let events: Vec<(Event, std::ops::Range<usize>)> = parser.collect();
        let mut in_code_block = false;
        let mut in_metadata_block = false;

        for i in 0..events.len() {
            let (event, range) = &events[i];
            let offset = range.start;
            let line_num = get_line_num(offset);
            let location = format!("{rel_posix}:{line_num}");

            match event {
                Event::Start(Tag::MetadataBlock(..)) => {
                    in_metadata_block = true;
                }
                Event::End(TagEnd::MetadataBlock(..)) => {
                    in_metadata_block = false;
                }
                Event::Start(Tag::CodeBlock(..)) => {
                    in_code_block = true;
                }
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                }

                // Rule 1 (Nomination): Mentions in raw prose outside inline code spans, code blocks, and metadata
                Event::Text(text) if !in_code_block && !in_metadata_block => {
                    for (surface, re) in &user_owned_regexes {
                        if re.is_match(text) {
                            problems.push(format!(
                                "{location}: mentions the user-owned `{surface}` surface outside inline code (taxonomy.md §3 — write it as `{surface}`; repository documentation must not bind a file the product treats as untrusted workspace input; ADR-0141)"
                            ));
                        }
                    }
                }

                // Rule 2 (Citation): Code-styled mention followed by section qualifier
                Event::Code(code_text) => {
                    for (surface, re) in &user_owned_regexes {
                        if re.is_match(code_text) {
                            // Check if next event is text starting with section qualifier
                            if let Some((Event::Text(next_text), _)) = events.get(i + 1) {
                                if section_qualifier_re.is_match(next_text) {
                                    problems.push(format!(
                                        "{location}: cites a section of the user-owned `{surface}` surface (taxonomy.md §3 — quote the filename as a filename, or cite the standards document that owns the rule; ADR-0141)"
                                    ));
                                }
                            }
                        }
                    }
                }

                // Relative Link integrity & Audience firewall
                Event::Start(Tag::Link { dest_url, .. }) => {
                    let target = dest_url.as_ref();
                    if is_external(target) || target.starts_with('#') {
                        continue;
                    }

                    checked_links += 1;

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

                    // Audience firewall check (taxonomy.md §2)
                    let is_public = config
                        .docs
                        .firewall
                        .public_surfaces
                        .iter()
                        .any(|surf| rel_posix.starts_with(surf));

                    if is_public {
                        if let Ok(resolved_rel) = resolved.strip_prefix(root) {
                            let resolved_rel_str =
                                resolved_rel.to_string_lossy().replace('\\', "/");
                            if resolved_rel_str.starts_with(&config.docs.firewall.internal_space) {
                                problems.push(format!(
                                    "{location}: `{target}` crosses the audience firewall (taxonomy.md §2 — public docs must not link into {})",
                                    config.docs.firewall.internal_space
                                ));
                            }
                        }
                    }

                    // Heading anchor check
                    if let Some(frag) = fragment {
                        let frag_lower = frag.to_lowercase();
                        let anchors = cached_anchors.entry(resolved.clone()).or_insert_with(|| {
                            let target_content = fs::read_to_string(&resolved).unwrap_or_default();
                            heading_anchors(&target_content)
                        });

                        if !anchors.contains(&frag_lower) {
                            problems.push(format!(
                                "{location}: `{target}` has no matching heading (style.md §1.3/§1.4 — broken anchor)"
                            ));
                        }
                    }
                }

                _ => {}
            }
        }
    }

    if problems.is_empty() {
        println!(
            "doc links: clean ({checked_links} relative targets checked in {} files)",
            files.len()
        );
        Ok(())
    } else {
        Err(format!(
            "doc links: {checked_links} relative targets checked, {} problem(s) in {} files:\n{}",
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
        write(
            root,
            "docs/reference/ok.md",
            "# Ok\n\nSee [decision](../adr/0001-x.md) and [guide](../how-to/guide.md).\n",
        );
        write(
            root,
            "docs/dev/note.md",
            "# Note\n\nSee [peer](other.md).\n",
        );
        write(root, "docs/dev/other.md", "# Other\n\nBody.\n");

        check_doc_links(root).unwrap();
    }

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

    #[test]
    fn code_styled_section_citation_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for body in [
            "# X\n\n2. **Mechanism over exhortation** (`AGENTS.md` §3).\n",
            "# X\n\nThe rule is in (`AGENTS.md`§3).\n",
            "# X\n\nAnchor-qualified: (`AGENTS.md` §3.1).\n",
        ] {
            write(root, "docs/adr/0001-x.md", body);
            let err = check_doc_links(root).unwrap_err();
            assert!(
                err.contains("cites a section of the user-owned `AGENTS.md` surface"),
                "unexpected error: {err}"
            );
        }
    }
}
