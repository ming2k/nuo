//! Text Overflow and Column Budget Governance Firewall
//! (ADR-0152 `[INV-TEXT-OVERFLOW-01]`, `[INV-ROW-GRID-01]`).
//!
//! Two rules, both aimed at a *bug class* rather than at style:
//!
//! - **`[INV-TEXT-OVERFLOW-01]`** No truncation helper may build its own
//!   ellipsis. A private implementation is how two surfaces come to disagree
//!   about how a string is shortened — one keeping the head of a wire id and the
//!   other its tail. The one implementation is
//!   `nuo_ui_primitives::components::overflow`.
//! - **`[INV-ROW-GRID-01]`** No renderer may size a column by percentage
//!   arithmetic. That is how a long model name came to starve the connection
//!   column: the cap was computed per column with nothing reconciling the sum.
//!   Columns are resolved by `nuo_ui_primitives::components::columns`.
//!
//! The rule is deliberately *narrow*: it fires on an ellipsis built inside a
//! function whose job is to shorten text, not on every ellipsis in the tree. A
//! search snippet's `…`, a theme's glyph table and a marker *matcher* are all
//! legitimate and stay untouched — over-broad governance is how a firewall earns
//! a `#[allow]` and stops protecting anything.

use regex::Regex;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

/// One rule violation, with the evidence a reader needs to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverflowViolation {
    pub file: String,
    pub line: usize,
    pub rule: &'static str,
    pub context: String,
}

/// Check the overflow and column-budget invariants across presentation code.
pub fn check_text_overflow(root: &Path) -> Result<(), String> {
    let scan_dirs = [
        root.join("crates/nuo/src/shell"),
        root.join("crates/nuo-view/src"),
    ];

    // A hand-built ellipsis: appending the marker by hand, or formatting it in.
    let ellipsis_build_re = Regex::new(
        r#"(?x)
            push \s* \( \s* '…' \s*\)
          | push_str \s* \( \s* "…" \s*\)
          | insert \s* \( \s* 0 \s*, \s* '…' \s*\)
          | format! \s* \( \s* "\{\}\.\.\." 
          | \+ \s* "…"
        "#,
    )
    .map_err(|e| e.to_string())?;

    // Column arithmetic done in a renderer: a percentage of the body width.
    let inline_share_re = Regex::new(r"body_width\s*\*\s*\d+\s*\+\s*\d+\s*\)\s*/\s*100")
        .map_err(|e| e.to_string())?;

    // The declaration of the function currently being scanned.
    let fn_decl_re = Regex::new(r"\bfn\s+([A-Za-z0-9_]+)").map_err(|e| e.to_string())?;

    let mut violations = Vec::new();

    for dir in &scan_dirs {
        if !dir.exists() {
            continue;
        }

        for entry in WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
        {
            let path_str = entry.path().to_string_lossy();
            if path_str.contains("/tests/")
                || path_str.ends_with("_tests.rs")
                || path_str.ends_with("tests.rs")
                || path_str.contains("snapshot_tests.rs")
            {
                continue;
            }

            let Ok(content) = fs::read_to_string(entry.path()) else {
                continue;
            };

            let mut in_cfg_test = false;
            let mut current_fn: Option<String> = None;

            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("mod tests {") {
                    in_cfg_test = true;
                }
                if in_cfg_test {
                    continue;
                }
                if trimmed.starts_with("//") || trimmed.starts_with('*') {
                    continue;
                }

                if let Some(caps) = fn_decl_re.captures(trimmed) {
                    current_fn = caps.get(1).map(|m| m.as_str().to_string());
                }

                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .to_string();

                let shortens_text = current_fn
                    .as_deref()
                    .is_some_and(|name| name.starts_with("truncate") || name.starts_with("clamp"));
                if shortens_text && ellipsis_build_re.is_match(trimmed) {
                    violations.push(OverflowViolation {
                        file: rel.clone(),
                        line: idx + 1,
                        rule: "[INV-TEXT-OVERFLOW-01]",
                        context: trimmed.to_string(),
                    });
                }
                if inline_share_re.is_match(trimmed) {
                    violations.push(OverflowViolation {
                        file: rel,
                        line: idx + 1,
                        rule: "[INV-ROW-GRID-01]",
                        context: trimmed.to_string(),
                    });
                }
            }
        }
    }

    if violations.is_empty() {
        return Ok(());
    }

    let mut msg = String::from(
        "Text Overflow / Column Budget Firewall Violations (ADR-0152):\n\
         [INV-TEXT-OVERFLOW-01] A truncation helper must not build its own \
         ellipsis. Route through \
         `nuo_ui_primitives::components::overflow` (`fit`, `fit_with`, \
         `fit_line`) and let it place the marker.\n\
         [INV-ROW-GRID-01] Column widths must come from \
         `nuo_ui_primitives::components::columns` (`Grid::solve`), never from \
         percentage arithmetic inside a renderer.\n\n",
    );
    for v in violations {
        msg.push_str(&format!(
            "  --> {}:{}  {}\n      {}\n\n",
            v.file, v.line, v.rule, v.context
        ));
    }
    Err(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ellipsis_re() -> Regex {
        Regex::new(
            r#"(?x)
            push \s* \( \s* '…' \s*\)
          | push_str \s* \( \s* "…" \s*\)
          | insert \s* \( \s* 0 \s*, \s* '…' \s*\)
          | format! \s* \( \s* "\{\}\.\.\." 
          | \+ \s* "…"
        "#,
        )
        .unwrap()
    }

    #[test]
    fn a_hand_built_ellipsis_is_recognised() {
        let re = ellipsis_re();
        assert!(re.is_match("    out.push('…');"));
        assert!(re.is_match("    out.push_str(\"…\");"));
        assert!(re.is_match("        lead.insert(0, '…');"));
        assert!(re.is_match("        format!(\"{}...\", prefix)"));
        // Delegating to the one implementation is not a violation.
        assert!(!re.is_match("    let fitted = fit(text, budget, overflow);"));
        assert!(!re.is_match("    .text"));
    }

    #[test]
    fn only_truncation_helpers_are_judged() {
        // The rule is narrow on purpose: a marker *matcher* and a data-model
        // ellipsis are legitimate, and an over-broad firewall stops protecting
        // anything because it earns an `#[allow]`.
        let re = ellipsis_re();
        let mut current_fn = Some("is_truncation_marker".to_string());
        let judged = |f: &Option<String>| {
            f.as_deref()
                .is_some_and(|n| n.starts_with("truncate") || n.starts_with("clamp"))
        };
        assert!(!judged(&current_fn));
        assert!(!re.is_match("    const MARKERS: &[&str] = &["));
        current_fn = Some("truncate_with".to_string());
        assert!(judged(&current_fn));
    }

    #[test]
    fn renderer_side_percentage_column_math_is_rejected() {
        let re = Regex::new(r"body_width\s*\*\s*\d+\s*\+\s*\d+\s*\)\s*/\s*100").unwrap();
        assert!(re.is_match("let conn_cap = ((body_width * 20 + 50) / 100).max(8);"));
        assert!(!re.is_match("ColumnSpec::new(name_demand, 16, 34)"));
    }
}
