//! Virtual File System (VFS) abstraction for in-process projection (ADR-0070).
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
pub fn is_virtual_path(path: &str) -> bool {
    let normalized = path.trim().replace('\\', "/");
    let spool_dir = spool_root_dir().display().to_string().replace('\\', "/");
    normalized.starts_with(&spool_dir)
        || normalized.starts_with("/tmp/nuo/spool/")
        || normalized.starts_with("/dev/shm/nuo/spool/")
        || normalized.starts_with("/tmp/nuo/vfs/")
        || normalized.starts_with("/tmp/vfs/")
        || normalized.starts_with("/virtual/")
        || normalized.starts_with(".nuo/virtual/")
        || normalized.starts_with("virtual://")
        || normalized.starts_with(".virtual/")
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
