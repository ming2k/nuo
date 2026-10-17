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

/// Returns the physical root directory for VFS files (`/tmp/nuo/vfs` on Unix, `%TEMP%\nuo\vfs` on Windows).
pub fn vfs_root_dir() -> std::path::PathBuf {
    #[cfg(unix)]
    {
        std::path::PathBuf::from("/tmp/nuo/vfs")
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("nuo").join("vfs")
    }
}

/// Format the canonical VFS path for a given content hash and extension.
pub fn vfs_canonical_path(hash: &str, ext: &str) -> String {
    format!("{VFS_ROOT_PREFIX}{hash}.{ext}")
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

/// Checks if a path matches any recognized virtual file system prefix.
pub fn is_virtual_path(path: &str) -> bool {
    let normalized = path.trim().replace('\\', "/");
    normalized.starts_with("/tmp/nuo/vfs/")
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
        "/tmp/nuo/vfs/",
        "/tmp/vfs/",
        "/virtual/",
        ".nuo/virtual/",
        "virtual://",
        ".virtual/",
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
}
