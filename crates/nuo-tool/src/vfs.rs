//! Virtual File System (VFS) abstraction for in-process projection (ADR-0070 → ADR-0093).
//!
//! Provides transparent in-memory resolution of virtual paths (e.g. `/tmp/nuo/vfs/...`,
//! `/virtual/...`, `virtual://...`) directly against Content-Addressed Storage (CAS)
//! or observation stores, guaranteeing zero root privilege dependency, zero disk pollution,
//! zero workspace mutation, and role-agnostic sovereign addressing.

use async_trait::async_trait;
use sha2::{Digest, Sha256};

/// Canonical VFS mount path prefix (Rootless, universal across all roles).
pub const VFS_ROOT_PREFIX: &str = "/tmp/nuo/vfs/";

/// Returns the ephemeral execution spool directory (ADR-0093 [INV-SPOOL-01]).
///
/// On Linux: uses `/dev/shm/nuo/spool` if `/dev/shm` is available for zero-disk-write RAM backing,
/// or falls back to `/tmp/nuo/spool`.
/// On non-Unix: uses `%TEMP%\nuo\spool`.
pub fn spool_root_dir() -> std::path::PathBuf {
    #[cfg(unix)]
    {
        if std::path::Path::new("/dev/shm").is_dir() {
            std::path::PathBuf::from("/dev/shm/nuo/spool")
        } else {
            std::path::PathBuf::from("/tmp/nuo/spool")
        }
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("nuo").join("spool")
    }
}

/// Returns the physical root directory for spool / VFS files.
pub fn vfs_root_dir() -> std::path::PathBuf {
    spool_root_dir()
}

/// Format the canonical spool path for a given content hash and extension (ADR-0093).
pub fn spool_canonical_path(hash: &str, ext: &str) -> String {
    let dir = spool_root_dir();
    let normalized = dir.display().to_string().replace('\\', "/");
    format!("{normalized}/{hash}.{ext}")
}

/// Format the canonical VFS / spool path for a given content hash and extension.
pub fn vfs_canonical_path(hash: &str, ext: &str) -> String {
    spool_canonical_path(hash, ext)
}

/// Compute a stable 12-char SHA-256 hex digest for CAS content addressing.
pub fn vfs_short_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let full = hex::encode(hasher.finalize());
    full[..12].to_string()
}

/// Resolver for virtual file paths.
#[async_trait]
pub trait VirtualFileResolver: Send + Sync {
    /// Attempt to resolve a virtual path into text content from in-memory / CAS backing stores.
    async fn resolve_virtual(&self, path: &str) -> Option<String>;
}

/// Checks if a path matches any recognized virtual file system or execution spool prefix.
///
/// Paths containing parent-traversal segments (`..`) or NUL bytes are always rejected so
/// a model-supplied path can never abuse a virtual prefix to bypass workspace confinement
/// (`[INV-READ-VFS-01]`, ADR-0257).
pub fn is_virtual_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return false;
    }
    let normalized = trimmed.replace('\\', "/");
    if normalized.split('/').any(|segment| segment == "..") {
        return false;
    }
    let spool_prefix = trailing_slash(&spool_root_dir().display().to_string().replace('\\', "/"));
    normalized.starts_with(&spool_prefix)
        || normalized.starts_with("/tmp/nuo/spool/")
        || normalized.starts_with("/dev/shm/nuo/spool/")
        || normalized.starts_with("/tmp/nuo/vfs/")
        || normalized.starts_with("/tmp/vfs/")
        || normalized.starts_with("/virtual/")
        || normalized.starts_with(".nuo/virtual/")
        || normalized.starts_with("virtual://")
        || normalized.starts_with(".virtual/")
}

/// Resolves a physical spool file when the in-memory [`VirtualFileResolver`] misses.
///
/// Only paths that remain canonically inside an owned spool root are admitted: logical
/// prefixes such as `.nuo/virtual/...` or `virtual://...` never fall back to the process
/// working directory, and symlinks escaping the spool root are rejected
/// (`[INV-READ-VFS-01]`, ADR-0257).
pub fn resolve_physical_spool_path(path: &str) -> Option<std::path::PathBuf> {
    if !is_virtual_path(path) {
        return None;
    }
    let candidate = std::path::Path::new(path.trim());
    if !candidate.is_absolute() {
        return None;
    }
    let canonical_target = std::fs::canonicalize(candidate).ok()?;
    if !canonical_target.is_file() {
        return None;
    }
    let mut allowed_roots = vec![spool_root_dir()];
    #[cfg(unix)]
    {
        for extra in ["/dev/shm/nuo/spool", "/tmp/nuo/spool"] {
            let root = std::path::PathBuf::from(extra);
            if !allowed_roots.contains(&root) {
                allowed_roots.push(root);
            }
        }
    }
    for root in allowed_roots {
        if let Ok(canonical_root) = std::fs::canonicalize(&root)
            && canonical_target.starts_with(&canonical_root)
        {
            return Some(canonical_target);
        }
    }
    None
}

fn trailing_slash(prefix: &str) -> String {
    if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

/// Strips virtual prefix and optional extension to yield the canonical hash / handle.
pub fn extract_virtual_handle(path: &str) -> &str {
    let mut s = path.trim();
    for prefix in [
        "/dev/shm/nuo/spool/",
        "/tmp/nuo/spool/",
        "/tmp/nuo/vfs/",
        "/tmp/vfs/",
        "/virtual/",
        ".nuo/virtual/",
        "virtual://",
        ".virtual/",
        "\\dev\\shm\\nuo\\spool\\",
        "\\tmp\\nuo\\spool\\",
        "\\tmp\\nuo\\vfs\\",
        "\\tmp\\vfs\\",
        "\\virtual\\",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
            break;
        }
    }
    s = s.strip_suffix(".txt").unwrap_or(s);
    s = s.strip_suffix(".md").unwrap_or(s);
    s = s.strip_suffix(".log").unwrap_or(s);
    s = s.strip_suffix(".png").unwrap_or(s);
    s = s.strip_suffix(".jpg").unwrap_or(s);
    s
}

