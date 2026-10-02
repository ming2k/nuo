//! Presenter for `execute_command` (and legacy persisted `bash` steps).

use super::{ArgLayout, ResultKind, ToolPresenter, ToolView, truncate};

pub struct ExecuteCommandPresenter;

impl ToolPresenter for ExecuteCommandPresenter {
    fn summary(&self, view: &ToolView) -> String {
        view.str("command")
            .and_then(extract_comm)
            .map(|comm| format!("Run {}", truncate(&comm, 64)))
            .unwrap_or_else(|| "Run command".to_string())
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Command
    }

    fn arg_layout(&self) -> ArgLayout {
        ArgLayout::Command
    }

    /// Collapsed by default: the summary line ("Run cargo test · 0ms")
    /// covers the common case, and verbose command output otherwise dominates
    /// the transcript. Failures still force-expand (lifecycle rule in
    /// `step_interaction::default_tool_expanded`), and
    /// `[tui.default_expanded] execute_command = true` restores the old
    /// open-by-default behavior.
    fn default_expanded(&self) -> bool {
        false
    }
}

/// Canonical executable base name (`comm` in Linux `/proc/[pid]/comm` / `pkill <comm>`).
///
/// Strips directory paths (`/usr/bin/`, `C:\tools\`), Windows file extensions (`.exe`, `.cmd`),
/// and wrapper utilities (`sudo`, `env`, `nohup`).
pub fn extract_comm(command: &str) -> Option<String> {
    for words in shell_segments(command) {
        let mut words_iter = words.into_iter().skip_while(|word| is_assignment(word));
        while let Some(candidate) = words_iter.next() {
            if matches!(
                candidate.as_str(),
                "cd" | "export" | "unset" | "set" | "source" | "." | "umask" | "ulimit"
            ) {
                break;
            }

            let mut comm = normalize_comm(&candidate);
            if comm.is_empty() {
                continue;
            }

            if is_wrapper(&comm) {
                let mut next_target = None;
                while let Some(arg) = words_iter.next() {
                    if arg.starts_with('-') {
                        if arg == "-u" || arg == "-g" || arg == "-C" {
                            let _ = words_iter.next();
                        }
                        continue;
                    }
                    if is_assignment(&arg) {
                        continue;
                    }
                    next_target = Some(arg);
                    break;
                }
                if let Some(target) = next_target {
                    comm = normalize_comm(&target);
                } else {
                    continue;
                }
            }

            if !comm.is_empty() {
                return Some(comm);
            }
        }
    }
    None
}

