//! Semantic and adaptive path formatting and rendering component.
//!
//! Provides intelligent path shortening (fish-style abbreviation, middle-ellipsis,
//! relative-to-workspace / tilde resolution, filename-preserving truncation)
//! and rich Ratatui span generation with semantic hierarchy (muted directories,
//! bold filenames, distinct extensions, highlighted line/column numbers).

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use nuotc::{Line, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// Strategy for shortening paths when budget is constrained or a specific style is preferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub enum PathFormatStrategy {
    /// Full path without shortening (except base_dir / tilde relative resolution).
    Full,
    /// Progressive contraction of ancestor directories:
    /// ellipsis form first (`.../components/path.rs`), then fish-style
    /// abbreviation (`c/m/s/c/path.rs`) as a deeper fallback.
    Fish,
    /// Ellipsis for middle directories (e.g. `crates/.../components/path.rs`).
    MiddleEllipsis,
    /// Direct parent + filename only (e.g. `.../components/path.rs`).
    BasenameWithParent,
    /// Only the filename (e.g. `path.rs`).
    BasenameOnly,
    /// Automatically selects the most descriptive strategy that fits within the width budget.
    #[default]
    Adaptive,
}

/// Visual styling rules for rendering paths into styled Ratatui spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub enum PathStyle {
    /// Rich two-tone / multi-tone semantic styling:
    /// - Ancestor directories and separators: muted / dim.
    /// - File stem: primary / bold.
    /// - Extension: muted accent.
    /// - Line/column suffix (e.g. `:42:10`): info / brand highlight.
    #[default]
    Semantic,
    /// Muted / secondary style for the entire path.
    Muted,
    /// Uniform custom style.
    Plain(Style),
    /// Custom styling for each segment.
    Custom {
        dir: Style,
        stem: Style,
        ext: Style,
        line_col: Style,
    },
}

/// A builder and presenter for displaying filesystem paths cleanly in the TUI.
#[derive(Clone, Debug)]
pub struct PathView<'a> {
    raw: Cow<'a, str>,
    base_dir: Option<PathBuf>,
    max_width: Option<usize>,
    strategy: PathFormatStrategy,
    style: PathStyle,
    normalize_slash: bool,
}

#[allow(dead_code)]
impl<'a> PathView<'a> {
    /// Create a new `PathView` from a `&Path` or path string.
    pub fn new<P: AsRef<Path> + ?Sized>(path: &'a P) -> Self {
        let path_ref = path.as_ref();
        let raw = path_ref.to_string_lossy();
        Self {
            raw,
            base_dir: None,
            max_width: None,
            strategy: PathFormatStrategy::Adaptive,
            style: PathStyle::Semantic,
            normalize_slash: true,
        }
    }

    /// Create a new `PathView` from a raw string slice (which may include line:column numbers).
    pub fn from_str(path_str: &'a str) -> Self {
        Self {
            raw: Cow::Borrowed(path_str),
            base_dir: None,
            max_width: None,
            strategy: PathFormatStrategy::Adaptive,
            style: PathStyle::Semantic,
            normalize_slash: true,
        }
    }

    /// Set the base directory (e.g. workspace root / current working directory)
    /// against which absolute paths are made relative.
    pub fn base_dir(mut self, base: &Path) -> Self {
        self.base_dir = Some(base.to_path_buf());
        self
    }

    /// Set the base directory if provided as an `Option`.
    pub fn maybe_base_dir(mut self, base: Option<&Path>) -> Self {
        self.base_dir = base.map(Path::to_path_buf);
        self
    }

