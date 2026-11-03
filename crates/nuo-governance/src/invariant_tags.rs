//! Invariant-tag uniqueness checker.
//!
//! Rule source: `docs/dev/standards/invariants.md` §2 (identifier policy) and §4
//! (in-place lifecycle).
//!
//! Enforces:
//! Every `[INV-*]` tag is defined at most once across the `docs/` tree.

use crate::config::GovernanceConfig;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use regex::Regex;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

/// One place a tag is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub path: String,
    pub line: usize,
    /// The rule's short title, the human discriminator between two rules that
    /// once shared a tag.
    pub title: String,
    /// The rule statement that follows the title.
    pub rule: String,
}

pub fn check_invariant_tags(root: &Path) -> Result<(), String> {
    let config = GovernanceConfig::load_from_workspace(root);
    check_invariant_tags_with_config(root, &config)
}

pub fn check_invariant_tags_with_config(
    root: &Path,
    config: &GovernanceConfig,
) -> Result<(), String> {
    let definitions = collect_definitions(root, config);
    let mut collisions = BTreeMap::new();
    for (tag, sites) in &definitions {
        if sites.len() > 1 {
            collisions.insert(tag.clone(), sites.clone());
        }
    }
    if collisions.is_empty() {
        return Ok(());
    }

    let mut message = String::from(
        "Invariant tag collisions — a tag is a citation target and must name exactly one rule \
         (docs/dev/standards/invariants.md §2):\n",
    );
    for (tag, sites) in collisions {
        message.push_str(&format!("\n[{tag}] is defined {} times:\n", sites.len()));
        for site in sites {
            message.push_str(&format!(
                "  {}:{}\n      title: {}\n      rule:  {}\n",
                site.path, site.line, site.title, site.rule
            ));
        }
        message.push_str(
            "  Fix: give the newer rule its own tag, or record the supersession in place \
             (invariants.md §4) and stop re-using the tag.\n",
        );
    }
    Err(message)
}

