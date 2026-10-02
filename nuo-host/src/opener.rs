//! Cross-platform URL and file opener with headless/SSH shims and fallbacks.
//!
//! Provides a single, clean platform API for opening URLs (e.g. OAuth flows, docs,
//! browser previews) and local file paths across Linux, macOS, Windows, WSL, and
//! headless SSH/cloud server environments.

use std::path::Path;
use std::process::{Command, Stdio};

/// The outcome of an open request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Successfully launched in a desktop browser or external application.
    Launched { launcher: String },
    /// No graphical environment detected (SSH / headless); formatted for terminal display.
    Headless {
        url_or_path: String,
        osc8_link: Option<String>,
    },
}

/// Error encountered while attempting to open a URL or file.
#[derive(Debug)]
pub enum OpenError {
    SpawnFailed {
        launcher: String,
        source: std::io::Error,
    },
    NoOpenerFound,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpawnFailed { launcher, source } => {
                write!(f, "failed to spawn opener process ({launcher}): {source}")
            }
            Self::NoOpenerFound => write!(f, "no suitable opener found for target"),
        }
    }
}

impl std::error::Error for OpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SpawnFailed { source, .. } => Some(source),
            Self::NoOpenerFound => None,
        }
    }
}

/// System opener interface for opening URLs and files.
pub struct SystemOpener;

impl SystemOpener {
    /// Open a URL in the user's default browser or appropriate viewer.
    pub fn open_url(url: &str) -> Result<OpenOutcome, OpenError> {
        if is_headless() {
            let link = format_osc8_link(url, url);
            return Ok(OpenOutcome::Headless {
                url_or_path: url.to_string(),
                osc8_link: Some(link),
            });
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(outcome) = open_windows(url) {
                return Ok(outcome);
            }
        }

        #[cfg(target_os = "macos")]
        {
            if let Ok(outcome) = open_macos(url) {
                return Ok(outcome);
            }
        }

        #[cfg(target_os = "linux")]
        {
            if is_wsl()
                && let Ok(outcome) = open_wsl(url)
            {
                return Ok(outcome);
            }
            if let Ok(outcome) = open_linux(url) {
                return Ok(outcome);
            }
        }

        // Generic fallback
        if let Ok(outcome) = open_generic_browser(url) {
            return Ok(outcome);
        }

        // If all GUI openers fail, return headless outcome as fallback shim
        Ok(OpenOutcome::Headless {
            url_or_path: url.to_string(),
            osc8_link: Some(format_osc8_link(url, url)),
        })
    }

    /// Open a local file or directory with the system default handler.
    pub fn open_path(path: &Path) -> Result<OpenOutcome, OpenError> {
        let path_str = path.to_string_lossy();
        if is_headless() {
            return Ok(OpenOutcome::Headless {
                url_or_path: path_str.into_owned(),
                osc8_link: None,
            });
        }

        #[cfg(target_os = "windows")]
        {
            return open_windows(&path_str);
        }

        #[cfg(target_os = "macos")]
        {
            return open_macos(&path_str);
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            if is_wsl()
                && let Ok(outcome) = open_wsl(&path_str)
            {
                return Ok(outcome);
            }
            open_linux(&path_str)
        }
    }
}

/// Detect if running in a headless (SSH, no display, container) environment.
pub fn is_headless() -> bool {
    // Explicit headless override
    if std::env::var_os("MUTA_HEADLESS").is_some() {
        return true;
    }

    #[cfg(target_os = "windows")]
    {
        false
    }

    #[cfg(target_os = "macos")]
    {
        // Check SSH connection without active GUI
        std::env::var_os("SSH_CONNECTION").is_some() && std::env::var_os("TERM_PROGRAM").is_none()
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if is_wsl() {
            return false;
        }
        let has_display =
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
        let is_ssh = std::env::var_os("SSH_CONNECTION").is_some()
            || std::env::var_os("SSH_CLIENT").is_some()
            || std::env::var_os("SSH_TTY").is_some();
        !has_display || is_ssh
    }
}

/// Check whether the current system is running inside WSL (Windows Subsystem for Linux).
pub fn is_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::env::var_os("WSL_INTEROP").is_some()
        {
            return true;
        }
        if let Ok(version) = std::fs::read_to_string("/proc/version") {
            let lower = version.to_lowercase();
            if lower.contains("microsoft") || lower.contains("wsl") {
                return true;
            }
        }
    }
    false
}

/// Formats an OSC 8 terminal hyperlink sequence: `\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\`
#[must_use]
pub fn format_osc8_link(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

#[cfg(target_os = "windows")]
fn open_windows(target: &str) -> Result<OpenOutcome, OpenError> {
    Command::new("cmd")
        .args(["/c", "start", "", target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| OpenOutcome::Launched {
            launcher: "cmd /c start".into(),
        })
        .map_err(|e| OpenError::SpawnFailed {
            launcher: "cmd /c start".into(),
            source: e,
        })
}

#[cfg(target_os = "macos")]
fn open_macos(target: &str) -> Result<OpenOutcome, OpenError> {
    Command::new("open")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| OpenOutcome::Launched {
            launcher: "open".into(),
        })
        .map_err(|e| OpenError::SpawnFailed {
            launcher: "open".into(),
            source: e,
        })
}

#[cfg(target_os = "linux")]
fn open_wsl(target: &str) -> Result<OpenOutcome, OpenError> {
    for cmd in &["wslview", "cmd.exe"] {
        let res = if *cmd == "cmd.exe" {
            Command::new("cmd.exe")
                .args(["/c", "start", target])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        } else {
            Command::new(cmd)
                .arg(target)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        };
        if res.is_ok() {
            return Ok(OpenOutcome::Launched {
                launcher: cmd.to_string(),
            });
        }
    }
    Err(OpenError::NoOpenerFound)
}

fn open_linux(target: &str) -> Result<OpenOutcome, OpenError> {
    if let Ok(browser) = std::env::var("BROWSER")
        && !browser.is_empty()
        && Command::new(&browser)
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    {
        return Ok(OpenOutcome::Launched { launcher: browser });
    }

    let candidates = [
        "xdg-open",
        "gio",
        "sensible-browser",
        "x-www-browser",
        "google-chrome",
        "firefox",
        "chromium",
        "brave",
        "microsoft-edge",
    ];

    for &candidate in &candidates {
        let res = if candidate == "gio" {
            Command::new("gio")
                .args(["open", target])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        } else {
            Command::new(candidate)
                .arg(target)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        };
        if res.is_ok() {
            return Ok(OpenOutcome::Launched {
                launcher: candidate.into(),
            });
        }
    }

    Err(OpenError::NoOpenerFound)
}

fn open_generic_browser(target: &str) -> Result<OpenOutcome, OpenError> {
    open_linux(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_osc8_link_correctly() {
        let link = format_osc8_link("https://example.com", "Example");
        assert_eq!(
            link,
            "\x1b]8;;https://example.com\x1b\\Example\x1b]8;;\x1b\\"
        );
    }
}
