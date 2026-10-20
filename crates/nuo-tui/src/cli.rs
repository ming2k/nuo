//! Command-line contract for the `nuo` terminal application.
//!
//! `nuo` is a client of the Nuo server. It owns interactive and headless
//! terminal workflows; server lifecycle, configuration, credentials, MCP,
//! skills, and server administration belong to the `nuo` core command.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// What the user asked the terminal application to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Bare `nuo` / `nuo "prompt"`: open an interactive session.
    Fresh,
    /// `nuo run <prompt>`: execute a headless one-shot.
    Run { prompt: String },
    /// `nuo attach [id]`: join a hosted session, using the picker without id.
    Attach { id: Option<String> },
    /// Open the full-screen session dashboard.
    Dashboard,
    /// Open the full-screen settings view (optional category).
    Settings { category: Option<String> },
    /// `nuo completions <shell>`.
    Completions(Shell),
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h` / `help [topic]`.
    Help(Option<String>),
}

/// A shell whose completion script `nuo completions` can print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }
}

/// The parsed command line: the [`Mode`] plus terminal-app global options.
#[derive(Debug, Clone, PartialEq)]
pub struct CliArgs {
    pub mode: Mode,
    /// `--project <path>`: operate on the project at `<path>`.
    pub project: Option<PathBuf>,
    /// `--unattended`: run unattended without interactive human confirmations.
    pub unattended: bool,
    /// `--role <id>`: staff the new session with a role (`developer`, `philosophist`, `ops`, or user role).
    pub role: Option<String>,
    /// `--resume`: resume the most recent matching session instead of a new one.
    pub resume: bool,
    /// `--no-confinement`: run with workspace filesystem confinement disabled.
    pub no_confinement: bool,
    /// `--interactive` / `-i`: force the TUI when headless would apply.
    pub interactive: bool,
    /// `-p`/`--prompt`/`--print` or a positional prompt phrase.
    pub prompt: Option<String>,
    /// Whether the prompt came from `-p`/`--prompt` rather than positionally.
    pub prompt_from_flag: bool,
    /// `-j`/`--json`: structured output where supported.
    pub json: bool,
    /// `--remote <addr>` / `--token <token>`: server endpoint override.
    pub remote: Option<String>,
    pub token: Option<String>,
    /// `--port <n>`: TCP port to bind or connect to.
    pub port: Option<u16>,
    /// `--headless`: run non-interactively without TUI and stream output to stdout.
    pub headless: bool,
    /// `-t` / `--thread [id]`: attach to existing thread (picker if ID omitted).
    pub thread: Option<Option<String>>,
}

struct Spec {
    name: &'static str,
    about: &'static str,
}

const COMMANDS: &[Spec] = &[
    Spec {
        name: "attach",
        about: "attach the TUI to a hosted thread (picker when no id)",
    },
    Spec {
        name: "dashboard",
        about: "open the full-screen session dashboard",
    },
    Spec {
        name: "settings",
        about: "open the full-screen settings view (optional category)",
    },
    Spec {
        name: "completions",
        about: "print a shell completion script",
    },
    Spec {
        name: "help",
        about: "print help for a command",
    },
];

const CORE_COMMANDS: &[&str] = &[
    "server", "session", "config", "auth", "mcp", "skill", "doctor",
];

fn command_index() -> BTreeMap<&'static str, &'static str> {
    COMMANDS.iter().map(|spec| (spec.name, spec.name)).collect()
}

fn resolve(word: &str) -> Option<&'static Spec> {
    COMMANDS.iter().find(|spec| spec.name == word)
}

struct FlagError(String);

impl From<FlagError> for String {
    fn from(error: FlagError) -> Self {
        error.0
    }
}

impl FlagError {
    fn new(flag: &str, message: impl std::fmt::Display) -> Self {
        Self(format!("{flag}: {message}"))
    }
}

fn split_flag(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value)),
        _ => (arg, None),
    }
}

fn flag_value<'a, I: Iterator<Item = &'a String>>(
    flag: &str,
    inline: Option<&str>,
    iter: &mut I,
) -> Result<String, FlagError> {
    if let Some(value) = inline {
        return Ok(value.to_string());
    }
    iter.next()
        .cloned()
        .ok_or_else(|| FlagError::new(flag, "requires a value"))
}

/// Parse the command line. The caller owns error rendering and exit policy.
pub fn parse(args: &[String]) -> Result<CliArgs, String> {
    let mut project = None;
    let mut unattended = false;
    let mut role = None;
    let mut resume = false;
    let mut no_confinement = false;
    let mut interactive = false;
    let mut prompt = None;
    let mut prompt_from_flag = false;
    let mut json = false;
    let mut version = false;
    let mut remote = None;
    let mut token = None;
    let mut port = None;
    let mut headless = false;
    let mut thread = None;
    let mut rest = Vec::new();

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let (name, inline) = split_flag(arg);
        match name {
            "--project" => {
                project = Some(PathBuf::from(flag_value("--project", inline, &mut iter)?));
            }
            "--home" => {
                return Err(
                    "--home was removed: set the NUO_HOME environment variable instead (e.g. NUO_HOME=/tmp/dev)"
                        .into(),
                );
            }
            "--config-dir" | "--data-dir" | "--state-dir" | "--cache-dir" => {
                return Err(format!(
                    "{name} was removed: use matching NUO_*_DIR environment variables instead"
                ));
            }
            "--headless" => headless = true,
            "-t" | "--thread" => {
                if let Some(val) = inline {
                    thread = Some(Some(val.to_string()));
                } else if let Some(next) = iter.peek().filter(|n| !n.starts_with('-')) {
                    thread = Some(Some((*next).clone()));
                    let _ = iter.next();
                } else {
                    thread = Some(None);
                }
            }
            "--unattended" => unattended = true,
            "--role" | "-r" => role = Some(flag_value("-r/--role", inline, &mut iter)?),
            "--resume" => resume = true,
            "--no-confinement" => no_confinement = true,
            "--interactive" | "-i" => interactive = true,
            "--json" | "-j" => json = true,
            "--print" | "--prompt" | "-p" => {
                prompt = Some(flag_value("-p/--prompt", inline, &mut iter)?);
                prompt_from_flag = true;
            }
            "--remote" => remote = Some(flag_value("--remote", inline, &mut iter)?),
            "--token" => token = Some(flag_value("--token", inline, &mut iter)?),
            "--port" => {
                let p_str = flag_value("--port", inline, &mut iter)?;
                port = Some(
                    p_str
                        .parse::<u16>()
                        .map_err(|_| format!("'{p_str}' is not a valid port number"))?,
                );
            }
            "--version" | "-V" => version = true,
            "--attach" => {
                let id = match iter.peek() {
                    Some(next) if !next.starts_with('-') => iter.next().cloned(),
                    _ => None,
                };
                rest.push("attach".to_string());
                if let Some(id) = id {
                    rest.push(id);
                }
            }
            "--single-instance" => {
                return Err(
                    "--single-instance was removed: the unified server owns every session".into(),
                );
            }
            _ => rest.push(arg.clone()),
        }
    }

    let base = |mode| CliArgs {
        mode,
        project: project.clone(),
        unattended,
        role: role.clone(),
        resume,
        no_confinement,
        interactive,
        prompt: prompt.clone(),
        prompt_from_flag,
        json,
        remote: remote.clone(),
        token: token.clone(),
        port,
        headless,
        thread: thread.clone(),
    };
    let ok = |mode| Ok(base(mode));

    if version {
        return ok(Mode::Version);
    }

    if let Some(first) = rest.first().map(String::as_str) {
        if first == "-h" || first == "--help" || (first == "help" && rest.len() == 1) {
            return ok(Mode::Help(None));
        }
        if first == "help" {
            let topic = &rest[1];
            return if command_index().contains_key(topic.as_str()) {
                ok(Mode::Help(Some(topic.clone())))
            } else {
                Err(format!("unknown help topic '{topic}'"))
            };
        }
        if command_index().contains_key(first)
            && rest[1..].iter().any(|arg| arg == "-h" || arg == "--help")
        {
            return ok(Mode::Help(Some(first.to_string())));
        }
    }

    let Some(cmd) = rest.first().cloned() else {
        if let Some(ref t) = thread {
            return ok(Mode::Attach { id: t.clone() });
        }
        return ok(Mode::Fresh);
    };
    let extra = &rest[1..];
    let unexpected = |arg: &str| Err(format!("unexpected argument '{arg}' found for 'nuo {cmd}'"));

    if cmd == "run" {
        return Err(
            "'run' was removed in favor of '--headless' (ADR-0061).\n\n  tip: use 'nuo --headless <prompt>' or 'nuo -p <prompt>'".into(),
        );
    }

    if CORE_COMMANDS.contains(&cmd.as_str()) {
        return Err(format!(
            "'{cmd}' is a nuo service command; run `nuo {cmd}` instead"
        ));
    }

    // A multi-word unknown phrase is an interactive prompt. A single unknown
    // word remains an error so command typos do not silently reach the model.
    if resolve(&cmd).is_none() && !cmd.starts_with('-') {
        let positional_prompt = if rest.len() > 1 || cmd.contains(' ') {
            Some(rest.join(" "))
        } else {
            None
        };
        match positional_prompt.or_else(|| prompt.clone()) {
            Some(text) if rest.len() > 1 || cmd.contains(' ') || prompt_from_flag => {
                return ok(Mode::Fresh).map(|mut args| {
                    if args.prompt.is_none() {
                        args.prompt = Some(text);
                    }
                    args
                });
            }
            _ => {
                let tip = suggest_command(&cmd)
                    .map(|name| format!("\n\n  tip: a similar command exists: '{name}'"))
                    .unwrap_or_default();
                return Err(format!("unrecognized command '{cmd}'{tip}"));
            }
        }
    }

    let Some(spec) = resolve(&cmd) else {
        return Err(format!("unrecognized command '{cmd}'"));
    };
    let mode = match spec.name {
        "attach" => match extra {
            [] => Mode::Attach { id: None },
            [id] if !id.starts_with('-') => Mode::Attach {
                id: Some(id.clone()),
            },
            [bad, ..] => return unexpected(bad),
        },
        "dashboard" => match extra {
            [] => Mode::Dashboard,
            [bad, ..] => return unexpected(bad),
        },
        "settings" => match extra {
            [] => Mode::Settings { category: None },
            [category] => Mode::Settings {
                category: Some(category.clone()),
            },
            [bad, ..] => return unexpected(bad),
        },
        "completions" => match extra {
            [shell] => match Shell::from_name(shell) {
                Some(shell) => Mode::Completions(shell),
                None => {
                    return Err(format!(
                        "unknown shell '{shell}' (expected bash, zsh, or fish)"
                    ));
                }
            },
            [] => return Err("missing shell name (expected bash, zsh, or fish)".into()),
            [bad, ..] => return unexpected(bad),
        },
        "help" => Mode::Help(None),
        _ => unreachable!("closed command set"),
    };

    Ok(base(mode))
}

fn suggest_command(input: &str) -> Option<&'static str> {
    let index = command_index();
    if input.len() >= 2
        && let Some((_, canonical)) = index.iter().find(|(name, _)| name.starts_with(input))
    {
        return Some(canonical);
    }
    let tolerance = if input.len() >= 5 { 2 } else { 1 };
    index
        .iter()
        .filter(|(name, _)| levenshtein(input, name) <= tolerance)
        .min_by_key(|(name, _)| levenshtein(input, name))
        .map(|(_, canonical)| *canonical)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, left) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, right) in b.iter().enumerate() {
            current[j + 1] = (previous[j] + usize::from(left != right))
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// Top-level or per-command help text.
pub fn help_text(topic: Option<&str>) -> Option<String> {
    let mut out = String::new();
    match topic {
        None => {
            out.push_str("nuo — Nuo terminal application\n\n");
            out.push_str("Usage: nuo [OPTIONS] [PROMPT]\n");
            out.push_str("       nuo [OPTIONS] <COMMAND>\n\nCommands:\n");
            let width = COMMANDS
                .iter()
                .map(|spec| spec.name.len())
                .max()
                .unwrap_or(0);
            for spec in COMMANDS {
                out.push_str(&format!("  {:<width$}  {}\n", spec.name, spec.about));
            }
            out.push_str("\nOptions:\n");
            out.push_str(
                "  -t, --thread [id]      attach to existing thread (opens picker if id omitted)\n",
            );
            out.push_str(
                "      --headless         execute prompt non-interactively and stream to stdout\n",
            );
            out.push_str(
                "  -p, --prompt <prompt>  prompt for headless execution or initial TUI input\n",
            );
            out.push_str("  -i, --interactive      force interactive TUI mode\n");
            out.push_str("  -j, --json             emit structured JSON where supported\n");
            out.push_str(
                "  -r, --role <id>        staff the thread with a role (developer, philosophist, ops, or custom)\n",
            );
            out.push_str(
                "      --resume           resume the most recent matching thread instead of a new one\n",
            );
            out.push_str(
                "      --unattended       run unattended without interactive human confirmations\n",
            );
            out.push_str("      --no-confinement   disable workspace filesystem confinement (unconfined file access)\n");
            out.push_str("      --project <path>   operate on the project at <path>\n");
            out.push_str("      --remote <addr>    connect to a remote Nuo server\n");
            out.push_str("      --token <token>    bearer token for server connection\n");
            out.push_str("  -h, --help             print help ('nuo help <command>' for more)\n");
            out.push_str("  -V, --version          print the version and exit\n");
            out.push_str("\nWith no command, nuo opens a fresh interactive thread.\n");
            out.push_str("It checks the Nuo server first and starts `nuo` when needed.\n");
            out.push_str("Server and service administration remains under the `nuo` command.\n");
        }
        Some(topic) => {
            let spec = resolve(topic)?;
            out.push_str(&format!("nuo {} — {}\n\n", spec.name, spec.about));
            out.push_str(&format!("Usage: nuo {}\n", spec.name));
            match spec.name {
                "attach" => {
                    out.push_str("\nWith no id the TUI thread picker opens. Supplying an id\n");
                    out.push_str("attaches directly to that hosted thread.\n");
                }
                "settings" => {
                    out.push_str(
                        "\nOpen the full-screen settings view. Optionally specify a category name\n",
                    );
                    out.push_str(
                        "(appearance, components, search, web, system) or index (0..4).\n",
                    );
                }
                _ => {}
            }
        }
    }
    Some(out)
}

/// A static completion script generated from the same command table.
pub fn completion_script(shell: Shell) -> String {
    let commands = COMMANDS
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>()
        .join(" ");
    match shell {
        Shell::Bash => format!(
            "# bash completion for nuo — eval \"$(nuo completions bash)\"\n\
             _nuo() {{\n\
             \x20   local cur\n\
             \x20   cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n\
             \x20   if [[ $COMP_CWORD -eq 1 ]]; then\n\
             \x20       COMPREPLY=($(compgen -W \"{commands} --project --remote --token --prompt -p --interactive -i --json -j --role -r --resume --unattended --no-confinement --help --version\" -- \"$cur\"))\n\
             \x20   fi\n\
             }}\n\
             complete -F _nuo nuo\n"
        ),
        Shell::Zsh => {
            let entries = COMMANDS
                .iter()
                .map(|spec| format!("        '{}:{}'", spec.name, spec.about))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "#compdef nuo\n\
                 _arguments '1:command:(({entries}))' '*::argument:->args'\n"
            )
        }
        Shell::Fish => format!(
            "# fish completion for nuo\n\
             set -l cmds {commands}\n\
             complete -c nuo -n '__fish_use_subcommand' -f -a \"$cmds\"\n"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(tokens: &[&str]) -> Result<CliArgs, String> {
        super::parse(
            &tokens
                .iter()
                .map(|token| token.to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn bare_invocation_starts_a_fresh_tui_session() {
        assert!(matches!(parse(&[]).unwrap().mode, Mode::Fresh));
    }

    #[test]
    fn client_commands_remain_on_the_nuo_surface() {
        assert!(matches!(
            parse(&["attach"]).unwrap().mode,
            Mode::Attach { id: None }
        ));
        let run_err = parse(&["run", "hello"]).unwrap_err();
        assert!(run_err.contains("'run' was removed in favor of '--headless'"));
    }

    #[test]
    fn thread_flag_parses_cleanly() {
        let parsed = parse(&["-t"]).unwrap();
        assert_eq!(parsed.thread, Some(None));
        assert!(matches!(parsed.mode, Mode::Attach { id: None }));

        let parsed = parse(&["--thread", "thr-42"]).unwrap();
        assert_eq!(parsed.thread, Some(Some("thr-42".into())));
        assert!(matches!(parsed.mode, Mode::Attach { id: Some(ref id) } if id == "thr-42"));

        let parsed = parse(&["--headless", "-p", "fix it"]).unwrap();
        assert!(parsed.headless);
        assert_eq!(parsed.prompt.as_deref(), Some("fix it"));
    }

    #[test]
    fn core_commands_point_to_nuo() {
        for command in CORE_COMMANDS {
            let error = parse(&[command]).unwrap_err();
            assert!(error.contains("nuo service command"), "{command}: {error}");
        }
    }

    #[test]
    fn home_flag_is_rejected_with_env_guidance() {
        let err = parse(&["--home", "/tmp/nuo-test"]).unwrap_err();
        assert!(err.contains("NUO_HOME"), "{err}");
    }

    #[test]
    fn positional_prompt_is_preserved() {
        let parsed = parse(&["fix", "the", "build"]).unwrap();
        assert!(matches!(parsed.mode, Mode::Fresh));
        assert_eq!(parsed.prompt.as_deref(), Some("fix the build"));
    }

    #[test]
    fn settings_subcommand_parses_category() {
        let parsed = parse(&["settings"]).unwrap();
        assert_eq!(parsed.mode, Mode::Settings { category: None });

        let parsed = parse(&["settings", "web"]).unwrap();
        assert_eq!(
            parsed.mode,
            Mode::Settings {
                category: Some("web".into())
            }
        );

        let parsed = parse(&["settings", "appearance"]).unwrap();
        assert_eq!(
            parsed.mode,
            Mode::Settings {
                category: Some("appearance".into())
            }
        );
    }

    #[test]
    fn posture_flags_parse_cleanly() {
        let parsed = parse(&["--unattended"]).unwrap();
        assert!(parsed.unattended);
        assert!(!parsed.no_confinement);

        let parsed = parse(&["--unattended", "--no-confinement"]).unwrap();
        assert!(parsed.unattended);
        assert!(parsed.no_confinement);

        let parsed = parse(&["--no-confinement"]).unwrap();
        assert!(!parsed.unattended);
        assert!(parsed.no_confinement);

        // Old flags are completely dropped and error cleanly
        assert!(parse(&["--delegate"]).is_err());
        assert!(parse(&["--yolo"]).is_err());
        assert!(parse(&["--autopilot"]).is_err());
        assert!(parse(&["--unconfine"]).is_err());
        assert!(parse(&["--unconfined"]).is_err());
        assert!(parse(&["--no-jail"]).is_err());
        assert!(parse(&["--escape"]).is_err());
        assert!(parse(&["-y"]).is_err());
        assert!(parse(&["--auto"]).is_err());
    }

    #[test]
    fn role_and_resume_flags_parse_cleanly() {
        let parsed = parse(&["--role", "philosophist"]).unwrap();
        assert_eq!(parsed.role.as_deref(), Some("philosophist"));
        assert!(!parsed.resume);

        let parsed = parse(&["--role", "ops"]).unwrap();
        assert_eq!(parsed.role.as_deref(), Some("ops"));
        assert!(!parsed.resume);

        let parsed = parse(&["-r", "philosophist"]).unwrap();
        assert_eq!(parsed.role.as_deref(), Some("philosophist"));
        assert!(!parsed.resume);

        let parsed = parse(&["--role=developer", "--resume"]).unwrap();
        assert_eq!(parsed.role.as_deref(), Some("developer"));
        assert!(parsed.resume);
    }

    #[test]
    fn top_level_help_includes_role_and_omits_environment() {
        let help = help_text(None).expect("top-level help text");
        assert!(help.contains("--role <id>"), "help should document --role");
        assert!(help.contains("--resume"), "help should document --resume");
        assert!(
            !help.contains("Environment:"),
            "help should not expose environment variables"
        );
        assert!(
            !help.contains("NUO_HOME"),
            "help should not expose NUO_HOME"
        );
        assert!(
            !help.contains("NUO_STARTUP_VIEW"),
            "help should not expose NUO_STARTUP_VIEW"
        );
    }
}
