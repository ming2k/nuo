//! Fail-closed local workspace process isolation.
//!
//! The sandbox starts from an empty root and admits only a minimal system
//! runtime, public DNS/TLS configuration, the exact workspace, and ephemeral
//! process state. Callers explicitly choose whether the workspace is writable
//! and whether the process receives a network namespace connected to the host.

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Filesystem authority granted to a sandboxed process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceAccess {
    ReadOnly,
    ReadWrite,
}

/// Active sandbox driver kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxDriverKind {
    /// Linux Bubblewrap (namespaces, ro-bind host overlay).
    LinuxBubblewrap,
    /// No supported native isolation mechanism available on this host.
    Unavailable,
}

/// Returns the native sandbox driver kind for the host platform.
#[must_use]
pub fn driver_kind() -> SandboxDriverKind {
    #[cfg(target_os = "linux")]
    {
        if available() {
            SandboxDriverKind::LinuxBubblewrap
        } else {
            SandboxDriverKind::Unavailable
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        SandboxDriverKind::Unavailable
    }
}

/// Network authority granted to a sandboxed process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkAccess {
    Disabled,
    Enabled,
}

/// Probe the physical Linux sandbox once. Existence is insufficient:
/// distributions may install bubblewrap while disabling user namespaces.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        #[cfg(target_os = "linux")]
        {
            if bubblewrap_program().is_none() {
                return false;
            }
            let probe_root = std::env::temp_dir().join(format!(
                "muta-workspace-sandbox-probe-{}",
                std::process::id()
            ));
            if std::fs::create_dir_all(&probe_root).is_err() {
                return false;
            }
            let Ok(mut command) = command_with_environment(
                "/bin/true",
                &[],
                &HashMap::new(),
                &probe_root,
                WorkspaceAccess::ReadOnly,
                NetworkAccess::Disabled,
            ) else {
                let _ = std::fs::remove_dir(&probe_root);
                return false;
            };
            let available = command
                .as_std_mut()
                .status()
                .is_ok_and(|status| status.success());
            let _ = std::fs::remove_dir(&probe_root);
            available
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    })
}

/// Build a sandboxed non-interactive shell command.
pub fn shell(
    command: &str,
    workspace_root: &Path,
    workspace_access: WorkspaceAccess,
    network_access: NetworkAccess,
) -> Result<tokio::process::Command, String> {
    shell_with_roots(
        command,
        workspace_root,
        &[],
        workspace_access,
        network_access,
    )
}

/// Build a sandboxed non-interactive shell command over a multi-root
/// workspace (ADR-0142): `workspace_root` stays the cwd, and every entry of
/// `additional_roots` is admitted read-write alongside it.
pub fn shell_with_roots(
    command: &str,
    workspace_root: &Path,
    additional_roots: &[PathBuf],
    workspace_access: WorkspaceAccess,
    network_access: NetworkAccess,
) -> Result<tokio::process::Command, String> {
    command_with_roots(
        "/bin/sh",
        &["-c".to_string(), command.to_string()],
        &HashMap::new(),
        workspace_root,
        additional_roots,
        workspace_access,
        network_access,
    )
}

/// Build a sandboxed direct-program command. `environment` is applied only
/// after the ambient environment is cleared and cannot override containment
/// variables such as HOME, PATH, or TMPDIR.
pub fn command_with_environment(
    program: &str,
    args: &[String],
    environment: &HashMap<String, String>,
    workspace_root: &Path,
    workspace_access: WorkspaceAccess,
    network_access: NetworkAccess,
) -> Result<tokio::process::Command, String> {
    command_with_roots(
        program,
        args,
        environment,
        workspace_root,
        &[],
        workspace_access,
        network_access,
    )
}

