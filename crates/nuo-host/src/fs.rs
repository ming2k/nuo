//! Cross-platform filesystem operations, symlinks, and path shims.

use std::io;
use std::path::{Path, PathBuf};

/// Creates a symbolic link to a file in a platform-appropriate way.
pub fn symlink_file(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(original, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(original, link)
    }
}

/// Creates a symbolic link to a directory in a platform-appropriate way.
pub fn symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(original, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(original, link)
    }
}

/// Checks whether a given path is a symbolic link without following it.
#[must_use]
pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

/// Reads the target destination of a symbolic link.
pub fn read_link(path: &Path) -> io::Result<PathBuf> {
    std::fs::read_link(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlink_file_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let original = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&original, b"content").expect("write target");

        if let Ok(()) = symlink_file(&original, &link) {
            assert!(is_symlink(&link));
            assert_eq!(std::fs::read(&link).unwrap(), b"content");
        }
    }
}