/// Normalizes a path or command string into its bare `comm` name.
/// - Linux/macOS: `/usr/bin/cargo` -> `cargo`, `./scripts/test.sh` -> `test.sh`
/// - Windows: `C:\tools\cargo.exe` -> `cargo`, `npm.cmd` -> `npm`
pub fn normalize_comm(candidate: &str) -> String {
    let name = candidate
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(candidate);

    let lower = name.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1", ".com"] {
        if lower.ends_with(ext) {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    name.to_string()
}

fn is_wrapper(comm: &str) -> bool {
    matches!(
        comm,
        "sudo" | "env" | "nohup" | "time" | "nice" | "xargs" | "exec" | "doas" | "chroot"
    )
}

/// Human-readable command summary for the header.
///
/// Surfaces the executable (`comm`) and key arguments (e.g. `Run python3 test.py`,
/// `Run cargo test`) while skipping shell setup boilerplate (`cd ... &&`,
/// variable assignments, wrapper utilities). Multi-line scripts use their first executable segment.
#[cfg(test)]
fn command_summary(command: &str) -> Option<String> {
    for words in shell_segments(command) {
        let mut words_iter = words.into_iter().skip_while(|word| is_assignment(word));
        while let Some(candidate) = words_iter.next() {
            // These mutate the shell rather than starting the workload. Look in
            // the following `&&`, `;`, or newline-delimited segment instead.
            if matches!(
                candidate.as_str(),
                "cd" | "export" | "unset" | "set" | "source" | "." | "umask" | "ulimit"
            ) {
                break;
            }

            let mut comm = normalize_comm(&candidate);
            if comm.is_empty() {
                continue;
            }

            // Peel away wrapper tools (sudo, env, nohup, etc.) to reveal the real command
            if is_wrapper(&comm) {
                let mut next_target = None;
                while let Some(arg) = words_iter.next() {
                    if arg.starts_with('-') {
                        if arg == "-u" || arg == "-g" || arg == "-C" {
                            let _ = words_iter.next();
                        }
                        continue;
                    }
                    if is_assignment(&arg) {
                        continue;
                    }
                    next_target = Some(arg);
                    break;
                }
                if let Some(target) = next_target {
                    comm = normalize_comm(&target);
                } else {
                    continue;
                }
            }

            let rest: Vec<String> = words_iter
                .map(|w| super::sanitize_single_line(&w))
                .filter(|w| !w.is_empty())
                .collect();
            if rest.is_empty() {
                return Some(comm);
            } else {
                return Some(format!("{} {}", comm, rest.join(" ")));
            }
        }
    }
    None
}

/// Tokenize enough POSIX-shell syntax to locate command boundaries and the
/// first executable word. This is presentation-only: it deliberately does
/// not expand variables or substitutions, but it does preserve quoted paths
/// and avoids treating arguments after `&&`, `;`, pipes, or newlines as part
/// of the same command.
fn shell_segments(command: &str) -> Vec<Vec<String>> {
    #[derive(Clone, Copy)]
    enum Quote {
        Single,
        Double,
    }

    fn finish_word(word: &mut String, segment: &mut Vec<String>) {
        if !word.is_empty() {
            segment.push(std::mem::take(word));
        }
    }

    fn finish_segment(
        word: &mut String,
        segment: &mut Vec<String>,
        segments: &mut Vec<Vec<String>>,
    ) {
        finish_word(word, segment);
        if !segment.is_empty() {
            segments.push(std::mem::take(segment));
        }
    }

    let mut segments = Vec::new();
    let mut segment = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            Some(Quote::Single) => {
                if ch == '\'' {
                    quote = None;
                } else {
                    word.push(ch);
                }
            }
            Some(Quote::Double) => {
                if ch == '"' {
                    quote = None;
                } else if ch == '\\' {
                    if let Some(&next_ch) = chars.peek() {
                        if matches!(next_ch, '"' | '\\' | '$' | '`' | '\n') {
                            chars.next();
                            if next_ch != '\n' {
                                word.push(next_ch);
                            }
                        } else {
                            word.push('\\');
                        }
                    } else {
                        word.push('\\');
                    }
                } else {
                    word.push(ch);
                }
            }
            None => match ch {
                '\'' => quote = Some(Quote::Single),
                '"' => quote = Some(Quote::Double),
                '\\' => {
                    if let Some(&next_ch) = chars.peek() {
                        if next_ch.is_whitespace()
                            || matches!(next_ch, '\'' | '"' | ';' | '|' | '&' | '\n' | '\\')
                        {
                            chars.next();
                            if next_ch != '\n' {
                                word.push(next_ch);
                            }
                        } else {
                            word.push('\\');
                        }
                    } else {
                        word.push('\\');
                    }
                }
                '\n' | ';' | '|' | '&' => finish_segment(&mut word, &mut segment, &mut segments),
                ch if ch.is_whitespace() => finish_word(&mut word, &mut segment),
                _ => word.push(ch),
            },
        }
    }

    finish_segment(&mut word, &mut segment, &mut segments);
    segments
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    matches!(chars.next(), Some('_') | Some('a'..='z') | Some('A'..='Z'))
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::{command_summary, extract_comm, normalize_comm};

    #[test]
    fn command_summary_includes_executable_and_args() {
        assert_eq!(command_summary("cargo build"), Some("cargo build".into()));
        assert_eq!(
            command_summary("/opt/local/bin/muta-server --listen 8080"),
            Some("muta-server --listen 8080".into())
        );
        assert_eq!(
            command_summary("'/opt/Long Path/worker' --serve"),
            Some("worker --serve".into())
        );
    }

    #[test]
    fn command_summary_skips_shell_setup_and_assignments() {
        assert_eq!(
            command_summary("cd '/tmp/work tree' && RUST_LOG=debug cargo run --release"),
            Some("cargo run --release".into())
        );
        assert_eq!(
            command_summary("export MODE=test; ./target/debug/worker | tee worker.log"),
            Some("worker".into())
        );
    }

    #[test]
    fn command_summary_sanitizes_multiline_script_arguments() {
        assert_eq!(
            command_summary("python3 -c 's=open(\"foo\").read()\nprint(s)'"),
            Some("python3 -c s=open(\"foo\").read() print(s)".into())
        );
    }

    #[test]
    fn command_comm_extracts_canonical_name() {
        assert_eq!(extract_comm("cargo test"), Some("cargo".into()));
        assert_eq!(
            extract_comm("sudo -E /usr/bin/git status"),
            Some("git".into())
        );
        assert_eq!(
            extract_comm("env RUST_LOG=info ./target/debug/muta-server"),
            Some("muta-server".into())
        );
        assert_eq!(
            extract_comm(r#""C:\Program Files\Git\bin\git.exe" commit -m "fix""#),
            Some("git".into())
        );
        assert_eq!(
            extract_comm(r#"C:\Tools\Git\bin\git.exe status"#),
            Some("git".into())
        );
        assert_eq!(
            extract_comm(r#"C:\Program\ Files\Git\bin\git.exe commit"#),
            Some("git".into())
        );
        assert_eq!(extract_comm("npm.cmd run build"), Some("npm".into()));
    }

    #[test]
    fn normalize_comm_handles_cross_platform_binaries() {
        assert_eq!(normalize_comm("/usr/local/bin/ripgrep"), "ripgrep");
        assert_eq!(normalize_comm(r#"C:\Tools\cargo.exe"#), "cargo");
        assert_eq!(normalize_comm("pkill"), "pkill");
        assert_eq!(normalize_comm("build.ps1"), "build");
        assert_eq!(normalize_comm("script.cmd"), "script");
        assert_eq!(normalize_comm("./test.sh"), "test.sh");
    }
}
