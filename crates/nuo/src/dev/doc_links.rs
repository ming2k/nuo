//! Markdown documentation hyperlink integrity checker.
//!
//! Rule source: `docs/dev/standards/style.md` §1.
//!
//! Enforces:
//!   1. Target existence (style.md §1.3)
//!   2. No absolute locators (style.md §1.2)
//!   3. Anchor existence (style.md §1.4)

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const EXTERNAL: &[&str] = &["http://", "https://", "mailto:", "tel:", "data:"];

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