    /// Convert into an owned [`PathView<'static>`].
    pub fn into_owned(self) -> PathView<'static> {
        PathView {
            raw: Cow::Owned(self.raw.into_owned()),
            base_dir: self.base_dir,
            max_width: self.max_width,
            strategy: self.strategy,
            style: self.style,
            normalize_slash: self.normalize_slash,
        }
    }

    /// Set the maximum display column width budget.
    pub fn max_width(mut self, width: usize) -> Self {
        self.max_width = Some(width);
        self
    }

    /// Set the maximum display width if provided as an `Option`.
    pub fn maybe_max_width(mut self, width: Option<usize>) -> Self {
        self.max_width = width;
        self
    }

    /// Set the formatting strategy.
    pub fn strategy(mut self, strategy: PathFormatStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Set the visual styling rule.
    pub fn style(mut self, style: PathStyle) -> Self {
        self.style = style;
        self
    }

    /// Get the current visual styling rule.
    pub fn path_style(&self) -> PathStyle {
        self.style
    }

    /// Whether to normalize backslashes `\` to forward slashes `/` for clean terminal display.
    /// Defaults to `true`.
    pub fn normalize_slash(mut self, normalize: bool) -> Self {
        self.normalize_slash = normalize;
        self
    }

    /// Format the path into a plain string according to configured options and width constraints.
    pub fn format_text(&self) -> String {
        format_path_str(
            &self.raw,
            self.base_dir.as_deref(),
            self.max_width,
            self.strategy,
            self.normalize_slash,
        )
    }

    /// Render the path as a list of styled Ratatui `Span`s.
    pub fn to_spans(&self, theme: &Theme) -> Vec<Span<'static>> {
        let text = self.format_text();
        render_path_spans(&text, self.style, theme)
    }

    /// Render the path as a Ratatui `Line`.
    pub fn to_line(&self, theme: &Theme) -> Line<'static> {
        Line::from(self.to_spans(theme))
    }
}

/// Parse optional trailing `:line` or `:line:col` from a path string.
pub fn split_line_col(s: &str) -> (&str, Option<&str>) {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return (s, None);
    }

    let mut col_pos = None;
    let mut line_pos = None;

    // Scan backwards from end for :digits[:digits]
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        if bytes[i] == b':' {
            if let Some(cp) = col_pos {
                if line_pos.is_none() {
                    let seg = &s[i + 1..cp];
                    if !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()) {
                        line_pos = Some(i);
                    }
                    break;
                }
            } else {
                // Check if segment after colon is all digits
                let seg = &s[i + 1..];
                if !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()) {
                    col_pos = Some(i);
                    continue;
                } else {
                    break;
                }
            }
        }
    }

    if let Some(pos) = line_pos {
        (&s[..pos], Some(&s[pos..]))
    } else if let Some(pos) = col_pos {
        (&s[..pos], Some(&s[pos..]))
    } else {
        (s, None)
    }
}

/// Convert an absolute path to be relative to `base_dir`, or relative to `$HOME` (`~`), or fallback.
pub fn normalize_path_relative(path_str: &str, base_dir: Option<&Path>) -> String {
    // Expand a leading `~` back to `$HOME` first, so home-rooted paths can
    // still be made workspace-relative when they fall under `base_dir`.
    let p = tilde_expand(Path::new(path_str));
    if let Some(base) = base_dir
        && p.is_absolute()
        && base.is_absolute()
        && let Ok(rel) = p.strip_prefix(base)
    {
        if rel.as_os_str().is_empty() {
            return ".".to_string();
        }
        return rel.display().to_string();
    }

    // Try tilde home shortening
    tilde_shorten(&p)
}

/// Expand a leading `~` / `~/` prefix to the user's home directory.
/// `~user`-style paths are returned untouched.
pub fn tilde_expand(path: &Path) -> PathBuf {
    let home = dirs::home_dir().or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    tilde_expand_with(path, home.as_deref())
}

fn tilde_expand_with(path: &Path, home: Option<&Path>) -> PathBuf {
    let s = path.as_os_str().to_string_lossy();
    if s != "~" && !s.starts_with("~/") {
        return path.to_path_buf();
    }
    let Some(home) = home else {
        return path.to_path_buf();
    };
    if s == "~" {
        return home.to_path_buf();
    }
    home.join(&s[2..])
}

