//! Cross-platform system clipboard integration and terminal OSC 52 fallback.
//!
//! Provides asynchronous reading and writing of:
//! - Plain text (via native clipboard, Wayland `wl-copy`/`wl-paste`, X11 `xclip`, macOS, Windows, and OSC 52)
//! - Image payloads (PNG bytes directly from clipboard)
//! - File drops (file paths copied in OS file managers: Finder, Nautilus/Dolphin, Explorer)

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Stdio;
#[cfg(target_os = "linux")]
use tokio::io::AsyncWriteExt;

/// The outcome of copying text to the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    Native,
    Osc52,
}

/// What `read()` found on the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardRead {
    /// An image (PNG bytes) plus its MIME type.
    Image { data: Vec<u8>, mime: String },
    /// File references copied in a file manager (e.g. Explorer, Finder, Nautilus).
    Files(Vec<PathBuf>),
    /// Plain text.
    Text(String),
    /// The clipboard is empty or unreadable.
    Empty,
}

/// Universal platform clipboard interface.
pub struct PlatformClipboard;

impl PlatformClipboard {
    /// Copy text through native clipboard owner when possible, falling back to terminal OSC 52.
    pub async fn copy_text(text: &str) -> Result<CopyOutcome, String> {
        let mut errors = Vec::new();

        #[cfg(target_os = "linux")]
        {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                match copy_with_command("wl-copy", &[], text).await {
                    Ok(()) => return Ok(CopyOutcome::Native),
                    Err(error) => errors.push(error),
                }
            }
        }

        match copy_system(text).await {
            Ok(()) => return Ok(CopyOutcome::Native),
            Err(error) => errors.push(error.to_string()),
        }

        write_osc52(text)
            .map(|_| CopyOutcome::Osc52)
            .map_err(|osc_error| {
                format!(
                    "native clipboard failed: {}; OSC52 failed: {}",
                    errors.join("; "),
                    osc_error
                )
            })
    }

    /// Read the system clipboard, preferring image, then file references, then plain text.
    pub async fn read() -> ClipboardRead {
        if let Some(bytes) = read_image_bytes().await {
            return ClipboardRead::Image {
                data: bytes,
                mime: "image/png".to_string(),
            };
        }
        let files = read_file_paths().await;
        if !files.is_empty() {
            return ClipboardRead::Files(files);
        }
        match read_text().await {
            Ok(Some(text)) if !text.is_empty() => ClipboardRead::Text(text),
            _ => ClipboardRead::Empty,
        }
    }

    /// Read plain text only from the system clipboard.
    pub async fn read_text() -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
        read_text().await
    }
}

/// Write an OSC52 "copy to clipboard" escape sequence to stdout.
///
/// Sequence: `ESC ] 52 ; c ; <base64> BEL`
/// In tmux: wrapped with `ESC P tmux ; ESC ... ESC \\`
/// In screen: wrapped with `ESC P ... ESC \\`
pub fn write_osc52(text: &str) -> io::Result<()> {
    let encoded = base64_encode(text);
    let sequence = format!("\x1b]52;c;{}\x07", encoded);

    let output = if std::env::var("TMUX").is_ok() {
        format!("\x1bPtmux;\x1b{}\x1b\\", sequence)
    } else if std::env::var("STY").is_ok() {
        format!("\x1bP{}\x1b\\", sequence)
    } else {
        sequence
    };

    let mut stdout = io::stdout();
    stdout.write_all(output.as_bytes())?;
    stdout.flush()
}

#[cfg(target_os = "linux")]
async fn copy_with_command(command: &str, args: &[&str], text: &str) -> Result<(), String> {
    let mut child = tokio::process::Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to start {}: {}", command, error))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("{} stdin was unavailable", command))?;
    stdin
        .write_all(text.as_bytes())
        .await
        .map_err(|error| format!("failed to write to {}: {}", command, error))?;
    drop(stdin);

    let status = child
        .wait()
        .await
        .map_err(|error| format!("{} failed: {}", command, error))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} exited with {}", command, status))
    }
}

async fn copy_system(text: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let text = text.to_string();
    tokio::task::spawn_blocking(move || {
        let mut clipboard = arboard::Clipboard::new()?;
        clipboard.set_text(text)?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    Ok(())
}

async fn read_text() -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    tokio::task::spawn_blocking(move || {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => return Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
        };
        match clipboard.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
        }
    })
    .await?
}

async fn read_image_bytes() -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        if let Some(bytes) = read_command_output("wl-paste", &["-t", "image/png"]).await
            && !bytes.is_empty()
        {
            return Some(bytes);
        }
        if let Some(bytes) = read_command_output(
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        )
        .await
            && !bytes.is_empty()
        {
            return Some(bytes);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(bytes) = read_macos_png().await {
            return Some(bytes);
        }
    }
    None
}

