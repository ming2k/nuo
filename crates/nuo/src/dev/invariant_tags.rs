//! Invariant-tag uniqueness checker.
//!
//! Rule source: `docs/dev/standards/invariants.md` §2 (identifier policy) and §4
//! (in-place lifecycle).
//!
//! # The failure this closes
//!
//! An `[INV-*]` tag is a *citation target*. When two ADRs define the same tag
//! for different rules, every later citation becomes ambiguous, and the tag stops
//! pointing at one meaning — which is exactly what an invariant must never do.
//!
//! This is not hypothetical. While completing the codec aperture (ADR-0143) two
//! tags were minted into a live ADR without checking, and both collided:
//! `[INV-WIRE-01]` already belonged to ADR-0005's zero-I/O rule, `[INV-LANE-01]`
//! to ADR-0014's three-lane exclusivity. A hand audit caught them; nothing in the
//! build would have. Separately, ADR-0034 and ADR-0045 had defined the same
//! `[INV-LIFECYCLE-03]` for unrelated rules for months, with the ADR index and
//! CHANGELOG citing the tag without saying which rule they meant.
//!
//! # What counts as a definition
//!
//! A definition is the line that *introduces* the rule, in the shape the ADR
//! template prescribes — a bullet whose bold lead is the tag, followed by the
//! rule's title:
//!
//! ```text
//! - **[INV-IMG-01] Content is the authority when published**: …
//! ```
//!
//! Citations (`see [INV-IMG-01]`, `**Unified Takeover Escalation
//! ([INV-LIFECYCLE-03])**`, table cells, code comments) are deliberately not
//! definitions: one tag is cited dozens of times by design, and a checker that
//! flagged those would be noise rather than a gate.
//!
//! Only `docs/` is scanned. A tag's authority is an ADR; a `.rs` comment may cite
//! one but may never mint one, and the rustdoc on the defining type is not the
//! registry.

use regex::Regex;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

/// Scan scope: the documentation tree, where invariant authority lives.
const DOCS_DIR: &str = "docs";

/// One place a tag is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub path: String,
    pub line: usize,
    /// The rule's short title, the human discriminator between two rules that
    /// once shared a tag (`Unified Takeover Escalation` vs `Intent-Driven Server
    /// Handoff`).
    pub title: String,
    /// The rule statement that follows the title.
    pub rule: String,
}

/// Check that every `[INV-*]` tag is defined at most once across the tree.
pub fn check_invariant_tags(root: &Path) -> Result<(), String> {
    let definitions = collect_definitions(root);
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

/// Every `[INV-*]` definition in `docs/`, keyed by tag.
pub fn collect_definitions(root: &Path) -> BTreeMap<String, Vec<Definition>> {
    let Ok(def_re) =
        Regex::new(r"^\s*[-*]\s*\*\*\[?(INV-[A-Z0-9][A-Z0-9-]*)\]?(.*?)\*\*\s*[:：]\s*(.*)$")
    else {
        return BTreeMap::new();
    };

    let mut found: BTreeMap<String, Vec<Definition>> = BTreeMap::new();
    let docs = root.join(DOCS_DIR);
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
        for (index, line) in content.lines().enumerate() {
            let Some(caps) = def_re.captures(line) else {
                continue;
            };
            let (Some(tag), Some(title), Some(rule)) = (caps.get(1), caps.get(2), caps.get(3))
            else {
                continue;
            };
            found
                .entry(tag.as_str().to_string())
                .or_default()
                .push(Definition {
                    path: relative.clone(),
                    line: index + 1,
                    title: title.as_str().trim().to_string(),
                    rule: rule.as_str().trim().to_string(),
                });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
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
        // The message must let a reader tell the two rules apart without
        // opening either file.
        assert!(error.contains("Unified Takeover Escalation"), "{error}");
        assert!(error.contains("Intent-Driven Server Handoff"), "{error}");
    }

    #[test]
    fn citations_are_not_definitions() {
        // One tag, cited many ways, defined once: this is the normal shape and
        // must not be flagged. A checker that counted citations would make the
        // gate unusable.
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
        let definitions = collect_definitions(dir.path());
        assert_eq!(definitions["INV-IMG-01"].len(), 1, "one definition only");
        assert!(check_invariant_tags(dir.path()).is_ok());
    }

    #[test]
    fn definitions_split_across_files_are_attributed() {
        let dir = tree(&[
            ("docs/adr/0001-a.md", "- **[INV-X-01] Alpha**: a\n"),
            ("docs/other/notes.md", "- **[INV-X-01] Beta**: b\n"),
        ]);
        let definitions = collect_definitions(dir.path());
        let sites = &definitions["INV-X-01"];
        assert_eq!(sites.len(), 2);
        assert!(sites.iter().any(|s| s.path == "docs/adr/0001-a.md"));
        assert!(sites.iter().any(|s| s.path == "docs/other/notes.md"));
    }

    #[test]
    fn rust_sources_cannot_mint_tags() {
        // Authority is an ADR. A `.rs` rustdoc that writes the definition shape
        // is not the registry, and must not create a second site.
        let dir = tree(&[
            ("docs/adr/0001-a.md", "- **[INV-X-01] Alpha**: a\n"),
            (
                "src/lib.rs",
                "/// - **[INV-X-01] From code**: not a definition\n",
            ),
        ]);
        let found: BTreeMap<String, Vec<Definition>> = collect_definitions(dir.path());
        assert_eq!(found["INV-X-01"].len(), 1);
    }
}