/// Abbreviate an absolute path to `~`-rooted form if under user's home directory.
pub fn tilde_shorten(path: &Path) -> String {
    let home = dirs::home_dir().or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    if let Some(home) = home
        && path.starts_with(&home)
        && let Ok(rest) = path.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return PathBuf::from("~").join(rest).display().to_string();
    }
    path.display().to_string()
}

/// Format a path string according to the requested strategy and width budget.
pub fn format_path_str(
    raw: &str,
    base_dir: Option<&Path>,
    max_width: Option<usize>,
    strategy: PathFormatStrategy,
    normalize_slash: bool,
) -> String {
    let (path_part, line_col_part) = split_line_col(raw);
    let mut normalized = normalize_path_relative(path_part, base_dir);
    if normalize_slash {
        normalized = normalized.replace('\\', "/");
    }

    let line_col = line_col_part.unwrap_or("");
    let line_col_w = UnicodeWidthStr::width(line_col);

    let budget = match max_width {
        Some(w) => w,
        None => {
            // No budget limit: apply strategy directly
            return match strategy {
                PathFormatStrategy::Full | PathFormatStrategy::Adaptive => {
                    format!("{}{}", normalized, line_col)
                }
                PathFormatStrategy::Fish => {
                    format!("{}{}", fish_shorten_path(&normalized), line_col)
                }
                PathFormatStrategy::MiddleEllipsis => {
                    format!(
                        "{}{}",
                        middle_ellipsis_path(&normalized, usize::MAX),
                        line_col
                    )
                }
                PathFormatStrategy::BasenameWithParent => {
                    format!("{}{}", basename_with_parent(&normalized), line_col)
                }
                PathFormatStrategy::BasenameOnly => {
                    format!("{}{}", basename_only(&normalized), line_col)
                }
            };
        }
    };

    if budget == 0 {
        return String::new();
    }

    let total_w = UnicodeWidthStr::width(normalized.as_str()) + line_col_w;
    if total_w <= budget && strategy == PathFormatStrategy::Adaptive {
        return format!("{}{}", normalized, line_col);
    }

    let path_budget = budget.saturating_sub(line_col_w);

    let shortened_path = match strategy {
        PathFormatStrategy::Full => normalized,
        PathFormatStrategy::Fish => fish_shorten_path(&normalized),
        PathFormatStrategy::MiddleEllipsis => middle_ellipsis_path(&normalized, path_budget),
        PathFormatStrategy::BasenameWithParent => basename_with_parent(&normalized),
        PathFormatStrategy::BasenameOnly => basename_only(&normalized),
        PathFormatStrategy::Adaptive => adaptive_shorten(&normalized, path_budget),
    };

    let result = format!("{}{}", shortened_path, line_col);
    let result_w = UnicodeWidthStr::width(result.as_str());
    if result_w <= budget {
        result
    } else {
        // Last-ditch middle-truncate to guarantee strict fitting
        truncate_middle(&result, budget)
    }
}

/// Decompose a path string into directory segments and filename.
fn split_segments(path_str: &str) -> (Option<&str>, Vec<&str>, &str) {
    let mut s = path_str;
    let mut prefix = None;

    if s.starts_with("~/") {
        prefix = Some("~/");
        s = &s[2..];
    } else if s.starts_with('/') {
        prefix = Some("/");
        s = &s[1..];
    } else if s.starts_with("./") {
        prefix = Some("./");
        s = &s[2..];
    }

    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return (prefix, Vec::new(), "");
    }

    let (dir_segments, filename) = parts.split_at(parts.len() - 1);
    (prefix, dir_segments.to_vec(), filename[0])
}

/// Extract just the filename / leaf segment of a path.
pub fn basename_only(path_str: &str) -> String {
    let (_, _, filename) = split_segments(path_str);
    if filename.is_empty() {
        path_str.to_string()
    } else {
        filename.to_string()
    }
}

