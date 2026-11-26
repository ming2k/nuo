//! Owner-only filesystem permissions for durable local storage (ADR-0255).
//!
//! Session journals, content shards, CAS blobs, and rolling backups hold
//! complete user prompts and tool outputs. On multi-user Unix hosts they are
//! created with mode `0o700` (directories) and `0o600` (files) so other local
//! accounts cannot read conversation data.

use std::io;
use std::path::Path;

/// Create `dir` (and any missing parents) with owner-only permissions (`0o700`
/// on Unix). Existing directories that already exist are left untouched only
/// for ancestor paths; the target directory itself is restricted to `0o700`.
pub(crate) fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::DirBuilder;
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        let mut builder = DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Restrict an already-created durable file to owner-only read/write (`0o600`
/// on Unix). Best-effort so filesystems that ignore POSIX modes (e.g. FAT/SMB)
/// never break commits.
pub(crate) fn restrict_private_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ensure_private_dir_and_file_apply_owner_only_modes() {
        let root = tempfile::tempdir().unwrap();
        let target_dir = root.path().join("private").join("nested");
        ensure_private_dir(&target_dir).unwrap();
        let dir_mode = std::fs::metadata(&target_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);

        let target_file = target_dir.join("secret.log");
        std::fs::write(&target_file, b"sensitive").unwrap();
        restrict_private_file(&target_file);
        let file_mode = std::fs::metadata(&target_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
    }
}
