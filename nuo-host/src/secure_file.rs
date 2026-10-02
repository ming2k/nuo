//! Private local-state files and crash-safe atomic replacement.

use std::fs::File;
use std::io;
use std::path::Path;

pub fn create_private_parent(path: &Path) -> io::Result<()> {
    native::create_private_parent(path)
}

pub fn create_private_file(path: &Path) -> io::Result<File> {
    native::create_private_file(path)
}

/// Exclusively create a new owner-only file.
///
/// Unlike [`create_private_file`], this never truncates an existing path. It
/// is the primitive atomic writers use to claim a unique temporary pathname
/// without a check-then-create race.
pub fn create_new_private_file(path: &Path) -> io::Result<File> {
    native::create_new_private_file(path)
}

/// Atomically replace `destination` with the same-filesystem temporary file
/// at `source`, including Windows' explicit replace-existing semantics.
pub fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    native::atomic_replace(source, destination)
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    pub(super) fn create_private_parent(path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    pub(super) fn create_private_file(path: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
    }

    pub(super) fn create_new_private_file(path: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }

    pub(super) fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
        std::fs::rename(source, destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn private_file_can_be_durably_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("private").join("state.json");
        create_private_parent(&destination).unwrap();
        let temporary = destination.with_extension("tmp");
        let mut file = create_private_file(&temporary).unwrap();
        file.write_all(b"new state").unwrap();
        file.sync_all().unwrap();
        drop(file);
        atomic_replace(&temporary, &destination).unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"new state");
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use crate::windows_security::SecurityDescriptor;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::ptr;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
        SetFileSecurityW,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_ALWAYS, CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    pub(super) fn create_private_parent(path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            let parent = wide(parent);
            let descriptor = SecurityDescriptor::current_user_only()?;
            // Tighten both newly-created and pre-existing leaf directories.
            // This makes custom MUTA_HOME roots obey the same privacy
            // contract as native profile directories.
            if unsafe {
                SetFileSecurityW(
                    parent.as_ptr(),
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    descriptor.as_ptr(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub(super) fn create_private_file(path: &Path) -> io::Result<File> {
        open_private_file(path, CREATE_ALWAYS)
    }

    pub(super) fn create_new_private_file(path: &Path) -> io::Result<File> {
        open_private_file(path, CREATE_NEW)
    }

    fn open_private_file(path: &Path, creation_disposition: u32) -> io::Result<File> {
        let wide = wide(path);
        let descriptor = SecurityDescriptor::current_user_only()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.as_ptr(),
            bInheritHandle: 0,
        };
        // SAFETY: all pointers remain valid for CreateFileW; the returned
        // handle is transferred exactly once into `File`.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ,
                &attributes,
                creation_disposition,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_handle(handle) })
        }
    }

    pub(super) fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
        let source = wide(source);
        let destination = wide(destination);
        // SAFETY: both paths are NUL-terminated for the duration of the call.
        if unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
}