/// Extract direct parent and filename (e.g. `.../components/path.rs` or `components/path.rs`).
pub fn basename_with_parent(path_str: &str) -> String {
    let (prefix, dirs, filename) = split_segments(path_str);
    let Some(parent) = dirs.last() else {
        return format!("{}{}", prefix.unwrap_or(""), filename);
    };
    if dirs.len() > 1 || prefix.is_some() {
        format!(".../{}/{}", parent, filename)
    } else {
        format!("{}/{}", parent, filename)
    }
}

/// Shorten a directory segment to its minimal / fish-style prefix.
/// e.g. `components` -> `c`, `.config` -> `.c`, `src` -> `s`.
fn fish_segment(seg: &str) -> String {
    let mut chars = seg.chars();
    if let Some(first) = chars.next() {
        if first == '.' {
            if let Some(second) = chars.next() {
                format!(".{}", second)
            } else {
                ".".to_string()
            }
        } else {
            first.to_string()
        }
    } else {
        String::new()
    }
}

/// Shorten ancestor directories in fish-shell style (e.g. `c/m/s/c/path.rs`).
pub fn fish_shorten_path(path_str: &str) -> String {
    let (prefix, dirs, filename) = split_segments(path_str);
    if dirs.is_empty() {
        return format!("{}{}", prefix.unwrap_or(""), filename);
    }

    let mut out = String::new();
    if let Some(p) = prefix {
        out.push_str(p);
    }

    for dir in dirs {
        out.push_str(&fish_segment(dir));
        out.push('/');
    }
    out.push_str(filename);
    out
}

/// Middle ellipsis path shortening (e.g. `crates/.../components/path.rs`).
pub fn middle_ellipsis_path(path_str: &str, max_width: usize) -> String {
    let (prefix, dirs, filename) = split_segments(path_str);
    if dirs.len() <= 2 {
        return path_str.to_string();
    }

    let (Some(first), Some(last)) = (dirs.first(), dirs.last()) else {
        return path_str.to_string();
    };
    let p = prefix.unwrap_or("");
    let candidate = format!("{}{}/.../{}/{}", p, first, last, filename);
    if UnicodeWidthStr::width(candidate.as_str()) <= max_width {
        return candidate;
    }

    // Otherwise `.../last/filename`
    format!(".../{}/{}", last, filename)
}

/// Adaptive shortening engine that smoothly degrades path representation to fit within `budget`.
pub fn adaptive_shorten(path_str: &str, budget: usize) -> String {
    let full_w = UnicodeWidthStr::width(path_str);
    if full_w <= budget {
        return path_str.to_string();
    }

    if budget <= 3 {
        return truncate_middle(path_str, budget);
    }

    let (prefix, dirs, filename) = split_segments(path_str);
    if dirs.is_empty() {
        return middle_truncate_filename(filename, budget);
    }

    // Stage 1: Try `.../parent/filename` — readability-first ellipsis form
    // (rustc/IDE convention). Preferred over lossless fish shortening since
    // the full path is always available in the tool payload.
    let parent_candidate = basename_with_parent(path_str);
    if UnicodeWidthStr::width(parent_candidate.as_str()) <= budget {
        return parent_candidate;
    }

    // Stage 2: Try lossless fish shortening (`c/m/s/c/path.rs`), expanding
    // directory segments from right-to-left to maximize readability. Used as
    // a fallback that retains more hierarchy than the ellipsis form.
    // Full fish representation: `prefix + dir[0].fish + ... + dir[n].fish + filename`
    let fish_dirs: Vec<String> = dirs.iter().map(|d| fish_segment(d)).collect();
    let p = prefix.unwrap_or("");

    // Calculate base fish length
    let base_fish_len =
        p.len() + fish_dirs.iter().map(|d| d.len() + 1).sum::<usize>() + filename.len();

    if base_fish_len <= budget {
        // We have extra budget: expand directory segments from right-to-left
        let mut expanded_dirs: Vec<String> = fish_dirs;
        let mut current_len = base_fish_len;

        for i in (0..dirs.len()).rev() {
            let full_dir = dirs[i];
            let fish_dir_len = expanded_dirs[i].len();
            let additional = full_dir.len().saturating_sub(fish_dir_len);
            if current_len + additional <= budget {
                expanded_dirs[i] = full_dir.to_string();
                current_len += additional;
            }
        }

        let mut out = String::with_capacity(current_len);
        out.push_str(p);
        for d in expanded_dirs {
            out.push_str(&d);
            out.push('/');
        }
        out.push_str(filename);
        return out;
    }

    // Stage 3: Try `.../filename`
    let ellipsis_filename = format!(".../{}", filename);
    if UnicodeWidthStr::width(ellipsis_filename.as_str()) <= budget {
        return ellipsis_filename;
    }

    // Stage 4: Try `filename` alone
    if UnicodeWidthStr::width(filename) <= budget {
        return filename.to_string();
    }

    // Stage 5: Middle-truncate filename while attempting to preserve file extension
    middle_truncate_filename(filename, budget)
}

