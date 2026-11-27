//! Quote-aware shell command tokenizer and permission-prefix classifier (ADR-0212).
//!
//! Splits compound shell pipelines/chains (`&&`, `||`, `;`, `|`, newlines) into
//! individual segments, detects shell hazards (redirections, substitutions,
//! subshells, and execution-hijacking environment overrides), and extracts
//! canonical command prefixes for safe `Always` permission suggestions.

/// Result of parsing a shell command string for permission evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    pub segments: Vec<CommandSegment>,
    /// True if the input contained any shell control operator (`&&`, `||`, `;`, `|`, `&`, `\n`),
    /// including a trailing `&` or `;`.
    pub is_compound: bool,
    /// True if any segment has unclosed quotes or invalid structure.
    pub malformed: bool,
}

/// One simple command segment between shell control operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSegment {
    /// Raw trimmed segment text.
    pub raw: String,
    /// Space-joined tokens after stripping safe leading `NAME=VALUE` env assignments.
    pub normalized_command: String,
    /// First non-env token (executable name).
    pub program: Option<String>,
    /// Tokens after `program`.
    pub args: Vec<String>,
    /// Second token when it is a plain subcommand word (not starting with `-`).
    pub subcommand: Option<String>,
    pub has_redirection: bool,
    pub has_substitution: bool,
    pub has_subshell: bool,
    pub has_dangerous_env: bool,
}

impl CommandSegment {
    /// Whether this segment is free of shell redirections, substitutions,
    /// subshells, and dangerous environment overrides, making it eligible for
    /// prefix-based permission matching.
    pub fn is_prefix_eligible(&self) -> bool {
        !self.has_redirection
            && !self.has_substitution
            && !self.has_subshell
            && !self.has_dangerous_env
            && self.program.is_some()
    }
}

/// Multi-word CLI tools whose second plain token forms a meaningful subcommand
/// prefix (e.g., `cargo test *`, `git status *`, `docker ps *`).
const SUBCOMMAND_PROGRAMS: &[&str] = &[
    "git",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "docker",
    "docker-compose",
    "kubectl",
    "helm",
    "terraform",
    "tofu",
    "gh",
    "glab",
    "rustup",
    "pip",
    "pip3",
    "uv",
    "poetry",
    "go",
    "systemctl",
    "journalctl",
    "apt",
    "apt-get",
    "dnf",
    "pacman",
    "brew",
    "nuo",
    "jj",
    "hg",
    "aws",
    "gcloud",
    "az",
];

/// Executables that interpret arbitrary code, spawn shells, escalate
/// privileges, or fetch remote code — never auto-suggest a wildcard `<prog> *`
/// for `Always`; require exact command approval or explicit config rules.
const NO_PREFIX_PROGRAMS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "csh",
    "tcsh",
    "ash",
    "cmd",
    "cmd.exe",
    "powershell",
    "pwsh",
    "python",
    "python2",
    "python3",
    "node",
    "deno",
    "ruby",
    "perl",
    "php",
    "lua",
    "awk",
    "gawk",
    "sed",
    "env",
    "sudo",
    "su",
    "doas",
    "pkexec",
    "xargs",
    "find",
    "eval",
    "exec",
    "source",
    "command",
    "builtin",
    "nohup",
    "timeout",
    "nice",
    "ionice",
    "stdbuf",
    "strace",
    "ltrace",
    "gdb",
    "lldb",
    "busybox",
    "curl",
    "wget",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "nc",
    "ncat",
    "socat",
    "rm",
    "rmdir",
    "mv",
    "cp",
    "ln",
    "install",
    "tee",
    "truncate",
    "shred",
    "dd",
    "mkfs",
    "chmod",
    "chown",
    "kill",
    "pkill",
    "killall",
];

/// Mutating or arbitrary-execution subcommands under [`SUBCOMMAND_PROGRAMS`]
/// that must never auto-generalize into `<prog> <sub> *` on `Always`.
const NO_PREFIX_SUBCOMMANDS: &[&str] = &[
    "run",
    "exec",
    "x",
    "dlx",
    "eval",
    "push",
    "pull",
    "reset",
    "rebase",
    "clean",
    "checkout",
    "restore",
    "commit",
    "merge",
    "cherry-pick",
    "config",
    "remote",
    "apply",
    "destroy",
    "delete",
    "rm",
    "rmi",
    "prune",
    "publish",
    "install",
    "uninstall",
    "add",
    "remove",
    "login",
    "logout",
];

