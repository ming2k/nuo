//! The `nuo` command line — modern, pure, uncompromising CLI topology.
//!
//! - **Pure resource-verb topology**: Server operations are unified under
//!   `nuo server <start|stop|reload|restart|status|token>`. Threads are
//!   managed under `nuo thread <ls|rm>`. Configurations under `nuo config`.
//! - **Zero scene leakage**: Internal TUI scenes (`dashboard`, `settings`,
//!   `attach`) belong strictly inside the interactive experience, never exposed
//!   as redundant top-level CLI commands.
//! - **Single clean entrypoints**:
//!   - Bare `nuo`: launches the interactive semantic terminal.
//!   - `nuo run <prompt>` / `-p`: executes headless turns.
//!   - `nuo status`: high-level application health and session dashboard.
//! - **The parser is a table, not a hand-rolled ladder.** One spec drives
//!   parsing, help, error messages, and shell completions.
//!
//! This module decides *what was asked*; the dispatch in `main.rs` decides
//! *what happens*.

use std::collections::BTreeMap;
use std::path::PathBuf;

// The parsed command line

/// What the user asked the binary to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Interactive TUI session, thread picker, or headless run (`nuo` unified engine per ADR-0005, ADR-0048, ADR-0061).
    Interactive(nuo_tui::cli::CliArgs),
    /// `nuo thread …` (ADR-0043, ADR-0048, ADR-0061).
    Thread(ThreadAction),
    /// Session server commands and lifecycle operations (`nuo server …`).
    Server(ServerAction),
    /// Developer offline tools (`nuo dev storybook`).
    Dev(DevAction),
    /// `nuo completions <shell>`.
    Completions(Shell),
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h` / `help [topic]`.
    Help(Option<String>),
}

/// `nuo dev …` offline developer tools (ADR-0073)
#[derive(Debug, Clone, PartialEq)]
pub enum DevAction {
    Storybook,
    Export(PathBuf),
}

/// `nuo thread …` / `nuo session …` (ADR-0043, ADR-0048, ADR-0061)
#[derive(Debug, Clone, PartialEq)]
pub enum ThreadAction {
    /// `nuo thread` or `nuo thread open [id]` — launch interactive TUI into the thread picker or specific thread.
    Open(Option<String>),
    /// `nuo thread ls` — list hosted threads.
    List { watch: bool, json: bool, all: bool },
    /// `nuo thread rm <id>` — terminate a hosted thread by id.
    Delete(String),
}

/// `nuo server` actions (start, stop, reload, restart, status, token)
#[derive(Debug, Clone, PartialEq)]
pub enum ServerAction {
    /// `nuo server start` — start the session server.
    Start {
        /// `--fg`: run in foreground (default: background detachment; ADR-0048, ADR-0078).
        foreground: bool,
        port: Option<u16>,
        public: bool,
        no_local_auth: bool,
        idle_exit_minutes: Option<u64>,
        shutdown_grace_secs: Option<u64>,
        zero_tcp: bool,
    },
    /// `nuo server stop` — graceful, budget-aware drain.
    Stop,
    /// `nuo server reload` — soft reload (ADR-0034 Level 1):
    /// re-read configuration and re-sync MCP + skills without dropping
    /// connections.
    Reload,
    /// `nuo server restart` — restart or replace the server instance.
    Restart {
        force: bool,
        port: Option<u16>,
        public: bool,
        no_local_auth: bool,
        idle_exit_minutes: Option<u64>,
        shutdown_grace_secs: Option<u64>,
        zero_tcp: bool,
    },
    /// `nuo server token` — print the local server's bearer token.
    Token,
    /// `nuo server status` / `nuo status` — the server's session table and endpoints.
    Status {
        watch: bool,
        json: bool,
        include_idle: bool,
        diagnostic: bool,
    },
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

/// The parsed command line: the [`Mode`] plus the global options.
#[derive(Debug, Clone)]
pub struct CliArgs {
    pub mode: Mode,
    /// `--project <path>`: operate on the project at `<path>`.
    pub project: Option<PathBuf>,
}

// The command spec — one source of truth for parse, help, and completion

/// A command or subcommand entry.
struct Spec {
    /// The canonical name (what help and completion advertise).
    name: &'static str,
    /// Accepted spellings — canonical first, aliases after. Aliases parse
    /// but are not advertised: they exist to retire gently.
    names: &'static [&'static str],
    about: &'static str,
}

const THREAD_SUBS: &[Spec] = &[
    Spec {
        name: "open",
        names: &["open", "attach", "resume"],
        about: "open interactive thread picker or attach to thread by id",
    },
    Spec {
        name: "ls",
        names: &["ls", "list"],
        about: "list hosted threads",
    },
    Spec {
        name: "rm",
        names: &["rm", "delete"],
        about: "terminate a hosted thread by id",
    },
];

const SERVER_SUBS: &[Spec] = &[
    Spec {
        name: "start",
        names: &["start"],
        about: "start the session server (detached background by default; --fg for foreground)",
    },
    Spec {
        name: "stop",
        names: &["stop"],
        about: "stop the session server gracefully",
    },
    Spec {
        name: "reload",
        names: &["reload"],
        about: "soft reload configuration and MCP/skills without dropping connections",
    },
    Spec {
        name: "restart",
        names: &["restart"],
        about: "restart or replace the session server",
    },
    Spec {
        name: "status",
        names: &["status"],
        about: "show session server status and active sessions",
    },
    Spec {
        name: "token",
        names: &["token"],
        about: "print the session server bearer token",
    },
];

const COMMANDS: &[Spec] = &[
    Spec {
        name: "thread",
        names: &["thread", "threads"],
        about: "browse, resume, or manage threads (ls, rm)",
    },
    Spec {
        name: "status",
        names: &["status"],
        about: "show application health, providers, and workflows",
    },
    Spec {
        name: "server",
        names: &["server"],
        about: "manage the session server (start, stop, reload, restart, status, token)",
    },
    Spec {
        name: "completions",
        names: &["completions"],
        about: "print a shell completion script",
    },
    Spec {
        name: "help",
        names: &["help"],
        about: "print help for a command",
    },
];

/// All accepted top-level spellings → canonical command, for help topics
/// and "did you mean" suggestions.
fn command_index() -> BTreeMap<&'static str, &'static str> {
    let mut index = BTreeMap::new();
    for spec in COMMANDS {
        for name in spec.names {
            index.insert(*name, spec.name);
        }
    }
    index
}

/// Resolve a word against a spec list by canonical name or alias.
fn resolve<'a>(word: &str, specs: &'a [Spec]) -> Option<&'a Spec> {
    specs.iter().find(|s| s.names.contains(&word))
}

// Flag parsing

/// A flag misuse, rendered as `--flag: message`.
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

/// Split `--flag=value` into `("--flag", Some("value"))`; a bare flag is
/// `("--flag", None)`; a positional word passes through unchanged.
fn split_flag(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value)),
        _ => (arg, None),
    }
}

/// `--flag value` / `--flag=value`: take the inline value or pull the
/// next token.
fn flag_value<'a, I: Iterator<Item = &'a String>>(
    flag: &str,
    inline: Option<&str>,
    iter: &mut I,
) -> Result<String, FlagError> {
    if let Some(v) = inline {
        return Ok(v.to_string());
    }
    iter.next()
        .cloned()
        .ok_or_else(|| FlagError::new(flag, "requires a value"))
}

fn parse_u16(flag: &str, value: &str) -> Result<u16, FlagError> {
    value
        .parse()
        .map_err(|_| FlagError::new(flag, format!("'{value}' is not a port number (0-65535)")))
}

fn parse_u64(flag: &str, value: &str) -> Result<u64, FlagError> {
    value
        .parse()
        .map_err(|_| FlagError::new(flag, format!("'{value}' is not a number")))
}

/// The `nuo server start` flags. Detachment is the canonical posture
/// (ADR-0078); `--fg` is the sole explicit foreground declaration.
#[derive(Default)]
struct ServerStartFlags {
    foreground: bool,
    port: Option<u16>,
    public: bool,
    no_local_auth: bool,
    idle_exit_minutes: Option<u64>,
    shutdown_grace_secs: Option<u64>,
    zero_tcp: bool,
}

fn parse_server_start_flags(args: &[String]) -> Result<ServerStartFlags, FlagError> {
    let mut flags = ServerStartFlags::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (name, inline) = split_flag(arg);
        match name {
            "--fg" | "--foreground" => flags.foreground = true,
            // ADR-0078: legacy no-op / superseded spellings are refused with
            // guidance — detachment IS the default posture, and `--zero-tcp`
            // is the canonical spelling for client-driven mode (ADR-0045).
            "--detach" | "--bg" | "-d" => {
                return Err(FlagError::new(
                    name,
                    "was removed — `nuo server start` already runs detached; use `--fg` for foreground",
                ));
            }
            "--client-driven" => {
                return Err(FlagError::new(
                    name,
                    "was renamed — use `--zero-tcp` (canonical spelling per ADR-0045)",
                ));
            }
            "--zero-tcp" => flags.zero_tcp = true,
            "--port" => {
                flags.port = Some(parse_u16(
                    "--port",
                    &flag_value("--port", inline, &mut iter)?,
                )?);
            }
            "--public" => flags.public = true,
            "--no-local-auth" => flags.no_local_auth = true,
            "--idle-exit" => {
                flags.idle_exit_minutes = Some(parse_u64(
                    "--idle-exit",
                    &flag_value("--idle-exit", inline, &mut iter)?,
                )?);
            }
            "--grace" => {
                flags.shutdown_grace_secs = Some(parse_u64(
                    "--grace",
                    &flag_value("--grace", inline, &mut iter)?,
                )?);
            }
            other => return Err(FlagError::new(other, "not a recognized flag here")),
        }
    }
    Ok(flags)
}

/// The `--watch/--json/--all` flags shared by the table-shaped commands.
#[derive(Default)]
struct TableFlags {
    watch: bool,
    json: bool,
    include_idle: bool,
    diagnostic: bool,
}

fn parse_table_flags(args: &[String], diagnostic: bool) -> Result<TableFlags, FlagError> {
    let mut flags = TableFlags {
        diagnostic,
        ..TableFlags::default()
    };
    for arg in args {
        let (name, inline) = split_flag(arg);
        if inline.is_some() {
            return Err(FlagError::new(name, "does not take a value"));
        }
        match name {
            "--watch" => flags.watch = true,
            "--json" => flags.json = true,
            "--all" => flags.include_idle = true,
            "--diagnostic" | "--diag" => flags.diagnostic = true,
            other => return Err(FlagError::new(other, "not a recognized flag here")),
        }
    }
    Ok(flags)
}

// parse

/// Parse the command line into [`CliArgs`]. Errors are short, actionable
/// strings; the caller owns the exit policy (GNU: stderr + exit 2).
pub fn parse(args: &[String]) -> Result<CliArgs, String> {
    let mut project: Option<PathBuf> = None;
    let mut json = false;
    let mut version = false;
    let mut rest: Vec<String> = Vec::new();

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
            "--json" | "-j" => json = true,
            "--version" | "-V" => version = true,
            "--single-instance" => {
                return Err(
                    "--single-instance was removed: the unified server owns every \
                     session, so a per-project instance lock no longer applies"
                        .into(),
                );
            }
            _ => rest.push(arg.clone()),
        }
    }

    let base = |mode| CliArgs {
        mode,
        project: project.clone(),
    };
    let ok = |mode| Ok(base(mode));
    let parse_interactive = |input_args: &[String]| -> Result<CliArgs, String> {
        let tui_args = nuo_tui::cli::parse(input_args)?;
        Ok(base(Mode::Interactive(tui_args)))
    };

    if version {
        return ok(Mode::Version);
    }

    // Help is position-sensitive: bare `--help`/`-h`/`help` is top-level;
    // `help <topic>` and `<command> --help` are the topic form.
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
            && rest[1..].iter().any(|a| a == "-h" || a == "--help")
        {
            return ok(Mode::Help(Some(first.to_string())));
        }
    }

    let Some(cmd) = rest.first().cloned() else {
        return parse_interactive(args);
    };

    if cmd == "dev" {
        let extra = &rest[1..];
        if extra.is_empty() || extra[0] == "storybook" || extra[0] == "ui" {
            return ok(Mode::Dev(DevAction::Storybook));
        } else if extra[0] == "export" {
            let out_dir = extra
                .get(1)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("target/storybook-snapshots"));
            return ok(Mode::Dev(DevAction::Export(out_dir)));
        } else {
            return Err("unknown dev command. Available: `nuo dev storybook`, `nuo dev ui`, or `nuo dev export [dir]`".into());
        }
    }

    if cmd == "run" {
        return Err(
            "'run' was removed in favor of '--headless' (ADR-0061).\n\n  tip: use 'nuo --headless <prompt>' or 'nuo -p <prompt>'".into(),
        );
    }

    if cmd == "config" {
        return Err(
            "'config' CLI commands were removed (ADR-0061).\n\n  tip: configuration is declarative in config.toml or managed interactively via /settings in TUI. Run 'nuo status' for environment overview.".into(),
        );
    }

    if cmd == "mcp" {
        return Err(
            "'mcp' CLI commands were removed (ADR-0061).\n\n  tip: MCP servers are configured declaratively in config.toml or managed via /mcp in TUI. Run 'nuo status' to check MCP health.".into(),
        );
    }

    if cmd == "skill" || cmd == "skills" {
        return Err(
            "'skill' CLI commands were removed (ADR-0061).\n\n  tip: skills are discovered from .nuo/skills or managed via /skills in TUI. Run 'nuo status' for overview.".into(),
        );
    }

    if cmd == "resume" {
        return Err(
            "'resume' was unified into 'nuo -t' / 'nuo thread' (ADR-0061).\n\n  tip: use 'nuo -t' to select a thread interactively, or 'nuo -t <id>' to resume by id.".into(),
        );
    }

    if cmd == "session" || cmd == "sessions" {
        return Err(
            "'session' was removed in favor of 'nuo thread' (ADR-0038, ADR-0048, ADR-0061).\n\n  tip: use 'nuo thread' to browse threads, or 'nuo thread ls'.".into(),
        );
    }

    if cmd == "-i"
        || cmd == "--interactive"
        || cmd == "-p"
        || cmd.starts_with("-p=")
        || cmd.starts_with('-')
    {
        return parse_interactive(args);
    }

    let extra: Vec<String> = rest[1..].to_vec();
    let unexpected = |arg: &str| Err(format!("unexpected argument '{arg}' found for 'nuo {cmd}'"));

    if resolve(&cmd, COMMANDS).is_none() && !cmd.starts_with('-') {
        let tip = suggest_command(&cmd);
        if let Some(s) = tip {
            return Err(format!(
                "unrecognized command '{cmd}'\n\n  tip: a similar command exists: '{s}'"
            ));
        } else {
            // Unrecognized word without command suggestion: treat as interactive session prompt
            return parse_interactive(args);
        }
    }

    // Reachable only when `cmd` matched a spec above (the match arm's
    // fallback errors out otherwise), so the resolve is infallible here.
    let Some(spec) = resolve(&cmd, COMMANDS) else {
        return Err(format!("unrecognized command '{cmd}'"));
    };
    let mode = match spec.name {
        "server" => {
            if extra.is_empty() {
                return Err("nuo server needs a subcommand: `nuo server start|stop|reload|restart|status|token`".into());
            }
            let sub = match resolve(&extra[0], SERVER_SUBS) {
                Some(sub) => sub,
                None => return unexpected(&extra[0]),
            };
            let sub_extra = &extra[1..];
            match sub.name {
                "start" => {
                    let flags = parse_server_start_flags(sub_extra).map_err(|e| e.0)?;
                    Mode::Server(ServerAction::Start {
                        foreground: flags.foreground,
                        port: flags.port,
                        public: flags.public,
                        no_local_auth: flags.no_local_auth,
                        idle_exit_minutes: flags.idle_exit_minutes,
                        shutdown_grace_secs: flags.shutdown_grace_secs,
                        zero_tcp: flags.zero_tcp,
                    })
                }
                "stop" => {
                    if !sub_extra.is_empty() {
                        return unexpected(&sub_extra[0]);
                    }
                    Mode::Server(ServerAction::Stop)
                }
                "reload" => {
                    if !sub_extra.is_empty() {
                        return unexpected(&sub_extra[0]);
                    }
                    Mode::Server(ServerAction::Reload)
                }
                "restart" => {
                    let mut force = false;
                    let mut remaining = Vec::new();
                    for arg in sub_extra {
                        if arg == "--force" {
                            force = true;
                        } else {
                            remaining.push(arg.clone());
                        }
                    }
                    let flags = parse_server_start_flags(&remaining).map_err(|e| e.0)?;
                    Mode::Server(ServerAction::Restart {
                        force,
                        port: flags.port,
                        public: flags.public,
                        no_local_auth: flags.no_local_auth,
                        idle_exit_minutes: flags.idle_exit_minutes,
                        shutdown_grace_secs: flags.shutdown_grace_secs,
                        zero_tcp: flags.zero_tcp,
                    })
                }
                "status" => {
                    let flags = parse_table_flags(sub_extra, false).map_err(|e| e.0)?;
                    Mode::Server(ServerAction::Status {
                        watch: flags.watch,
                        json: flags.json || json,
                        include_idle: flags.include_idle,
                        diagnostic: flags.diagnostic,
                    })
                }
                "token" => match sub_extra {
                    [] => Mode::Server(ServerAction::Token),
                    [bad, ..] => return unexpected(bad),
                },
                _ => unreachable!("server subcommands are closed"),
            }
        }
        "status" => {
            let flags = parse_table_flags(&extra, false).map_err(|e| e.0)?;
            Mode::Server(ServerAction::Status {
                watch: flags.watch,
                json: flags.json || json,
                include_idle: flags.include_idle,
                diagnostic: flags.diagnostic,
            })
        }
        "thread" => {
            let (sub, sub_extra) = if extra.is_empty() {
                (
                    &Spec {
                        name: "open",
                        names: &["open", "attach", "resume"],
                        about: "open interactive thread picker or attach to thread by id",
                    },
                    &[][..],
                )
            } else if let Some(sub) = resolve(&extra[0], THREAD_SUBS) {
                (sub, &extra[1..])
            } else if extra[0].starts_with('-') {
                (
                    &Spec {
                        name: "ls",
                        names: &["ls", "list"],
                        about: "list hosted threads",
                    },
                    &extra[..],
                )
            } else {
                (
                    &Spec {
                        name: "open",
                        names: &["open", "attach", "resume"],
                        about: "open interactive thread picker or attach to thread by id",
                    },
                    &extra[..],
                )
            };
            match sub.name {
                "open" => {
                    let id = match sub_extra {
                        [] => None,
                        [target] if !target.starts_with('-') => Some(target.clone()),
                        [bad, ..] => return unexpected(bad),
                    };
                    Mode::Thread(ThreadAction::Open(id))
                }
                "ls" => {
                    let mut watch = false;
                    let mut json_flag = false;
                    let mut all = false;
                    for arg in sub_extra {
                        match arg.as_str() {
                            "-w" | "--watch" => watch = true,
                            "-j" | "--json" => json_flag = true,
                            "-a" | "--all" => all = true,
                            bad => return unexpected(bad),
                        }
                    }
                    Mode::Thread(ThreadAction::List {
                        watch,
                        json: json_flag || json,
                        all,
                    })
                }
                "rm" => {
                    let args: &[String] = sub_extra;
                    match args {
                        [id] if !id.starts_with('-') => {
                            Mode::Thread(ThreadAction::Delete(id.clone()))
                        }
                        [] => return Err("thread rm requires a thread id".into()),
                        [bad, ..] => return unexpected(bad),
                    }
                }
                _ => unreachable!("thread subcommands are closed"),
            }
        }
        "completions" => match extra.as_slice() {
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

// Suggestions

/// clap-style "did you mean": an exact-prefix match first, then the
/// closest command within a small edit distance.
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
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            curr[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

// Help (generated from the spec tables)

/// Per-command flag tables rendered into help. Kept as data so help and
/// completion share them.
fn command_flags(cmd: &str) -> &'static [(&'static str, &'static str)] {
    match cmd {
        "status" => &[
            ("--watch", "keep streaming live updates"),
            ("--json", "emit one JSON frame per update"),
            ("--all", "include idle sessions"),
            ("--diagnostic", "report discovery/lock/socket/log health"),
        ],
        "thread" => &[
            (
                "open [id]",
                "open interactive thread picker or attach by id",
            ),
            ("ls", "list hosted threads"),
            ("rm <id>", "terminate a hosted thread by id"),
        ],
        _ => &[],
    }
}

/// The help text for a topic: `None` is the top-level help, a command
/// name its per-command text. Returns `None` for unknown topics.
pub fn help_text(topic: Option<&str>) -> Option<String> {
    let mut out = String::new();
    match topic {
        None => {
            out.push_str("nuo — terminal-first AI agent platform\n\n");
            out.push_str(
                "Usage: nuo [OPTIONS] [PROMPT]\n       nuo [OPTIONS] <COMMAND>\n\nCommands:\n",
            );
            let width = COMMANDS.iter().map(|s| s.name.len()).max().unwrap_or(0);
            for spec in COMMANDS {
                out.push_str(&format!("  {:<width$}  {}\n", spec.name, spec.about));
            }
            out.push_str("\nOptions:\n");
            out.push_str(
                "  -t, --thread [ID]      attach to existing thread (opens picker if ID omitted)\n",
            );
            out.push_str(
                "      --headless         execute prompt non-interactively and stream to stdout\n",
            );
            out.push_str("  -p, --prompt <TEXT>    headless prompt or initial TUI input\n");
            out.push_str("  -i, --interactive      force interactive TUI mode\n");
            out.push_str("  -r, --role <ROLE>      agent role (developer, ops, etc.)\n");
            out.push_str("      --resume           resume the most recent matching thread\n");
            out.push_str(
                "      --unattended       auto-approve permissions without confirmation\n",
            );
            out.push_str("      --no-confinement   disable workspace filesystem confinement\n");
            out.push_str("      --project <PATH>   operate on the project at <PATH>\n");
            out.push_str("      --remote <ADDR>    connect to a remote server (e.g. wss://tunnel.domain.com, :9800)\n");
            out.push_str("      --token <TOKEN>    authentication bearer token for remote server\n");
            out.push_str("  -h, --help             print help ('nuo help <command>' for more)\n");
            out.push_str("  -V, --version          print the version and exit\n");
            out.push_str("\nEnvironment:\n");
            out.push_str(
                "  NUO_HOME              instance root for isolated execution (<dir>/nuo)\n",
            );
            out.push_str(
                "  NUO_PORT              override default server TCP port (default: 9800)\n",
            );
            out.push_str(
                "  NUO_TOKEN             fallback bearer token for remote server authentication\n",
            );
        }
        Some(topic) => {
            let spec = resolve(topic, COMMANDS)?;
            out.push_str(&format!("nuo {} — {}\n\n", spec.name, spec.about));
            out.push_str(&format!("Usage: nuo {}", spec.name));
            if let Some(subs) = subs_of(spec.name) {
                out.push_str(" [COMMAND]\n\nCommands:\n");
                let width = subs.iter().map(|s| s.name.len()).max().unwrap_or(0);
                for sub in subs {
                    out.push_str(&format!("  {:<width$}  {}\n", sub.name, sub.about));
                }
            } else {
                out.push('\n');
            }
            let flags = command_flags(spec.name);
            if !flags.is_empty() {
                out.push_str("\nOptions:\n");
                let width = flags.iter().map(|(f, _)| f.len()).max().unwrap_or(0);
                for (flag, about) in flags {
                    out.push_str(&format!("  {:<width$}  {about}\n", flag));
                }
            }
            if spec.name == "server" {
                out.push_str(
                    "\nThe server hosts every session across every project. `nuo server start` runs it\n",
                );
                out.push_str("detached by default (--fg for systemd/tmux-style supervision);\n");
                out.push_str(
                    "`nuo server stop` drains within the server's --grace budget before escalating;\n",
                );
                out.push_str("`nuo status` observes without ever spawning one.\n");
                out.push_str("\nNUO_HOME points the whole instance (config, data, server files,\n");
                out.push_str("port default via NUO_PORT) at an isolated root — the dev/test\n");
                out.push_str("sandbox shape. See `nuo --help` for the config paths.\n");
            }
        }
    }
    Some(out)
}

fn subs_of(cmd: &str) -> Option<&'static [Spec]> {
    match cmd {
        "server" => Some(SERVER_SUBS),
        "thread" => Some(THREAD_SUBS),
        _ => None,
    }
}

// Shell completions (generated from the same tables)

/// The static completion script for a shell.
pub fn completion_script(shell: Shell) -> String {
    match shell {
        Shell::Bash => bash_completion(),
        Shell::Zsh => zsh_completion(),
        Shell::Fish => fish_completion(),
    }
}

fn subs_and_flags(cmd: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let subs: Vec<&'static str> = subs_of(cmd)
        .map(|s| s.iter().map(|x| x.name).collect())
        .unwrap_or_default();
    let flags: Vec<&'static str> = command_flags(cmd)
        .iter()
        .filter_map(|(flag, _)| {
            // Strip the leading subcommand and the value placeholder; keep
            // the flag itself.
            let flag = flag.split(' ').next().unwrap_or(flag);
            flag.starts_with("--").then_some(flag)
        })
        .collect();
    (subs, flags)
}

fn bash_completion() -> String {
    let cmds: Vec<&str> = COMMANDS.iter().map(|s| s.name).collect();
    let mut cases = String::new();
    for spec in COMMANDS {
        let (subs, flags) = subs_and_flags(spec.name);
        let mut words = subs;
        words.extend(flags.iter().copied());
        words.push("--help");
        if !words.is_empty() {
            cases.push_str(&format!(
                "        {})\n            COMPREPLY=($(compgen -W \"{}\" -- \"$cur\")) ;;\n",
                spec.name,
                words.join(" ")
            ));
        }
    }
    format!(
        "# bash completion for nuo — eval \"$(nuo completions bash)\"\n\
         _nuo() {{\n\
         \x20   local cur cmd\n\
         \x20   cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n\
         \x20   cmd=\"${{COMP_WORDS[1]}}\"\n\
         \x20   if [[ $COMP_CWORD -eq 1 ]]; then\n\
         \x20       COMPREPLY=($(compgen -W \"{} --project --json -j --help --version\" -- \"$cur\"))\n\
         \x20       return 0\n\
         \x20   fi\n\
         \x20   case \"$cmd\" in\n{}\n\
         \x20   esac\n\
         }}\n\
         complete -F _nuo nuo\n",
        cmds.join(" "),
        cases
    )
}

fn zsh_completion() -> String {
    let mut cmds = String::new();
    for spec in COMMANDS {
        cmds.push_str(&format!("        '{}:{}'\n", spec.name, spec.about));
    }
    let mut cases = String::new();
    for spec in COMMANDS {
        let (subs, flags) = subs_and_flags(spec.name);
        if !subs.is_empty() {
            cases.push_str(&format!(
                "        {})\n            _describe 'subcommand' '({})' ;;\n",
                spec.name,
                subs.iter()
                    .map(|s| format!("{}:{}", s, subs_about(spec.name, s)))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        if !flags.is_empty() {
            cases.push_str(&format!(
                "        {})\n            _arguments '{}'\n            ;;\n",
                spec.name,
                flags.join("' '")
            ));
        }
    }
    format!(
        "#compdef nuo\n\
         # zsh completion for nuo — save as `_nuo` on $fpath\n\
         _nuo() {{\n\
         \x20   local -a cmds\n\
         \x20   cmds=(\n{}\n\
         \x20   )\n\
         \x20   if (( CURRENT == 2 )); then\n\
         \x20       _describe 'command' cmds\n\
         \x20       return\n\
         \x20   fi\n\
         \x20   case \"$words[2]\" in\n{}\
         \x20   esac\n\
         }}\n\
         _nuo \"$@\"\n",
        cmds, cases
    )
}

fn subs_about(cmd: &str, sub: &str) -> &'static str {
    subs_of(cmd)
        .and_then(|subs| subs.iter().find(|s| s.name == sub))
        .map(|s| s.about)
        .unwrap_or("")
}

fn fish_completion() -> String {
    let cmds: Vec<&str> = COMMANDS.iter().map(|s| s.name).collect();
    let mut out = format!(
        "# fish completion for nuo — save to ~/.config/fish/completions/nuo.fish\n\
         set -l cmds {}\n\
         complete -c nuo -n '__fish_use_subcommand' -f\n\
         complete -c nuo -n '__fish_use_subcommand' -a \"$cmds\"\n",
        cmds.join(" ")
    );
    for spec in COMMANDS {
        let (subs, flags) = subs_and_flags(spec.name);
        for sub in subs {
            out.push_str(&format!(
                "complete -c nuo -n '__fish_seen_subcommand_from {}' -f -a '{}' -d '{}'\n",
                spec.name,
                sub,
                subs_about(spec.name, sub)
            ));
        }
        for flag in flags {
            out.push_str(&format!(
                "complete -c nuo -n '__fish_seen_subcommand_from {}' -l '{}' -d 'flag'\n",
                spec.name,
                flag.trim_start_matches("--")
            ));
        }
    }
    out
}

// Tests

#[cfg(test)]
mod surface_tests {
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
    fn bare_invocation_starts_interactive_tui() {
        assert!(matches!(parse(&[]).unwrap().mode, Mode::Interactive(_)));
    }

    #[test]
    fn server_subcommands_parse() {
        let parsed_default = parse(&["server", "start"]).unwrap();
        assert!(matches!(
            parsed_default.mode,
            Mode::Server(ServerAction::Start {
                foreground: false,
                zero_tcp: false,
                ..
            })
        ));

        let parsed_fg = parse(&["server", "start", "--fg"]).unwrap();
        assert!(matches!(
            parsed_fg.mode,
            Mode::Server(ServerAction::Start {
                foreground: true,
                ..
            })
        ));

        // ADR-0078: no-op legacy detachment and superseded spellings are
        // removed with guidance.
        for flag in ["--detach", "-d", "--bg"] {
            let err = parse(&["server", "start", flag]).unwrap_err();
            assert!(err.contains("already runs detached"), "{err}");
            assert!(err.contains("--fg"), "{err}");
        }
        let err = parse(&["server", "start", "--client-driven"]).unwrap_err();
        assert!(err.contains("--zero-tcp"), "{err}");
        // restart shares the same flag space.
        let err = parse(&["server", "restart", "--detach"]).unwrap_err();
        assert!(err.contains("already runs detached"), "{err}");
        let err = parse(&["server", "restart", "--client-driven"]).unwrap_err();
        assert!(err.contains("--zero-tcp"), "{err}");

        let parsed_zero = parse(&["server", "start", "--zero-tcp"]).unwrap();
        assert!(matches!(
            parsed_zero.mode,
            Mode::Server(ServerAction::Start { zero_tcp: true, .. })
        ));

        assert!(matches!(
            parse(&["server", "stop"]).unwrap().mode,
            Mode::Server(ServerAction::Stop)
        ));
        assert!(matches!(
            parse(&["server", "reload"]).unwrap().mode,
            Mode::Server(ServerAction::Reload)
        ));
        assert!(matches!(
            parse(&["server", "restart"]).unwrap().mode,
            Mode::Server(ServerAction::Restart { .. })
        ));
        assert!(matches!(
            parse(&["server", "status"]).unwrap().mode,
            Mode::Server(ServerAction::Status { .. })
        ));
        assert!(matches!(
            parse(&["server", "token"]).unwrap().mode,
            Mode::Server(ServerAction::Token)
        ));
    }

    #[test]
    fn top_level_status_parses() {
        assert!(matches!(
            parse(&["status"]).unwrap().mode,
            Mode::Server(ServerAction::Status { .. })
        ));
    }

    #[test]
    fn legacy_and_scene_commands_are_eradicated() {
        // serve, start, stop, doctor, auth, quota are unrecognized as commands
        assert!(parse(&["serve"]).is_err());
        assert!(parse(&["doctor"]).is_err());
        assert!(parse(&["auth"]).is_err());
        assert!(parse(&["quota"]).is_err());
        assert!(parse(&["context"]).is_err());
    }

    #[test]
    fn run_command_is_retired_in_favor_of_headless() {
        let err = parse(&["run", "hello"]).unwrap_err();
        assert!(err.contains("'run' was removed in favor of '--headless'"));
    }

    #[test]
    fn pseudo_crud_commands_are_retired_with_guidance() {
        let mcp_err = parse(&["mcp", "ls"]).unwrap_err();
        assert!(mcp_err.contains("'mcp' CLI commands were removed (ADR-0061)"));

        let config_err = parse(&["config", "ls"]).unwrap_err();
        assert!(config_err.contains("'config' CLI commands were removed (ADR-0061)"));

        let skill_err = parse(&["skill", "ls"]).unwrap_err();
        assert!(skill_err.contains("'skill' CLI commands were removed (ADR-0061)"));

        let resume_err = parse(&["resume"]).unwrap_err();
        assert!(resume_err.contains("'resume' was unified into 'nuo -t' / 'nuo thread'"));
    }

    #[test]
    fn thread_verbs_parse_and_legacy_session_is_retired() {
        assert!(matches!(
            parse(&["thread", "rm", "thr-0123"]).unwrap().mode,
            Mode::Thread(ThreadAction::Delete(ref id)) if id == "thr-0123"
        ));
        assert!(matches!(
            parse(&["thread", "ls"]).unwrap().mode,
            Mode::Thread(ThreadAction::List {
                watch: false,
                json: false,
                all: false
            })
        ));
        // Bare `nuo thread` defaults to interactive Thread Picker
        assert!(matches!(
            parse(&["thread"]).unwrap().mode,
            Mode::Thread(ThreadAction::Open(None))
        ));
        assert!(matches!(
            parse(&["thread", "open"]).unwrap().mode,
            Mode::Thread(ThreadAction::Open(None))
        ));
        assert!(matches!(
            parse(&["thread", "open", "thr-0123"]).unwrap().mode,
            Mode::Thread(ThreadAction::Open(Some(ref id))) if id == "thr-0123"
        ));
        assert!(matches!(
            parse(&["thread", "list", "--json", "--watch"])
                .unwrap()
                .mode,
            Mode::Thread(ThreadAction::List {
                watch: true,
                json: true,
                all: false
            })
        ));
        let session_err = parse(&["session", "rm", "thr-0123"]).unwrap_err();
        assert!(session_err.contains("'session' was removed in favor of 'nuo thread'"));
    }

    #[test]
    fn headless_and_thread_flags_route_interactively() {
        let parsed = parse(&["--headless", "-p", "fix it"]).unwrap();
        if let Mode::Interactive(args) = parsed.mode {
            assert!(args.headless);
            assert_eq!(args.prompt.as_deref(), Some("fix it"));
        } else {
            panic!("expected Interactive mode");
        }

        let parsed_thread = parse(&["-t"]).unwrap();
        if let Mode::Interactive(args) = parsed_thread.mode {
            assert_eq!(args.thread, Some(None));
        } else {
            panic!("expected Interactive mode");
        }
    }
}