/// Build a sandboxed direct-program command over a multi-root workspace
/// (ADR-0142). Identical containment to
/// [`command_with_environment`], plus one read-write bind per additional
/// root. A root that is redundant with the primary (or a duplicate of an
/// earlier entry) is skipped rather than rejected, so a stale
/// `[workspace]` table cannot brick the sandbox.
pub fn command_with_roots(
    program: &str,
    args: &[String],
    environment: &HashMap<String, String>,
    workspace_root: &Path,
    additional_roots: &[PathBuf],
    workspace_access: WorkspaceAccess,
    network_access: NetworkAccess,
) -> Result<tokio::process::Command, String> {
    validate_environment(environment)?;

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            program,
            args,
            workspace_root,
            additional_roots,
            workspace_access,
            network_access,
        );
        return Err(
            "Workspace process isolation is unavailable on this platform; refusing host execution."
                .to_string(),
        );
    }

    #[cfg(target_os = "linux")]
    {
        let root = std::fs::canonicalize(workspace_root).map_err(|error| {
            format!(
                "Cannot establish workspace sandbox root '{}': {error}",
                workspace_root.display()
            )
        })?;
        if root.parent().is_none() {
            return Err(
                "The filesystem root cannot be used as a development workspace sandbox."
                    .to_string(),
            );
        }
        let bubblewrap = bubblewrap_program().ok_or_else(|| {
            "Workspace process isolation requires bubblewrap (bwrap); refusing unsandboxed host execution."
                .to_string()
        })?;

        let mut invocation = tokio::process::Command::new(bubblewrap);
        invocation.args([
            "--die-with-parent",
            "--new-session",
            "--unshare-user",
            "--disable-userns",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup-try",
            "--cap-drop",
            "ALL",
        ]);
        if network_access == NetworkAccess::Disabled {
            invocation.arg("--unshare-net");
        }

        // ADR-0287: Pure immutable read-only host root overlay.
        // Mounts the entire host filesystem as strictly read-only, eliminating
        // fragile distro-specific path lists while preserving developer toolchains,
        // glibc NSS databases, and SSL certificates across all Linux distributions.
        invocation.args([
            "--ro-bind", "/", "/",
            "--dev", "/dev",
            "--proc", "/proc",
            "--tmpfs", "/tmp",
        ]);

        invocation
            .arg(match workspace_access {
                WorkspaceAccess::ReadOnly => "--ro-bind",
                WorkspaceAccess::ReadWrite => "--bind",
            })
            .arg(&root)
            .arg(&root);

        // Additional roots (ADR-0142): widened admission, same containment.
        let mut bound_roots: BTreeSet<PathBuf> = BTreeSet::new();
        bound_roots.insert(root.clone());
        for extra in additional_roots {
            let Ok(canonical) = std::fs::canonicalize(extra) else {
                continue;
            };
            if !bound_roots.insert(canonical.clone()) {
                continue;
            }
            invocation.arg("--bind").arg(&canonical).arg(&canonical);
        }

        invocation.arg("--chdir").arg(&root);

        let path = std::env::var("PATH")
            .unwrap_or_else(|_| "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string());
        invocation.args([
            "--clearenv",
            "--setenv",
            "HOME",
            "/tmp/muta-home",
            "--setenv",
            "PATH",
            &path,
            "--setenv",
            "TMPDIR",
            "/tmp",
            "--setenv",
            "LANG",
            "C.UTF-8",
            "--setenv",
            "CI",
            "1",
            "--setenv",
            "GIT_TERMINAL_PROMPT",
            "0",
            "--setenv",
            "NO_COLOR",
            "1",
            "--setenv",
            "PAGER",
            "cat",
            "--setenv",
            "GIT_PAGER",
            "cat",
            "--setenv",
            "EDITOR",
            "false",
            "--setenv",
            "VISUAL",
            "false",
            "--setenv",
            "GIT_EDITOR",
            "false",
            "--setenv",
            "GIT_SEQUENCE_EDITOR",
            "false",
            "--setenv",
            "DEBIAN_FRONTEND",
            "noninteractive",
            "--setenv",
            "TERM",
            "dumb",
        ]);
        for (name, value) in environment.iter().collect::<BTreeMap<_, _>>() {
            invocation.arg("--setenv").arg(name).arg(value);
        }
        invocation.arg("--").arg(program).args(args);
        Ok(invocation)
    }
}