/// Environment variables that can hijack dynamic linking, shell startup, PATH
/// resolution, or git command execution.
const DANGEROUS_ENV_VARS: &[&str] = &[
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "PATH",
    "IFS",
    "BASH_ENV",
    "ENV",
    "CDPATH",
    "SHELLOPTS",
    "BASHOPTS",
    "PERL5OPT",
    "PERL5LIB",
    "PYTHONSTARTUP",
    "PYTHONPATH",
    "NODE_OPTIONS",
    "RUBYOPT",
    "RUSTFLAGS",
    "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
    "GIT_SSH_COMMAND",
    "GIT_SSH",
    "GIT_EXEC_PATH",
    "GIT_EXTERNAL_DIFF",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "EDITOR",
    "VISUAL",
    "PAGER",
];

/// Parse a shell command string into segments and hazard metadata.
pub fn parse_shell_commands(command: &str) -> ParsedCommand {
    let raw_segments = split_shell_segments(command);
    let is_compound = raw_segments.saw_operator || raw_segments.segments.len() > 1;
    let segments = raw_segments
        .segments
        .into_iter()
        .map(|seg| analyze_segment(&seg))
        .collect();
    ParsedCommand {
        segments,
        is_compound,
        malformed: raw_segments.malformed,
    }
}

/// Suggest a safe prefix rule (e.g., `"cargo test *"`) for an interactive
/// `Always` grant on `execute_command`, or `None` if only exact matching is safe.
pub fn suggest_permission_scope(command: &str) -> Option<String> {
    let parsed = parse_shell_commands(command);
    if parsed.malformed || parsed.is_compound || parsed.segments.len() != 1 {
        return None;
    }
    let seg = &parsed.segments[0];
    if !seg.is_prefix_eligible() {
        return None;
    }
    let program = seg.program.as_deref()?;
    let prog_base = program.rsplit('/').next().unwrap_or(program);
    let prog_lower = prog_base.to_ascii_lowercase();

    if NO_PREFIX_PROGRAMS.contains(&prog_lower.as_str()) {
        return None;
    }

    if SUBCOMMAND_PROGRAMS.contains(&prog_lower.as_str()) {
        if let Some(sub) = seg.subcommand.as_deref() {
            if NO_PREFIX_SUBCOMMANDS.contains(&sub.to_ascii_lowercase().as_str()) {
                return None;
            }
            return Some(format!("{program} {sub} *"));
        }
        // Without a plain subcommand (e.g. `git --version`), stay exact.
        return None;
    }

    Some(format!("{program} *"))
}

/// Match a trailing-`*` prefix rule (`"<prefix> *"`) against a parsed command
/// segment on a word boundary.
///
/// Prefix rules only match simple, hazard-free segments. `"cargo test *"`
/// matches both `"cargo test"` and `"cargo test --lib"`, and refuses
/// `"cargo testx"`.
pub fn segment_matches_prefix(pattern: &str, segment: &CommandSegment) -> bool {
    let Some(prefix) = pattern.strip_suffix('*') else {
        return false;
    };
    if !segment.is_prefix_eligible() {
        return false;
    }
    let stem = prefix.trim_end();
    if stem.is_empty() {
        return false;
    }
    let cmd = segment.normalized_command.as_str();
    if cmd == stem {
        return true;
    }
    match cmd.strip_prefix(stem) {
        Some(rest) => rest.starts_with(' '),
        None => false,
    }
}