async fn read_file_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Some(bytes) = read_command_output("wl-paste", &["-t", "text/uri-list"]).await
            && let Some(paths) = parse_uri_list(&String::from_utf8_lossy(&bytes))
        {
            return paths;
        }
        if let Some(bytes) = read_command_output(
            "xclip",
            &["-selection", "clipboard", "-t", "text/uri-list", "-o"],
        )
        .await
            && let Some(paths) = parse_uri_list(&String::from_utf8_lossy(&bytes))
        {
            return paths;
        }
        for (command, args) in [
            ("wl-paste", &["-t", "x-special/gnome-copied-files"][..]),
            (
                "xclip",
                &[
                    "-selection",
                    "clipboard",
                    "-t",
                    "x-special/gnome-copied-files",
                    "-o",
                ][..],
            ),
        ] {
            if let Some(bytes) = read_command_output(command, args).await
                && let Some(paths) = parse_gnome_copied_files(&String::from_utf8_lossy(&bytes))
            {
                return paths;
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(paths) = read_macos_file_urls().await {
            return paths;
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(bytes) = read_command_output(
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$d = Get-Clipboard -Format FileDropList; if ($d) { $d | ForEach-Object { $_.FullName } }",
            ],
        )
        .await
        {
            return String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute() && path.exists())
                .collect();
        }
    }
    Vec::new()
}

#[cfg(target_os = "macos")]
async fn read_macos_png() -> Option<Vec<u8>> {
    let script = r#"
        use framework "AppKit"
        set pb to current application's NSPasteboard's generalPasteboard()
        set imgType to current application's NSPasteboardTypePNG
        set data to pb's dataForType:imgType
        if data is missing value then
            return ""
        end if
        return (data's base64EncodedStringWithOptions:0) as text
    "#;
    let bytes = read_command_output("osascript", &["-l", "AppleScript", "-e", script]).await?;
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    if text.is_empty() {
        return None;
    }
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

#[cfg(target_os = "macos")]
async fn read_macos_file_urls() -> Option<Vec<PathBuf>> {
    let script = r#"
        use framework "AppKit"
        set pb to current application's NSPasteboard's generalPasteboard()
        set urlType to current application's NSPasteboardTypeFileURL
        set readUrls to pb's readObjectsForClasses:{current application's NSURL} options:(missing value)
        if readUrls is missing value or (count of readUrls) is 0 then
            return ""
        end if
        set outText to ""
        repeat with oneUrl in readUrls
            set outText to outText & (oneUrl's |path|() as text) & linefeed
        end repeat
        return outText
    "#;
    let bytes = read_command_output("osascript", &["-l", "AppleScript", "-e", script]).await?;
    let text = String::from_utf8_lossy(&bytes);
    let paths: Vec<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.exists())
        .collect();
    if paths.is_empty() { None } else { Some(paths) }
}

async fn read_command_output(command: &str, args: &[&str]) -> Option<Vec<u8>> {
    let output = tokio::process::Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if output.status.success() && !output.stdout.is_empty() {
        Some(output.stdout)
    } else {
        None
    }
}

#[must_use]
pub fn parse_uri_list(input: &str) -> Option<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(file_uri_to_path)
        .filter(|path| path.is_absolute() && path.exists())
        .collect();
    if paths.is_empty() { None } else { Some(paths) }
}

#[must_use]
pub fn parse_gnome_copied_files(input: &str) -> Option<Vec<PathBuf>> {
    let mut lines = input.lines().map(str::trim);
    let action = lines.next()?;
    if action != "copy" && action != "cut" {
        return None;
    }
    let paths: Vec<PathBuf> = lines
        .filter(|line| !line.is_empty())
        .filter_map(file_uri_to_path)
        .filter(|path| path.is_absolute() && path.exists())
        .collect();
    if paths.is_empty() { None } else { Some(paths) }
}

/// Convert one `file://` URI to a local absolute path, or `None` when it
/// points at a remote host / isn't a file URI.
#[must_use]
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let host = &rest[..slash];
    if !host.is_empty() && host != "localhost" {
        return None;
    }
    let decoded = percent_decode(&rest[slash..]);
    #[cfg(target_os = "windows")]
    let decoded = {
        if let Some(drive) = decoded.strip_prefix('/')
            && drive
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            && drive.as_bytes().get(1) == Some(&b':')
        {
            drive.to_string()
        } else {
            decoded
        }
    };
    let path = std::path::Path::new(&decoded);
    path.is_absolute().then(|| path.to_path_buf())
}

/// Interpret a text paste payload as file references.
///
/// Returns the referenced files only when *every* non-empty line resolves to an
/// existing local file.
#[must_use]
pub fn paste_text_as_file_paths(payload: &str) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for line in payload.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let path = match file_uri_to_path(line) {
            Some(path) => path,
            None => {
                let bare = PathBuf::from(line);
                if !bare.is_absolute() {
                    return None;
                }
                bare
            }
        };
        if !path.is_file() {
            return None;
        }
        paths.push(path);
    }
    (!paths.is_empty()).then_some(paths)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(high << 4 | low);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[must_use]
pub fn base64_encode(input: &str) -> String {
    base64_encode_bytes(input.as_bytes())
}

#[must_use]
pub fn base64_encode_bytes(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_spaces_and_utf8() {
        assert_eq!(
            percent_decode("/path/to/my%20file.txt"),
            "/path/to/my file.txt"
        );
        assert_eq!(percent_decode("%E4%BD%A0%E5%A5%BD"), "你好");
    }

    #[test]
    fn base64_encoding_works() {
        assert_eq!(base64_encode("hello world"), "aGVsbG8gd29ybGQ=");
    }
}