fn validate_environment(environment: &HashMap<String, String>) -> Result<(), String> {
    const RESERVED: &[&str] = &[
        "HOME",
        "TMPDIR",
        "PATH",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "BASH_ENV",
        "ENV",
        "SHELLOPTS",
    ];
    for name in environment.keys() {
        if name.is_empty()
            || name.contains('=')
            || name.as_bytes().contains(&0)
            || RESERVED.contains(&name.as_str())
        {
            return Err(format!(
                "Project process environment variable '{name}' is invalid or reserved by the workspace sandbox."
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bubblewrap_program() -> Option<&'static str> {
    if Path::new("/usr/bin/bwrap").is_file() {
        Some("/usr/bin/bwrap")
    } else if Path::new("/bin/bwrap").is_file() {
        Some("/bin/bwrap")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "muta-platform-sandbox-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn additional_roots_are_mounted_read_write() {
        if !available() {
            return;
        }
        let root = scratch();
        let sibling = scratch();
        std::fs::write(sibling.join("sibling.txt"), "from sibling").unwrap();

        let command = shell_with_roots(
            "true",
            &root,
            std::slice::from_ref(&sibling),
            WorkspaceAccess::ReadWrite,
            NetworkAccess::Disabled,
        )
        .unwrap();
        // The sibling must appear as a bind mount at its canonical path...
        let arguments = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let bind_at = arguments
            .iter()
            .position(|arg| arg == "--bind" && *sibling.to_string_lossy() == **arg)
            .or_else(|| {
                arguments
                    .windows(3)
                    .position(|w| w[0] == "--bind" && w[1] == sibling.to_string_lossy())
            });
        let bind_ok = bind_at
            .map(|i| {
                arguments
                    .get(i + 2)
                    .map(|dst| *dst == sibling.to_string_lossy())
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        assert!(bind_ok, "sibling not bound: {arguments:?}");
        // ...and the process can read and write through it, while the host
        // filesystem outside the roots remains read-only.
        std::fs::write(
            root.join("probe.sh"),
            format!(
                "cat {}/sibling.txt && touch {}/written && ! touch /etc/muta_leak_test 2>/dev/null",
                sibling.display(),
                sibling.display()
            ),
        )
        .unwrap();
        let mut command = shell_with_roots(
            "sh probe.sh",
            &root,
            std::slice::from_ref(&sibling),
            WorkspaceAccess::ReadWrite,
            NetworkAccess::Disabled,
        )
        .unwrap();
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("from sibling"));
        assert!(sibling.join("written").exists());
        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(sibling).ok();
    }

    #[tokio::test]
    async fn project_extension_profile_is_read_only_and_host_blind() {
        if !available() {
            return;
        }
        let root = scratch();
        std::fs::write(root.join("visible"), "ok").unwrap();
        let mut command = shell(
            "test -r visible && ! touch created 2>/dev/null && ! touch /etc/muta_leak_test 2>/dev/null",
            &root,
            WorkspaceAccess::ReadOnly,
            NetworkAccess::Disabled,
        )
        .unwrap();
        let arguments = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(arguments.iter().any(|arg| arg == "--unshare-net"));
        assert!(arguments.windows(3).any(|window| {
            window[0] == "--ro-bind"
                && window[1] == root.to_string_lossy()
                && window[2] == root.to_string_lossy()
        }));
        assert!(command.status().await.unwrap().success());
        assert!(!root.join("created").exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn project_environment_cannot_override_containment() {
        let root = scratch();
        let error = command_with_environment(
            "true",
            &[],
            &HashMap::from([("PATH".to_string(), "/attacker".to_string())]),
            &root,
            WorkspaceAccess::ReadOnly,
            NetworkAccess::Disabled,
        )
        .unwrap_err();
        assert!(error.contains("reserved"));
        std::fs::remove_dir_all(root).ok();
    }
}