/// Returns `true` if a single parsed shell segment is provably read-only, free
/// of shell hazards, and confined to workspace-relative paths.
///
/// Used by `BrokerPolicy` to auto-approve safe workspace inspection commands
/// (`ls`, `cat`, `rg`, `git status`, `git diff`, `git log`, `cargo metadata`,
/// `pwd`, etc.) without prompting the user, while explicit deny rules still
/// take precedence before this check runs.
pub fn is_segment_safe_readonly(segment: &CommandSegment) -> bool {
    if !segment.is_prefix_eligible() {
        return false;
    }
    let Some(program) = segment.program.as_deref() else {
        return false;
    };
    // Require bare command names so local/arbitrary binaries like `./cat` or
    // `/tmp/evil/ls` are never auto-approved.
    if program.contains('/') || program.contains('\\') {
        return false;
    }
    let prog = program.to_ascii_lowercase();
    let args = &segment.args;

    match prog.as_str() {
        "pwd" | "whoami" | "id" | "uname" | "arch" | "nproc" | "which" | "type" | "echo"
        | "printf" => true,
        // POSIX `date` mutates the clock through `-s`/`--set` or a positional
        // date operand; only flag/format forms are read-only.
        "date" => args
            .iter()
            .all(|arg| (arg.starts_with('-') && !is_date_set_flag(arg)) || arg.starts_with('+')),
        // `hostname` with a positional name sets it; flags only read.
        "hostname" => args.iter().all(|arg| arg.starts_with('-')),
        "rustc" | "node" | "python" | "python3" | "deno" | "bun" | "go" => {
            matches!(
                args.first().map(String::as_str),
                Some("--version" | "-V" | "-v" | "version")
            ) && args.len() == 1
        }
        "ls" | "du" | "df" | "stat" | "file" | "wc" | "cat" | "head" | "tail" => {
            args.iter().all(|a| is_workspace_safe_token(a))
        }
        "tree" => args
            .iter()
            .all(|a| is_workspace_safe_token(a) && a != "-o" && !a.starts_with("-o")),
        "rg" | "grep" => args.iter().all(|a| {
            is_workspace_safe_token(a)
                && a != "--pre"
                && !a.starts_with("--pre=")
                && a != "--hostname-bin"
                && !a.starts_with("--hostname-bin=")
                && a != "-f"
                && a != "--file"
                && !a.starts_with("--file=")
        }),
        "git" => is_safe_readonly_git(args),
        "cargo" => is_safe_readonly_cargo(args),
        _ => false,
    }
}

fn is_workspace_safe_token(tok: &str) -> bool {
    if tok.starts_with('/') || tok.starts_with('~') || tok.contains('\\') {
        return false;
    }
    // Shell variable expansion (`$HOME`, `${HOME}`, PowerShell `$env:...`)
    // resolves outside the tokenizer's view; the workspace-relative guarantee
    // cannot hold, so never auto-approve it.
    if tok.contains('$') {
        return false;
    }
    // cmd.exe `%VAR%` expansion is the Windows equivalent.
    if has_percent_expansion(tok) {
        return false;
    }
    // Windows drive-relative/absolute paths (`C:/Users/...`, `C:foo`) are not
    // workspace-relative even though they do not start with `/` or `\`.
    if tok.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && tok.as_bytes().get(1) == Some(&b':')
    {
        return false;
    }
    if has_parent_traversal(tok) {
        return false;
    }
    if let Some((_, val)) = tok.split_once('=')
        && (val.starts_with('/') || val.starts_with('~') || has_parent_traversal(val))
    {
        return false;
    }
    if tok.starts_with('-') && !tok.starts_with("--") && tok.len() > 2 {
        let tail = &tok[2..];
        if tail.starts_with('/') || tail.starts_with('~') || has_parent_traversal(tail) {
            return false;
        }
    }
    true
}

/// `%NAME%` anywhere in a token is a cmd.exe environment expansion.
fn has_percent_expansion(tok: &str) -> bool {
    tok.match_indices('%').count() >= 2
}

/// `date -s`/`--set` (and their attached-value forms) mutate the system clock.
fn is_date_set_flag(arg: &str) -> bool {
    arg == "--set" || arg.starts_with("--set=") || (arg.starts_with("-s") && arg.len() >= 2)
}

fn has_parent_traversal(s: &str) -> bool {
    s == ".." || s.split('/').any(|part| part == "..")
}

