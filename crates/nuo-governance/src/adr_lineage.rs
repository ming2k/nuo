//! ADR reference lineage checker.
//!
//! Rule source: `docs/dev/standards/adr.md` §4.
//!
//! Enforces:
//!   Every `ADR-NNNN` citation in prose (.md) and comments (.rs) must resolve
//!   to a canonical ADR in `docs/adr/`.
//!   Immutable shipped migration SQL in `crates/nuo-persistence/src/db/migrations.rs`
//!   is explicitly exempt from string-literal scanning.
//!   User-owned surfaces (`AGENTS.md`) are not repository documentation and are never scanned.

use crate::config::GovernanceConfig;
use pulldown_cmark::{Event, Options, Parser, Tag};
use regex::Regex;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use walkdir::WalkDir;

pub fn canonical_numbers(root: &Path, config: &GovernanceConfig) -> HashSet<String> {
    let mut have = HashSet::new();
    let adr_path = root.join(&config.adr.directory);
    let Ok(entries) = fs::read_dir(adr_path) else {
        return have;
    };

    let re = match Regex::new(r"^(\d{4})-.*\.md$") {
        Ok(r) => r,
        Err(_) => return have,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(caps) = re.captures(&name) {
            if let Some(num) = caps.get(1) {
                have.insert(num.as_str().to_string());
            }
        }
    }
    have
}

fn dead_tokens<'a>(text: &'a str, have: &HashSet<String>, token_re: &Regex) -> Vec<&'a str> {
    let mut dead = Vec::new();
    for caps in token_re.captures_iter(text) {
        if let Some(num) = caps.get(1) {
            if !have.contains(num.as_str()) {
                if let Some(full) = caps.get(0) {
                    dead.push(full.as_str());
                }
            }
        }
    }
    dead
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LexState {
    Code,
    Raw(usize),
    Str,
    BlockComment,
}

fn split_rust_line(line: &str, mut state: LexState, scan_re: &Regex) -> (LexState, String, String) {
    let mut i = 0;
    let mut code_end = line.len();
    let mut comment = String::new();

    while i < line.len() {
        match state {
            LexState::Raw(extra) => {
                let close = format!("\"{}", "#".repeat(extra));
                if let Some(pos) = line[i..].find(&close) {
                    state = LexState::Code;
                    i += pos + close.len();
                } else {
                    return (state, line[..code_end].to_string(), comment);
                }
            }
            LexState::Str => {
                let bytes = line.as_bytes();
                let mut closed = false;
                let mut j = i;
                while j < bytes.len() {
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == b'"' {
                        state = LexState::Code;
                        i = j + 1;
                        closed = true;
                        break;
                    }
                    j += 1;
                }
                if !closed {
                    return (state, line[..code_end].to_string(), comment);
                }
            }
            LexState::BlockComment => {
                if let Some(pos) = line[i..].find("*/") {
                    state = LexState::Code;
                    i += pos + 2;
                } else {
                    return (state, String::new(), line.to_string());
                }
            }
            LexState::Code => {
                if let Some(m) = scan_re.find(&line[i..]) {
                    let start = i + m.start();
                    let end = i + m.end();
                    let token = m.as_str();

                    if token.starts_with('r') {
                        let hash_count = token.chars().filter(|c| *c == '#').count();
                        state = LexState::Raw(hash_count);
                        i = end;
                    } else if token == "\"" {
                        state = LexState::Str;
                        i = end;
                    } else if token == "//" {
                        code_end = start;
                        comment = line[start..].to_string();
                        break;
                    } else if token == "/*" {
                        state = LexState::BlockComment;
                        i = end;
                    } else {
                        i = end;
                    }
                } else {
                    break;
                }
            }
        }
    }

    (state, line[..code_end].to_string(), comment)
}

fn get_tracked_files(root: &Path, config: &GovernanceConfig) -> Vec<String> {
    let keep = |rel: &str| !config.docs.user_owned_surfaces.contains(&rel.to_string());

    if let Ok(output) = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "*.rs", "*.md"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return stdout
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .filter(|l| keep(l))
                .map(|l| l.to_string())
                .collect();
        }
    }

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
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if ext == "rs" || ext == "md" {
                if let Ok(rel) = path.strip_prefix(root) {
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    if keep(&rel) {
                        files.push(rel);
                    }
                }
            }
        }
    }
    files.sort();
    files
}

pub fn check_adr_lineage(root: &Path) -> Result<(), String> {
    let config = GovernanceConfig::load_from_workspace(root);
    check_adr_lineage_with_config(root, &config)
}

pub fn check_adr_lineage_with_config(root: &Path, config: &GovernanceConfig) -> Result<(), String> {
    let have = canonical_numbers(root, config);
    let token_re = Regex::new(r"ADR-(\d{4})").map_err(|e| e.to_string())?;
    let scan_re = Regex::new(r#"r(#*)"|"|//|/\*|\*/"#).map_err(|e| e.to_string())?;

    let files = get_tracked_files(root, config);
    let mut failures = Vec::new();

    for rel_path in files {
        let full_path = root.join(&rel_path);
        let content = match fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if rel_path.ends_with(".md") {
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
            let mut in_code_block = false;

            for (event, range) in parser {
                match event {
                    Event::Start(Tag::CodeBlock(..)) => {
                        in_code_block = true;
                    }
                    Event::End(pulldown_cmark::TagEnd::CodeBlock) => {
                        in_code_block = false;
                    }
                    Event::Text(text) | Event::Code(text) if !in_code_block => {
                        for dead in dead_tokens(&text, &have, &token_re) {
                            let line_num = get_line_num(range.start);
                            let preview: String = text.trim().chars().take(110).collect();
                            failures.push(format!("{rel_path}:{line_num}: [{dead}] {preview}"));
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }

        // Rust file parsing
        let mut state = LexState::Code;
        let allow_strings = config
            .adr
            .string_allowlist
            .iter()
            .any(|allow| allow == &rel_path);

        for (idx, line) in content.lines().enumerate() {
            let line_num = idx + 1;
            let (new_state, code, comment) = split_rust_line(line, state, &scan_re);
            state = new_state;

            if dead_tokens(line, &have, &token_re).is_empty() {
                continue;
            }

            if !dead_tokens(&code, &have, &token_re).is_empty() && !allow_strings {
                let preview: String = line.trim().chars().take(110).collect();
                failures.push(format!("{rel_path}:{line_num}: (string literal) {preview}"));
            }
            if !dead_tokens(&comment, &have, &token_re).is_empty() {
                let preview: String = comment.trim().chars().take(110).collect();
                failures.push(format!("{rel_path}:{line_num}: {preview}"));
            }
        }
    }

    if failures.is_empty() {
        println!(
            "ADR lineage: clean ({} canonical ADRs, no dangling citations)",
            have.len()
        );
        Ok(())
    } else {
        Err(format!(
            "ADR lineage: {} dangling citation(s) — a reference must resolve to a file in {}/ (docs/dev/standards/adr.md §4):\n{}",
            failures.len(),
            config.adr.directory,
            failures.join("\n")
        ))
    }
}
