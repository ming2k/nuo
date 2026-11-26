//! Optimistic-concurrency file version fingerprints shared across filesystem tools.
//!
//! Small files use a content-addressed SHA-256 prefix so same-length rewrites
//! are always caught; large files fall back to length + mtime + Unix inode/ctime
//! metadata so a read never hashes gigabytes.

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Maximum file size for which a read computes a full content hash.
const MAX_CONTENT_VERSION_BYTES: u64 = 1024 * 1024;

/// Content-addressed version token computed directly from in-memory bytes.
///
/// Reused by small-file reads so the file is read from disk exactly once
/// (`[INV-READ-IO-01]`, ADR-0257).
pub(crate) fn file_version_from_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 0x0f) as usize] as char);
    }
    format!("c{hex}-{:x}", bytes.len())
}

/// Content-addressed version for small files, metadata identity for large ones.
pub(crate) fn file_version(path: &Path, meta: &fs::Metadata) -> String {
    if meta.is_file()
        && meta.len() <= MAX_CONTENT_VERSION_BYTES
        && let Ok(bytes) = fs::read(path)
    {
        return file_version_from_bytes(&bytes);
    }
    metadata_version(meta)
}

fn metadata_version(meta: &fs::Metadata) -> String {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        format!(
            "-{:x}-{:x}-{:x}-{:x}",
            meta.dev(),
            meta.ino(),
            meta.ctime(),
            meta.ctime_nsec()
        )
    };
    #[cfg(not(unix))]
    let identity = String::new();
    format!("m{:x}-{mtime:x}{identity}", meta.len())
}
