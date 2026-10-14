//! Runtime environment and machine identity detection matching agy's mechanisms.
//!
//! Provides reverse-engineered runtime environment classification (`SSH session`,
//! `WSL environment`, `Container environment`, or `Standalone`) and stable device
//! fingerprint generation.

use sha2::{Digest, Sha256};
use std::path::Path;

/// Detect the active runtime environment using agy's detection heuristics.
pub fn detect_runtime_environment() -> &'static str {
    // 1. SSH session detection (SSH_CLIENT, SSH_TTY, SSH_CONNECTION)
    if std::env::var_os("SSH_CLIENT").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("SSH_CONNECTION").is_some()
    {
        return "SSH session";
    }

    // 2. WSL environment detection
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return "WSL environment";
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(version) = std::fs::read_to_string("/proc/version") {
            let lower = version.to_ascii_lowercase();
            if lower.contains("microsoft") || lower.contains("wsl") {
                return "WSL environment";
            }
        }
    }

    // 3. Container environment detection
    if Path::new("/.dockerenv").exists() || Path::new("/run/systemd/container").exists() {
        return "Container environment";
    }

    if std::env::var_os("container").is_some_and(|c| !c.is_empty()) {
        return "Container environment";
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            let lower = cgroup.to_ascii_lowercase();
            if lower.contains("docker")
                || lower.contains("containerd")
                || lower.contains("kubepods")
                || lower.contains("lxc")
            {
                return "Container environment";
            }
        }
    }

    "Standalone"
}

/// Generate a stable 64-hex-character device fingerprint matching agy's device identification.
pub fn detect_device_fingerprint() -> String {
    let mut hasher = Sha256::new();

    // 1. Try reading system-level machine ID
    #[cfg(target_os = "linux")]
    {
        if let Ok(id) = std::fs::read_to_string("/etc/machine-id") {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                hasher.update(b"linux-machine-id:");
                hasher.update(trimmed.as_bytes());
                return format!("{:x}", hasher.finalize());
            }
        }
        if let Ok(id) = std::fs::read_to_string("/var/lib/dbus/machine-id") {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                hasher.update(b"dbus-machine-id:");
                hasher.update(trimmed.as_bytes());
                return format!("{:x}", hasher.finalize());
            }
        }
    }

    // 2. Fallback to stable machine traits (hostname, user, home path)
    hasher.update(b"fallback-device:");
    if let Ok(hostname) = std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")) {
        hasher.update(hostname.as_bytes());
    }
    hasher.update(b":");
    if let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("USERNAME")) {
        hasher.update(user.as_bytes());
    }
    hasher.update(b":");
    if let Some(home) = dirs::home_dir() {
        hasher.update(home.to_string_lossy().as_bytes());
    }

    format!("{:x}", hasher.finalize())
}

/// Generate a fresh session UUIDv4 string.
pub fn generate_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_environment_detection() {
        let env = detect_runtime_environment();
        assert!(!env.is_empty());
        assert!(matches!(
            env,
            "Standalone" | "SSH session" | "WSL environment" | "Container environment"
        ));
    }

    #[test]
    fn test_device_fingerprint_is_stable_and_valid_hex() {
        let fp1 = detect_device_fingerprint();
        let fp2 = detect_device_fingerprint();
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 64);
        assert!(fp1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_session_id() {
        let sid = generate_session_id();
        assert_eq!(sid.len(), 36);
        assert_eq!(sid.chars().filter(|c| *c == '-').count(), 4);
    }
}