/// Middle-truncate a filename while preserving its file extension when possible.
/// e.g. `very_long_component_name.rs` (budget 14) -> `very_...ame.rs`.
pub fn middle_truncate_filename(filename: &str, max_width: usize) -> String {
    let fn_w = UnicodeWidthStr::width(filename);
    if fn_w <= max_width {
        return filename.to_string();
    }
    if max_width <= 3 {
        return truncate_middle(filename, max_width);
    }

    // Split stem and ext
    if let Some(dot_idx) = filename.rfind('.')
        && dot_idx > 0
        && dot_idx < filename.len() - 1
    {
        let stem = &filename[..dot_idx];
        let ext = &filename[dot_idx..]; // includes '.'
        let ext_w = UnicodeWidthStr::width(ext);

        // If we have room for at least 2 stem chars + ellipsis '…' + extension
        if max_width > ext_w + 3 {
            let stem_budget = max_width - ext_w;
            let shortened_stem = truncate_middle(stem, stem_budget);
            return format!("{}{}", shortened_stem, ext);
        }
    }

    truncate_middle(filename, max_width)
}

/// Truncate a string with a single ellipsis character `…` in the middle.
pub fn truncate_middle(s: &str, max_width: usize) -> String {
    let width = UnicodeWidthStr::width(s);
    if width <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    if max_width == 2 {
        return "..".to_string();
    }
    if max_width == 3 {
        return "...".to_string();
    }

    let avail = max_width.saturating_sub(1); // 1 cell for '…'
    let left_budget = avail / 2;
    let right_budget = avail - left_budget;

    let chars: Vec<char> = s.chars().collect();
    let mut left_str = String::new();
    let mut left_w = 0;
    for &c in &chars {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1);
        if left_w + cw > left_budget {
            break;
        }
        left_str.push(c);
        left_w += cw;
    }

    let mut right_chars = Vec::new();
    let mut right_w = 0;
    for &c in chars.iter().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1);
        if right_w + cw > right_budget {
            break;
        }
        right_chars.push(c);
        right_w += cw;
    }
    right_chars.reverse();
    let right_str: String = right_chars.into_iter().collect();

    format!("{}…{}", left_str, right_str)
}

/// Render a formatted path text into semantic Ratatui `Span`s.
#[allow(dead_code)]
pub fn render_path_spans(text: &str, style: PathStyle, theme: &Theme) -> Vec<Span<'static>> {
    if text.is_empty() {
        return Vec::new();
    }

    match style {
        PathStyle::Plain(s) => vec![Span::styled(text.to_string(), s)],
        PathStyle::Muted => vec![Span::styled(
            text.to_string(),
            Style::default().fg(theme.muted()),
        )],
        PathStyle::Semantic => {
            let (dir_style, stem_style, ext_style, line_col_style) = (
                Style::default().fg(theme.muted()),
                Style::default().fg(theme.fg()),
                Style::default().fg(theme.muted()),
                Style::default().fg(theme.brand()),
            );
            render_semantic_spans(text, dir_style, stem_style, ext_style, line_col_style)
        }
        PathStyle::Custom {
            dir,
            stem,
            ext,
            line_col,
        } => render_semantic_spans(text, dir, stem, ext, line_col),
    }
}