fn is_safe_readonly_git(args: &[String]) -> bool {
    let Some(first) = args.first().map(String::as_str) else {
        return false;
    };
    if first == "--version" || first == "-v" || first == "version" {
        return args.len() == 1;
    }
    // Reject leading global git flags such as `git -c ...` or `git --git-dir=...`.
    if first.starts_with('-') {
        return false;
    }
    let rest = &args[1..];
    if !rest.iter().all(|a| {
        is_workspace_safe_token(a)
            && a != "-o"
            && !a.starts_with("-o")
            && a != "--output"
            && !a.starts_with("--output=")
            && a != "--ext-diff"
            && a != "--exec-path"
            && !a.starts_with("--exec-path=")
            && a != "--git-dir"
            && !a.starts_with("--git-dir=")
            && a != "--work-tree"
            && !a.starts_with("--work-tree=")
            // `git grep -O<cmd>` / `--open-files-in-pager[=<cmd>]` executes a
            // caller-supplied program.
            && a != "-O"
            && !a.starts_with("-O")
            && a != "--open-files-in-pager"
            && !a.starts_with("--open-files-in-pager=")
    }) {
        return false;
    }

    match first {
        "status" | "diff" | "log" | "show" | "rev-parse" | "ls-files" | "grep" | "blame"
        | "describe" | "shortlog" => true,
        "branch" => rest.iter().all(|a| {
            matches!(
                a.as_str(),
                "-a" | "--all"
                    | "-r"
                    | "--remotes"
                    | "-v"
                    | "-vv"
                    | "--verbose"
                    | "-l"
                    | "--list"
                    | "--show-current"
                    | "--no-color"
                    | "--color"
                    | "--merged"
                    | "--no-merged"
                    | "--column"
            ) || a.starts_with("--format=")
                || a.starts_with("--sort=")
                || a.starts_with("--color=")
                || a.starts_with("--abbrev=")
        }),
        "tag" => {
            rest.is_empty()
                || rest.iter().all(|a| {
                    matches!(a.as_str(), "-l" | "--list" | "-v" | "--verify")
                        || a.starts_with("--sort=")
                        || a.starts_with("--format=")
                })
        }
        "remote" => {
            rest.is_empty()
                || matches!(rest, [arg] if arg == "-v" || arg == "--verbose")
                || matches!(rest, [sub, name] if sub == "show" && !name.starts_with('-'))
        }
        _ => false,
    }
}

fn is_safe_readonly_cargo(args: &[String]) -> bool {
    let Some(first) = args.first().map(String::as_str) else {
        return false;
    };
    if first == "--version" || first == "-V" || first == "version" {
        return args.len() == 1;
    }
    if !matches!(first, "metadata" | "tree" | "pkgid" | "locate-project") {
        return false;
    }
    args[1..].iter().all(|a| is_workspace_safe_token(a))
}

struct RawSplit {
    segments: Vec<String>,
    saw_operator: bool,
    malformed: bool,
}

fn split_shell_segments(input: &str) -> RawSplit {
    let bytes = input.as_bytes();
    let mut segments = Vec::new();
    let mut saw_operator = false;
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut paren_depth = 0usize;
    let mut backtick = false;

    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            in_double = true;
            i += 1;
            continue;
        }
        if b == b'`' {
            backtick = !backtick;
            i += 1;
            continue;
        }
        if backtick {
            i += 1;
            continue;
        }
        if b == b'(' {
            paren_depth += 1;
            i += 1;
            continue;
        }
        if b == b')' {
            if paren_depth == 0 {
                return RawSplit {
                    segments: vec![input.trim().to_string()],
                    saw_operator: false,
                    malformed: true,
                };
            }
            paren_depth -= 1;
            i += 1;
            continue;
        }
        if paren_depth > 0 {
            i += 1;
            continue;
        }

        // Control operators outside quotes and parens.
        let op_len = if i + 1 < bytes.len() && matches!(&bytes[i..i + 2], b"&&" | b"||" | b"|&") {
            Some(2)
        } else if b == b';' || b == b'\n' {
            Some(1)
        } else if b == b'|' {
            // Note: `>|` is clobber redirection, not a pipe!
            let prev_is_gt = i > 0 && bytes[i - 1] == b'>';
            if prev_is_gt { None } else { Some(1) }
        } else if b == b'&' {
            // Note: `>&` and `&>` are redirections, not background operators.
            let prev_is_redir = i > 0 && (bytes[i - 1] == b'>' || bytes[i - 1] == b'<');
            let next_is_gt = i + 1 < bytes.len() && bytes[i + 1] == b'>';
            if prev_is_redir || next_is_gt {
                None
            } else {
                Some(1)
            }
        } else {
            None
        };

        if let Some(len) = op_len {
            saw_operator = true;
            let seg = input[start..i].trim();
            if !seg.is_empty() {
                segments.push(seg.to_string());
            }
            i += len;
            start = i;
        } else {
            i += 1;
        }
    }

    let tail = input[start..].trim();
    if !tail.is_empty() {
        segments.push(tail.to_string());
    }

    let malformed = in_single || in_double || escaped || backtick || paren_depth > 0;
    RawSplit {
        segments,
        saw_operator,
        malformed,
    }
}