/// Scans omitted content for high-entropy error, panic, or failure signatures (ADR-0093 [INV-FOLD-01]).
/// Returns up to `max_signatures` of `(1-based relative line index, signature line)`.
pub fn extract_salient_signatures(omitted: &str, max_signatures: usize) -> Vec<(usize, String)> {
    let mut results = Vec::new();
    for (idx, line) in omitted.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        let is_salient = lower.contains("error:")
            || lower.contains("error[")
            || lower.starts_with("error")
            || lower.contains("panic:")
            || (lower.starts_with("thread '") && lower.contains("panicked at"))
            || lower.contains("failed")
            || lower.contains("failure")
            || lower.contains("fatal:")
            || lower.contains("syntaxerror")
            || lower.contains("typeerror")
            || lower.contains("assertionerror");

        if is_salient {
            let safe_line = if trimmed.chars().count() > 140 {
                let s: String = trimmed.chars().take(137).collect();
                format!("{s}...")
            } else {
                trimmed.to_string()
            };
            results.push((idx + 1, safe_line));
            if results.len() >= max_signatures {
                break;
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_virtual_path() {
        assert!(is_virtual_path("/tmp/nuo/vfs/e3b0c44298fc.log"));
        assert!(is_virtual_path("/virtual/e3b0c44298fc.txt"));
        assert!(is_virtual_path(".nuo/virtual/observations/call_123.txt"));
        assert!(is_virtual_path("/virtual/sub/ses_456.md"));
        assert!(is_virtual_path("virtual://obs:call_789"));
        assert!(!is_virtual_path("src/main.rs"));
        assert!(!is_virtual_path("/usr/bin/cargo"));
        // Traversal segments and sibling prefix collisions must never be treated as virtual.
        assert!(!is_virtual_path("/tmp/nuo/spool/../../etc/passwd"));
        assert!(!is_virtual_path("/dev/shm/nuo/spool/../../../etc/passwd"));
        assert!(!is_virtual_path(".nuo/virtual/../../secret.env"));
        assert!(!is_virtual_path(".virtual/../Cargo.toml"));
        assert!(!is_virtual_path("/tmp/nuo/spool_evil/file.txt"));
        assert!(!is_virtual_path("/tmp/nuo/spool/foo\0bar"));
    }

    #[test]
    fn test_resolve_physical_spool_path_confines_to_spool_root() {
        assert!(resolve_physical_spool_path(".nuo/virtual/does_not_escape.txt").is_none());
        assert!(resolve_physical_spool_path("virtual://obs:call_1").is_none());
        assert!(resolve_physical_spool_path("/tmp/nuo/spool/../../etc/passwd").is_none());
        assert!(resolve_physical_spool_path("/etc/passwd").is_none());

        let spool_dir = spool_root_dir();
        let _ = std::fs::create_dir_all(&spool_dir);
        let unique = format!(
            "nuo_vfs_test_{}_{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let spool_file = spool_dir.join(&unique);
        std::fs::write(&spool_file, "spool body").unwrap();
        let resolved = resolve_physical_spool_path(&spool_file.to_string_lossy());
        assert!(resolved.is_some());
        let _ = std::fs::remove_file(&spool_file);

        #[cfg(unix)]
        {
            let link_name = format!("link_{unique}");
            let link_path = spool_dir.join(&link_name);
            let _ = std::fs::remove_file(&link_path);
            if std::os::unix::fs::symlink("/etc/hosts", &link_path).is_ok() {
                assert!(
                    resolve_physical_spool_path(&link_path.to_string_lossy()).is_none(),
                    "symlink escaping spool root must be rejected"
                );
                let _ = std::fs::remove_file(&link_path);
            }
        }
    }

    #[test]
    fn test_extract_virtual_handle() {
        assert_eq!(
            extract_virtual_handle("/tmp/nuo/vfs/e3b0c44298fc.log"),
            "e3b0c44298fc"
        );
        assert_eq!(
            extract_virtual_handle("/virtual/sub:ses_456.md"),
            "sub:ses_456"
        );
        assert_eq!(
            extract_virtual_handle(".nuo/virtual/observations/call_123.txt"),
            "observations/call_123"
        );
    }

    #[test]
    fn test_vfs_short_hash() {
        let hash = vfs_short_hash(b"hello world");
        assert_eq!(hash.len(), 12);
        assert_eq!(hash, "b94d27b9934d");
    }

    #[test]
    fn test_spool_paths_and_salient_signatures() {
        assert!(is_virtual_path("/tmp/nuo/spool/abc123.log"));
        assert_eq!(
            extract_virtual_handle("/tmp/nuo/spool/abc123.log"),
            "abc123"
        );

        let log = "\
compiling crate foo v1.0.0
compiling crate bar v1.0.0
error[E0308]: mismatched types
  expected `bool`, found `i32`
thread 'main' panicked at 'explicit panic', src/lib.rs:10:5
done
";
        let signatures = extract_salient_signatures(log, 5);
        assert_eq!(signatures.len(), 2);
        assert_eq!(signatures[0].0, 3);
        assert!(signatures[0].1.contains("error[E0308]"));
        assert_eq!(signatures[1].0, 5);
        assert!(signatures[1].1.contains("panicked at"));
    }
}