fn render_semantic_spans(
    text: &str,
    dir_style: Style,
    stem_style: Style,
    ext_style: Style,
    line_col_style: Style,
) -> Vec<Span<'static>> {
    let (path_part, line_col_part) = split_line_col(text);
    let mut spans = Vec::with_capacity(4);

    let (dir_prefix, file_part) = if let Some(last_slash) = path_part.rfind('/') {
        (&path_part[..=last_slash], &path_part[last_slash + 1..])
    } else {
        ("", path_part)
    };

    if !dir_prefix.is_empty() {
        spans.push(Span::styled(dir_prefix.to_string(), dir_style));
    }

    if !file_part.is_empty() {
        if let Some(dot_idx) = file_part.rfind('.') {
            if dot_idx > 0 && dot_idx < file_part.len() - 1 {
                let stem = &file_part[..dot_idx];
                let ext = &file_part[dot_idx..];
                spans.push(Span::styled(stem.to_string(), stem_style));
                spans.push(Span::styled(ext.to_string(), ext_style));
            } else {
                spans.push(Span::styled(file_part.to_string(), stem_style));
            }
        } else {
            spans.push(Span::styled(file_part.to_string(), stem_style));
        }
    }

    if let Some(line_col) = line_col_part
        && !line_col.is_empty()
    {
        spans.push(Span::styled(line_col.to_string(), line_col_style));
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_line_col() {
        assert_eq!(
            split_line_col("src/main.rs:42:10"),
            ("src/main.rs", Some(":42:10"))
        );
        assert_eq!(
            split_line_col("src/main.rs:42"),
            ("src/main.rs", Some(":42"))
        );
        assert_eq!(split_line_col("src/main.rs"), ("src/main.rs", None));
        assert_eq!(split_line_col(""), ("", None));
        assert_eq!(split_line_col("foo:bar"), ("foo:bar", None));
    }

    #[test]
    fn test_fish_shorten_path() {
        assert_eq!(
            fish_shorten_path("apps/terminal/crates/nuo/src/components/path.rs"),
            "a/t/c/n/s/c/path.rs"
        );
        assert_eq!(
            fish_shorten_path("~/projects/nuo/src/lib.rs"),
            "~/p/n/s/lib.rs"
        );
        assert_eq!(fish_shorten_path("/usr/local/bin/nuo"), "/u/l/b/nuo");
        assert_eq!(
            fish_shorten_path(".config/nuo/config.toml"),
            ".c/n/config.toml"
        );
        assert_eq!(fish_shorten_path("main.rs"), "main.rs");
    }

    #[test]
    fn test_basename_with_parent() {
        assert_eq!(
            basename_with_parent("apps/terminal/crates/nuo/src/components/path.rs"),
            ".../components/path.rs"
        );
        assert_eq!(basename_with_parent("src/path.rs"), "src/path.rs");
        assert_eq!(basename_with_parent("path.rs"), "path.rs");
    }

    #[test]
    fn test_adaptive_shortening_hierarchy() {
        let long_path = "crates/nuo/src/components/path_view.rs";

        // Budget fits full path
        assert_eq!(adaptive_shorten(long_path, 50), long_path);

        // Budget fits the ellipsis form: preferred over fish abbreviations
        // (readability-first, rustc/IDE convention). `.../components/path_view.rs`
        // is 27 cells.
        assert_eq!(
            adaptive_shorten(long_path, 27),
            ".../components/path_view.rs"
        );

        // Tighter budget: ellipsis no longer fits -> fish fallback
        // (`c/nuo/src/c/path_view.rs`, 25 cells), expanded right-to-left.
        assert_eq!(adaptive_shorten(long_path, 25), "c/nuo/src/c/path_view.rs");

        // Tight budget -> basename
        let tight = adaptive_shorten(long_path, 15);
        assert!(UnicodeWidthStr::width(tight.as_str()) <= 15);
        assert_eq!(tight, "path_view.rs");

        // Super tight budget -> middle-truncated filename preserving extension
        let super_tight = adaptive_shorten(long_path, 10);
        assert!(UnicodeWidthStr::width(super_tight.as_str()) <= 10);
        assert!(super_tight.ends_with(".rs"));
    }

    #[test]
    fn test_adaptive_fish_fallback_when_ellipsis_does_not_fit() {
        // Ellipsis form exceeds budget but fish still fits -> fish is the fallback.
        let long_path = "a-very-long-dir-name/another-long-dir/final.rs";
        // `.../another-long-dir/final.rs` = 31 cells > 28.
        // fish form `a/another-long-dir/final.rs` = 27 <= 28.
        assert_eq!(
            adaptive_shorten(long_path, 28),
            "a/another-long-dir/final.rs"
        );
    }

    #[test]
    fn test_middle_truncate_filename() {
        let fn_name = "very_long_view_model_component.rs";
        let res = middle_truncate_filename(fn_name, 15);
        assert_eq!(UnicodeWidthStr::width(res.as_str()), 15);
        assert!(res.ends_with(".rs"));
        assert!(res.contains('…'));
    }

    #[test]
    fn test_path_with_line_col() {
        let formatted = format_path_str(
            "crates/nuo/src/main.rs:120:5",
            None,
            Some(20),
            PathFormatStrategy::Adaptive,
            true,
        );
        assert!(formatted.ends_with(":120:5"));
        assert!(UnicodeWidthStr::width(formatted.as_str()) <= 20);
    }

    #[test]
    fn test_path_view_relative_base_dir() {
        let base = Path::new("/workspace/project");
        let target = Path::new("/workspace/project/crates/nuo/src/lib.rs");

        let view = PathView::new(target).base_dir(base);
        assert_eq!(view.format_text(), "crates/nuo/src/lib.rs");
    }

    #[test]
    fn test_tilde_expand() {
        let home = Path::new("/home/ming");
        assert_eq!(
            tilde_expand_with(Path::new("~/projects/nuo"), Some(home)),
            PathBuf::from("/home/ming/projects/nuo")
        );
        assert_eq!(
            tilde_expand_with(Path::new("~"), Some(home)),
            home.to_path_buf()
        );
        // `~user` style and relative paths must pass through untouched.
        assert_eq!(
            tilde_expand_with(Path::new("~other/x"), Some(home)),
            PathBuf::from("~other/x")
        );
        assert_eq!(
            tilde_expand_with(Path::new("src/lib.rs"), Some(home)),
            PathBuf::from("src/lib.rs")
        );
    }

    #[test]
    fn test_path_view_tilde_under_base_dir() {
        // A `~/...` path that lives under the workspace root (which itself is
        // under `$HOME`) must be made workspace-relative, not left as `~/...`.
        if let Some(home) = dirs::home_dir().or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        {
            let base = home.join("projects/nuo");
            let view = PathView::from_str("~/projects/nuo/crates/nuo-llm-client/src/lib.rs")
                .base_dir(&base);
            assert_eq!(view.format_text(), "crates/nuo-llm-client/src/lib.rs");
        }
    }

    #[test]
    fn test_path_view_to_spans() {
        let theme = Theme::default();
        let view = PathView::from_str("src/components/path.rs:42:10");
        let spans = view.to_spans(&theme);

        assert!(!spans.is_empty());
        let full_text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(full_text, "src/components/path.rs:42:10");
    }

    #[test]
    fn test_cjk_unicode_shorten() {
        let cjk_path = "文档/项目架构/设计方案/核心组件.md";
        let shortened = adaptive_shorten(cjk_path, 20);
        assert!(UnicodeWidthStr::width(shortened.as_str()) <= 20);
        assert!(shortened.ends_with(".md"));
    }
}