/// Collect all `[INV-*]` definitions from the documentation tree.
pub fn collect_definitions(
    root: &Path,
    config: &GovernanceConfig,
) -> BTreeMap<String, Vec<Definition>> {
    let tag_re = match Regex::new(r"^\[?(INV-[A-Z0-9][A-Z0-9-]*)\]?(.*)$") {
        Ok(r) => r,
        Err(_) => return BTreeMap::new(),
    };

    let mut found: BTreeMap<String, Vec<Definition>> = BTreeMap::new();
    let docs = root.join(&config.invariants.directory);
    if !docs.exists() {
        return found;
    }

    for entry in WalkDir::new(&docs)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
    {
        let Ok(content) = fs::read_to_string(entry.path()) else {
            continue;
        };

        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");

        let line_starts: Vec<usize> = std::iter::once(0)
            .chain(content.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let get_line_num = |offset: usize| -> usize {
            match line_starts.binary_search(&offset) {
                Ok(idx) => idx + 1,
                Err(idx) => idx,
            }
        };

        let parser = Parser::new_ext(&content, Options::all()).into_offset_iter();
        let events: Vec<(Event, std::ops::Range<usize>)> = parser.collect();

        let mut in_list_item = false;
        let mut first_inline_in_item = false;
        let mut in_strong = false;
        let mut strong_has_code = false;
        let mut strong_text = String::new();
        let mut strong_offset = 0;

        for i in 0..events.len() {
            let (event, range) = &events[i];
            match event {
                Event::Start(Tag::Item) => {
                    in_list_item = true;
                    first_inline_in_item = true;
                }
                Event::End(TagEnd::Item) => {
                    in_list_item = false;
                    first_inline_in_item = false;
                }
                Event::Start(Tag::Strong) if in_list_item && first_inline_in_item => {
                    in_strong = true;
                    strong_has_code = false;
                    strong_text.clear();
                    strong_offset = range.start;
                }
                Event::Code(_) if in_strong => {
                    strong_has_code = true;
                }
                Event::Text(t) if in_strong => {
                    strong_text.push_str(t);
                }
                Event::End(TagEnd::Strong) if in_strong => {
                    in_strong = false;
                    first_inline_in_item = false;

                    if !strong_has_code {
                        let trimmed_strong = strong_text.trim();
                        if let Some(caps) = tag_re.captures(trimmed_strong) {
                            let tag = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
                            let title = caps
                                .get(2)
                                .map(|m| m.as_str())
                                .unwrap_or_default()
                                .trim()
                                .to_string();

                            // Check if immediately followed by `: ` or `：` and rule text
                            if let Some((Event::Text(after_text), _)) = events.get(i + 1) {
                                let after_trimmed = after_text.trim_start();
                                if after_trimmed.starts_with(':') || after_trimmed.starts_with('：')
                                {
                                    let rule = after_trimmed[1..].trim().to_string();
                                    let line = get_line_num(strong_offset);

                                    found.entry(tag.to_string()).or_default().push(Definition {
                                        path: relative.clone(),
                                        line,
                                        title,
                                        rule,
                                    });
                                }
                            }
                        }
                    }
                }
                Event::Start(Tag::Paragraph) if in_list_item => {
                    // Paragraph inside item, next should be inline elements
                }
                Event::Text(t) if in_list_item && first_inline_in_item && !t.trim().is_empty() => {
                    // Non-whitespace text before strong tag, not a definition lead
                    first_inline_in_item = false;
                }
                _ => {}
            }
        }
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (relative, content) in files {
            let path = dir.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("mkdir");
            }
            let mut file = fs::File::create(&path).expect("create");
            file.write_all(content.as_bytes()).expect("write");
        }
        dir
    }

    #[test]
    fn a_tag_defined_once_is_accepted() {
        let dir = tree(&[(
            "docs/adr/0001-a.md",
            "- **[INV-IMG-01] Content is the authority**: rule text\n",
        )]);
        assert!(check_invariant_tags(dir.path()).is_ok());
    }

    #[test]
    fn the_same_tag_defined_twice_is_rejected() {
        let dir = tree(&[
            (
                "docs/adr/0034-a.md",
                "- **[INV-LIFECYCLE-03] Unified Takeover Escalation**: one rule\n",
            ),
            (
                "docs/adr/0045-b.md",
                "- **[INV-LIFECYCLE-03] Intent-Driven Server Handoff**: another rule\n",
            ),
        ]);
        let error = check_invariant_tags(dir.path()).expect_err("collision must fail");
        assert!(error.contains("INV-LIFECYCLE-03"), "{error}");
        assert!(error.contains("Unified Takeover Escalation"), "{error}");
        assert!(error.contains("Intent-Driven Server Handoff"), "{error}");
    }

    #[test]
    fn citations_are_not_definitions() {
        let dir = tree(&[(
            "docs/adr/0034-a.md",
            concat!(
                "- **[INV-IMG-01] Content is the authority**: the rule\n",
                "\n",
                "See [INV-IMG-01] for details.\n",
                "- **Unified Takeover Escalation ([INV-IMG-01])**: a citation in a bold lead\n",
                "| [INV-IMG-01] | table citation |\n",
            ),
        )]);
        let config = GovernanceConfig::default();
        let definitions = collect_definitions(dir.path(), &config);
        assert_eq!(definitions["INV-IMG-01"].len(), 1, "one definition only");
        assert!(check_invariant_tags(dir.path()).is_ok());
    }

    #[test]
    fn definitions_split_across_files_are_attributed() {
        let dir = tree(&[
            ("docs/adr/0001-a.md", "- **[INV-X-01] Alpha**: a\n"),
            ("docs/other/notes.md", "- **[INV-X-01] Beta**: b\n"),
        ]);
        let config = GovernanceConfig::default();
        let definitions = collect_definitions(dir.path(), &config);
        let sites = &definitions["INV-X-01"];
        assert_eq!(sites.len(), 2);
        assert!(sites.iter().any(|s| s.path == "docs/adr/0001-a.md"));
        assert!(sites.iter().any(|s| s.path == "docs/other/notes.md"));
    }

    #[test]
    fn rust_sources_cannot_mint_tags() {
        let dir = tree(&[
            ("docs/adr/0001-a.md", "- **[INV-X-01] Alpha**: a\n"),
            (
                "src/lib.rs",
                "/// - **[INV-X-01] From code**: not a definition\n",
            ),
        ]);
        let config = GovernanceConfig::default();
        let found = collect_definitions(dir.path(), &config);
        assert_eq!(found["INV-X-01"].len(), 1);
    }
}