fn analyze_segment(raw: &str) -> CommandSegment {
    let scan = scan_tokens(raw);
    let mut has_dangerous_env = false;
    let mut cmd_tokens: Vec<&str> = Vec::new();
    let mut past_env = false;

    for tok in &scan.tokens {
        if !past_env && is_env_assignment(tok) {
            let name = tok.split('=').next().unwrap_or("");
            if DANGEROUS_ENV_VARS.contains(&name) {
                has_dangerous_env = true;
            }
            continue;
        }
        past_env = true;
        cmd_tokens.push(tok.as_str());
    }

    let program = cmd_tokens.first().map(|s| (*s).to_string());
    let args = cmd_tokens
        .iter()
        .skip(1)
        .map(|s| (*s).to_string())
        .collect();
    let subcommand = cmd_tokens
        .get(1)
        .filter(|tok| is_plain_subcommand(tok))
        .map(|s| (*s).to_string());
    let normalized_command = cmd_tokens.join(" ");

    CommandSegment {
        raw: raw.to_string(),
        normalized_command,
        program,
        args,
        subcommand,
        has_redirection: scan.has_redirection,
        has_substitution: scan.has_substitution,
        has_subshell: scan.has_subshell,
        has_dangerous_env,
    }
}

struct TokenScan {
    tokens: Vec<String>,
    has_redirection: bool,
    has_substitution: bool,
    has_subshell: bool,
}

