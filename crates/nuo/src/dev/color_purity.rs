//! Semantic Color Purity and Anti-Drift Governance Firewall (ADR-0147 `[INV-TUI-COLOR-01]`).
//!
//! Enforces:
//!   - Presentation code in `crates/nuo/src/shell`, `crates/nuo-view/src`, and
//!     `crates/nuo-ui-primitives/src/components` must never write hardcoded color
//!     literals (`Color::Rgb`, `Color::White`, `Color::Red`, etc.).
//!   - All visual styling must resolve through `Theme` semantic tokens.
//!   - Only `Color::Reset` (attribute clear) and pattern extraction are permitted.

use regex::Regex;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

/// Violation record for hardcoded dirty color writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorPurityViolation {
    pub file: String,
    pub line: usize,
    pub literal: String,
    pub context: String,
}

/// Check semantic color purity across presentation layers.
pub fn check_color_purity(root: &Path) -> Result<(), String> {
    let scan_dirs = [
        root.join("crates/nuo/src/shell"),
        root.join("crates/nuo-view/src"),
        root.join("crates/nuo-ui-primitives/src/components"),
    ];

    // Forbidden color constructors and literal tokens (excluding Color::Reset)
    let forbidden_re = Regex::new(
        r"\b(?:nuotc::)?Color::(Rgb\s*\(\s*\d+|White|Black|Red|Green|Yellow|Blue|Magenta|Cyan|Gray|DarkGray|Light\w+)",
    ).map_err(|e| e.to_string())?;

    let pattern_match_re =
        Regex::new(r"Color::Rgb\s*\([a-zA-Z0-9_,\s\.]+\)\s*=>").map_err(|e| e.to_string())?;

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
            // Skip dedicated test files and snapshot assertion helpers
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

            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim();

                // Track test modules
                if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("mod tests {") {
                    in_cfg_test = true;
                }
                if in_cfg_test {
                    continue;
                }

                // Skip comments
                if trimmed.starts_with("//")
                    || trimmed.starts_with("/*")
                    || trimmed.starts_with('*')
                {
                    continue;
                }

                // Check for pattern matches which are destructuring rather than dirty writes
                if pattern_match_re.is_match(trimmed) || trimmed.contains("matches!(") {
                    continue;
                }

                if let Some(caps) = forbidden_re.captures(trimmed) {
                    let literal = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
                    let rel_path = entry
                        .path()
                        .strip_prefix(root)
                        .unwrap_or(entry.path())
                        .to_string_lossy()
                        .to_string();

                    violations.push(ColorPurityViolation {
                        file: rel_path,
                        line: idx + 1,
                        literal: literal.to_string(),
                        context: trimmed.to_string(),
                    });
                }
            }
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        let mut msg = String::from(
            "Color Purity Firewall Violations (ADR-0147 `[INV-TUI-COLOR-01]`):\n\
             Direct hardcoding of Color::* literals is forbidden in presentation components.\n\
             All styling must resolve through `Theme` semantic tokens (e.g. `theme.text()`, `theme.brand()`, `theme.ok()`).\n\n",
        );
        for v in violations {
            msg.push_str(&format!(
                "  --> {}:{}\n      Found: {}\n      Line:  {}\n\n",
                v.file, v.line, v.literal, v.context
            ));
        }
        Err(msg)
    }
}