fn scan_tokens(segment: &str) -> TokenScan {
    let chars: Vec<char> = segment.chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut has_redirection = false;
    let mut has_substitution = false;
    let mut has_subshell = false;

    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    while i < chars.len() {
        let ch = chars[i];
        if escaped {
            current.push(ch);
            has_token = true;
            escaped = false;
            i += 1;
            continue;
        }
        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            has_token = true;
            i += 1;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            has_token = true;
            i += 1;
            continue;
        }
        if in_double {
            if ch == '"' {
                in_double = false;
            } else {
                if ch == '`' || (ch == '$' && chars.get(i + 1) == Some(&'(')) {
                    has_substitution = true;
                }
                current.push(ch);
            }
            has_token = true;
            i += 1;
            continue;
        }

        match ch {
            '\'' => {
                in_single = true;
                has_token = true;
            }
            '"' => {
                in_double = true;
                has_token = true;
            }
            '`' => {
                has_substitution = true;
                current.push(ch);
                has_token = true;
            }
            '$' if chars.get(i + 1) == Some(&'(') => {
                has_substitution = true;
                current.push(ch);
                has_token = true;
            }
            '<' | '>' if chars.get(i + 1) == Some(&'(') => {
                has_substitution = true;
                current.push(ch);
                has_token = true;
            }
            '>' | '<' => {
                has_redirection = true;
                current.push(ch);
                has_token = true;
            }
            '(' | ')' => {
                has_subshell = true;
                current.push(ch);
                has_token = true;
            }
            c if c.is_ascii_whitespace() => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
        i += 1;
    }

    if has_token {
        tokens.push(current);
    }

    if tokens
        .first()
        .is_some_and(|first| first == "{" || first == "}")
    {
        has_subshell = true;
    }

    TokenScan {
        tokens,
        has_redirection,
        has_substitution,
        has_subshell,
    }
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _value)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_plain_subcommand(token: &str) -> bool {
    !token.is_empty()
        && !token.starts_with('-')
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_subcommand_prefix_for_cargo_and_git() {
        assert_eq!(
            suggest_permission_scope("cargo test --lib"),
            Some("cargo test *".to_string())
        );
        assert_eq!(
            suggest_permission_scope("git status -s"),
            Some("git status *".to_string())
        );
        assert_eq!(
            suggest_permission_scope("RUST_LOG=debug cargo check"),
            Some("cargo check *".to_string())
        );
        assert_eq!(
            suggest_permission_scope("git --version"),
            None,
            "flags are not subcommands; stay exact"
        );
    }

    #[test]
    fn suggests_single_verb_prefix_for_safe_commands() {
        assert_eq!(
            suggest_permission_scope("echo hello world"),
            Some("echo *".to_string())
        );
        assert_eq!(
            suggest_permission_scope("rg -n foo src/"),
            Some("rg *".to_string())
        );
    }

    #[test]
    fn refuses_prefix_suggestion_for_interpreters_shells_and_hazards() {
        for cmd in [
            "bash -c 'echo hi'",
            "python3 script.py",
            "node index.js",
            "sudo ls",
            "timeout 10 ls",
            "tee /etc/passwd",
            "curl https://example.com",
            "rm foo.txt",
            "git push origin main",
            "cargo run --bin nuo",
            "docker exec -it c sh",
            "cargo test && echo ok",
            "echo hi &",
            "echo hi > out.txt",
            "grep root < /etc/shadow",
            "echo $(whoami)",
            "echo `whoami`",
            "(echo hi)",
            "LD_PRELOAD=/tmp/x.so echo hi",
            "PATH=/tmp echo hi",
        ] {
            assert_eq!(
                suggest_permission_scope(cmd),
                None,
                "must not suggest wildcard prefix for {cmd:?}"
            );
        }
    }

    #[test]
    fn compound_split_respects_quotes_and_redirections() {
        let p = parse_shell_commands("echo 'a && b' && cargo test -p nuo; git status 2>&1");
        assert!(p.is_compound);
        assert!(!p.malformed);
        assert_eq!(p.segments.len(), 3);
        assert_eq!(p.segments[0].program.as_deref(), Some("echo"));
        assert_eq!(p.segments[1].program.as_deref(), Some("cargo"));
        assert_eq!(p.segments[1].subcommand.as_deref(), Some("test"));
        assert_eq!(p.segments[2].program.as_deref(), Some("git"));
        assert!(p.segments[2].has_redirection);
    }

    #[test]
    fn word_boundary_prefix_matches_with_and_without_args() {
        let bare = parse_shell_commands("cargo test");
        assert!(segment_matches_prefix("cargo test *", &bare.segments[0]));

        let with_args = parse_shell_commands("cargo test --lib");
        assert!(segment_matches_prefix(
            "cargo test *",
            &with_args.segments[0]
        ));

        let similar = parse_shell_commands("cargo testx");
        assert!(!segment_matches_prefix(
            "cargo test *",
            &similar.segments[0]
        ));

        let redirected = parse_shell_commands("cargo test > log.txt");
        assert!(!segment_matches_prefix(
            "cargo test *",
            &redirected.segments[0]
        ));
    }

    #[test]
    fn readonly_classifier_accepts_safe_workspace_inspection() {
        for cmd in [
            "ls -la src",
            "cat Cargo.toml",
            "rg -n foo crates/nuo-guardrails",
            "git status -s",
            "git diff HEAD~1 -- src/lib.rs",
            "git log --oneline -n 10",
            "git branch -a",
            "git remote -v",
            "cargo metadata --no-deps",
            "rustc --version",
            "pwd",
            "date +%s",
            "date -u",
            "hostname -f",
            "git grep -n foo",
            "git log --format=%H -n 5",
        ] {
            let parsed = parse_shell_commands(cmd);
            assert_eq!(parsed.segments.len(), 1, "for {cmd}");
            assert!(
                is_segment_safe_readonly(&parsed.segments[0]),
                "expected readonly for `{cmd}`"
            );
        }
    }

    #[test]
    fn readonly_classifier_rejects_escapes_hazards_and_mutations() {
        for cmd in [
            "cat /etc/passwd",
            "cat ~/secret.txt",
            "cat ../other_repo/Cargo.toml",
            "ls /tmp",
            "rg --pre=evil foo",
            "rg --pre evil foo",
            "tree -o out.txt",
            "./cat Cargo.toml",
            "git -c core.fsmonitor=evil status",
            "git status > out.txt",
            "git branch feature-new",
            "git branch -D main",
            "git checkout -- .",
            "git commit -m test",
            "git diff --output=diff.patch",
            "git diff --ext-diff",
            "cargo test",
            "cargo metadata --manifest-path /etc/Cargo.toml",
            "python3 -c 'print(1)'",
            "echo $(whoami)",
            "LD_PRELOAD=/tmp/lib.so ls",
            // Shell environment expansion resolves outside the token's view.
            "cat $HOME/.ssh/id_rsa",
            "cat \"$HOME/.ssh/id_rsa\"",
            "cat ${HOME}/.ssh/id_rsa",
            "ls $HOME",
            // cmd.exe expansion and Windows drive paths are not workspace-relative.
            "cat %USERPROFILE%/secret.txt",
            "cat C:/Windows/win.ini",
            "cat C:secret.txt",
            // Read-only-looking commands whose flags execute or mutate.
            "git grep -Oevil foo",
            "git grep -O evil foo",
            "git grep --open-files-in-pager=sh foo",
            "date -s 2020-01-01",
            "date --set=2020-01-01",
            "hostname evil",
        ] {
            let parsed = parse_shell_commands(cmd);
            assert!(
                !parsed.segments.iter().all(is_segment_safe_readonly),
                "expected rejection for `{cmd}`"
            );
        }
    }
}
