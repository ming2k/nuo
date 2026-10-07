//! The `nuo` command line — parsed where it belongs (ADR-0116).
//!
//! For most of the project's life this lived in `nuo-server::startup`
//! (`parse_args`), which put a frontend concern inside the session-runtime
//! library and let two flag tables drift independently. ADR-0116 fixes both:
//!
//! - **One noun per resource, one verb per action.** The daemon is managed
//!   by the top-level `nuo start|stop|status|token` verbs; sessions by
//!   `nuo session rm` (listing is `status`). Interactive
//!   run/attach/dashboard commands are top-level `nuo` verbs. The former
//!   spellings (`serve`, the `daemon` noun, `resume`, `exec`) are removed
//!   outright: no alias, no teaching error — an unknown word is an
//!   unrecognized command.
//! - **The parser is a table, not a hand-rolled ladder.** One spec drives
//!   parsing, help, error messages, and shell completions, so a flag
//!   cannot exist in one place and not the other (the `--expose`/
//!   `--public` usage drift this replaces).
//!
//! This module decides *what was asked*; the dispatch in `main.rs` decides
//! *what happens*.

use std::collections::BTreeMap;
use std::path::PathBuf;

// The parsed command line

/// What the user asked the binary to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Interactive TUI session or headless run (`nuox` unified engine per ADR-0005).
    Interactive(Vec<String>),
    Session(SessionAction),
    /// Session server commands and lifecycle operations.
    Server(ServerAction),
    Config(ConfigAction),
    Auth(AuthAction),
    /// `nuo mcp ls` — list configured MCP servers.
    Mcp(McpAction),
    /// `nuo skill ls` — list discovered skills.
    Skill(SkillAction),
    /// `nuo context migrate` — offline legacy→canonical conversion (ADR-0280).
    Context(ContextAction),
    Doctor,
    /// `nuo completions <shell>`.
    Completions(Shell),
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h` / `help [topic]`.
    Help(Option<String>),
}

/// `nuo mcp …` (ADR-0252: read-only inspection; mutations are file-authored)
#[derive(Debug, Clone, PartialEq)]
pub enum McpAction {
    /// `nuo mcp ls` — list configured MCP servers.
    List,
    /// `nuo mcp get <name>` — print one server's effective TOML entry.
    Get { name: String },
    /// `nuo mcp probe <name>` — connect once, list the advertised tools.
    Probe { name: String },
}

/// `nuo skill …`
#[derive(Debug, Clone, PartialEq)]
pub enum SkillAction {
    /// `nuo skill ls` — list discovered skills.
    List,
    /// `nuo skill show <name>` — print one skill's full markdown instructions.
    Show { name: String },
    /// `nuo skill info <name>` — print one skill's diagnostics and metadata.
    Info { name: String },
    /// `nuo skill init <name> [--user]` — scaffold a new skill folder with standard templates.
    Init { name: String, user: bool },
}

/// `nuo session …`
#[derive(Debug, Clone, PartialEq)]
pub enum SessionAction {
    /// `nuo session rm <id>` — terminate a hosted session. Listing is
    /// `nuo status`: the session table is the daemon's view.
    Delete(String),
}

/// `nuo` server actions (server, serve, start, stop, restart, status, token)
#[derive(Debug, Clone, PartialEq)]
pub enum ServerAction {
    /// `nuo server start` / `nuo start` / `nuo serve` — start the session server.
    Start {
        /// `--detach`: run in background.
        /// By default, runs in the foreground (ADR-0029).
        foreground: bool,
        port: Option<u16>,
        public: bool,
        no_local_auth: bool,
        idle_exit_minutes: Option<u64>,
        shutdown_grace_secs: Option<u64>,
        client_driven: bool,
    },
    /// `nuo server stop` / `nuo stop` — graceful, budget-aware drain.
    Stop,
    /// `nuo server reload` / `nuo reload` — soft reload (ADR-0034 Level 1):
    /// re-read configuration and re-sync MCP + skills without dropping
    /// connections.
    Reload,
    /// `nuo server restart` / `nuo restart` — restart or replace the server instance.
    Restart {
        force: bool,
        port: Option<u16>,
        public: bool,
        no_local_auth: bool,
        idle_exit_minutes: Option<u64>,
        shutdown_grace_secs: Option<u64>,
        client_driven: bool,
    },
    /// `nuo server token` / `nuo token` — print the local server's bearer token.
    Token,
    /// `nuo server status` / `nuo status` — the server's session table and endpoints.
    Status {
        watch: bool,
        json: bool,
        include_idle: bool,
        diagnostic: bool,
    },
}

/// Backward-compatible alias for [`ServerAction`].
#[allow(dead_code)]
pub type DaemonAction = ServerAction;

/// `nuo config …`
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigAction {
    List,
    Path,
    Get(String),
    Set {
        key: String,
        value: String,
    },
    /// `nuo config check` — validate configuration against domain schemas.
    Check,
    /// `nuo config migrate` — perform clean-break migration from legacy config.toml
    /// into domain-separated server.toml, client.toml, terminal.toml, agent.toml (ADR-0031).
    Migrate,
}

/// `nuo auth …`
#[derive(Debug, Clone, PartialEq)]
pub enum AuthAction {
    List,
    Show(String),
    Set { provider: String, key: String },
}

/// `nuo context …` (ADR-0280 §4): the offline migration surface.
#[derive(Debug, Clone, PartialEq)]
pub enum ContextAction {
    /// `nuo context migrate --legacy <db> --target <db>` — convert a legacy
    /// database into a canonical one, offline, never in the runtime path.
    Migrate {
        /// The legacy database to read.
        legacy: String,
        /// The canonical database to write.
        target: String,
    },
    /// `nuo context verify --db <db> [--json]` — emit a machine-readable
    /// integrity report for a canonical database.
    Verify {
        /// The canonical database to verify.
        db: String,
        /// Emit JSON instead of human-readable text.
        json: bool,
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

const SESSION_SUBS: &[Spec] = &[
    // The listing is `nuo status`, not a session subcommand: the
    // session table is the daemon's view of what it hosts (ADR-0116's
    // one-noun-per-resource — `session ls` duplicated `nuo status`
    // verbatim).
    Spec {
        name: "rm",
        names: &["rm", "delete"],
        about: "terminate a hosted session by id",
    },
];

const CONFIG_SUBS: &[Spec] = &[
    Spec {
        name: "list",
        names: &["list", "show"],
        about: "show current configuration",
    },
    Spec {
        name: "get",
        names: &["get"],
        about: "get a configuration value",
    },
    Spec {
        name: "set",
        names: &["set"],
        about: "set a configuration value",
    },
    Spec {
        name: "path",
        names: &["path"],
        about: "print the configuration file path",
    },
    Spec {
        name: "check",
        names: &["check"],
        about: "validate config.toml: syntax errors, typo'd keys, dead legacy keys",
    },
];

const AUTH_SUBS: &[Spec] = &[
    Spec {
        name: "list",
        names: &["list", "status"],
        about: "list configured providers and auth status",
    },
    Spec {
        name: "show",
        names: &["show"],
        about: "show one provider's credential status",
    },
    Spec {
        name: "set",
        names: &["set"],
        about: "set a provider API key",
    },
];

const MCP_SUBS: &[Spec] = &[
    Spec {
        name: "ls",
        names: &["ls", "list"],
        about: "list configured MCP servers",
    },
    Spec {
        name: "get",
        names: &["get"],
        about: "print one server's config entry",
    },
    Spec {
        name: "probe",
        names: &["probe"],
        about: "connect to a server once and list its tools",
    },
];

const SERVER_SUBS: &[Spec] = &[
    Spec {
        name: "start",
        names: &["start"],
        about: "start the session server (foreground by default; --detach for background)",
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

const SKILL_SUBS: &[Spec] = &[Spec {
    name: "ls",
    names: &["ls", "list"],
    about: "list discovered skills",
}];

const COMMANDS: &[Spec] = &[
    Spec {
        name: "server",
        names: &["server"],
        about: "manage the session server (start, stop, restart, status, token)",
    },
    Spec {
        name: "serve",
        names: &["serve"],
        about: "run the session server in the foreground",
    },
    Spec {
        name: "run",
        names: &["run"],
        about: "execute a headless turn (-p / run) streaming to stdout",
    },
    Spec {
        name: "attach",
        names: &["attach"],
        about: "join an existing or hosted session in the interactive TUI",
    },
    Spec {
        name: "dashboard",
        names: &["dashboard"],
        about: "open the full-screen interactive session dashboard",
    },
    Spec {
        name: "settings",
        names: &["settings"],
        about: "open the interactive settings overlay",
    },
    Spec {
        name: "start",
        names: &["start"],
        about: "start the server (runs in foreground by default; --detach runs in background)",
    },
    Spec {
        name: "stop",
        names: &["stop"],
        about: "stop the server gracefully",
    },
    Spec {
        name: "reload",
        names: &["reload"],
        about: "soft reload configuration and MCP/skills without dropping connections",
    },
    Spec {
        name: "restart",
        names: &["restart"],
        about: "restart the server instance",
    },
    Spec {
        name: "status",
        names: &["status"],
        about: "show the server's sessions and endpoints",
    },
    Spec {
        name: "token",
        names: &["token"],
        about: "print the local server bearer token",
    },
    Spec {
        name: "session",
        names: &["session"],
        about: "manage sessions (rm; listing is `status`)",
    },
    Spec {
        name: "config",
        names: &["config"],
        about: "inspect or modify configuration",
    },
    Spec {
        name: "context",
        names: &["context"],
        about: "context lifecycle: offline legacy-to-canonical migration",
    },
    Spec {
        name: "auth",
        names: &["auth"],
        about: "manage provider credentials and API keys",
    },
    Spec {
        name: "mcp",
        names: &["mcp"],
        about: "inspect MCP servers (ls, get, probe); configured declaratively in TOML",
    },
    Spec {
        name: "skill",
        names: &["skill", "skills"],
        about: "manage skills (ls)",
    },
    Spec {
        name: "doctor",
        names: &["doctor"],
        about: "verify stored session integrity",
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

/// The `nuo start` flags (one table — the `serve` duplication is gone).
struct DaemonStartFlags {
    foreground: bool,
    port: Option<u16>,
    public: bool,
    no_local_auth: bool,
    idle_exit_minutes: Option<u64>,
    shutdown_grace_secs: Option<u64>,
    client_driven: bool,
}

impl Default for DaemonStartFlags {
    fn default() -> Self {
        Self {
            foreground: true,
            port: None,
            public: false,
            no_local_auth: false,
            idle_exit_minutes: None,
            shutdown_grace_secs: None,
            client_driven: false,
        }
    }
}

fn parse_daemon_start_flags(args: &[String]) -> Result<DaemonStartFlags, FlagError> {
    let mut flags = DaemonStartFlags::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (name, inline) = split_flag(arg);
        match name {
            "--fg" | "--foreground" => flags.foreground = true,
            "--detach" | "-d" | "--bg" => flags.foreground = false,
            "--client-driven" => flags.client_driven = true,
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
                    "--single-instance was removed: the unified daemon owns every \
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
        return ok(Mode::Interactive(args.to_vec()));
    };

    if cmd == "daemon" {
        return Err(
            "the 'daemon' noun was removed: use 'nuo start|stop|status|token' or 'nuo serve'"
                .to_string(),
        );
    }

    if cmd == "-i"
        || cmd == "--interactive"
        || cmd == "-p"
        || cmd.starts_with("-p=")
        || cmd == "run"
        || cmd == "attach"
        || cmd == "dashboard"
        || cmd == "settings"
    {
        return ok(Mode::Interactive(args.to_vec()));
    }

    if cmd == "serve" {
        let flags = parse_daemon_start_flags(&rest[1..]).map_err(|e| e.0)?;
        return ok(Mode::Server(ServerAction::Start {
            foreground: true,
            port: flags.port,
            public: flags.public,
            no_local_auth: flags.no_local_auth,
            idle_exit_minutes: flags.idle_exit_minutes,
            shutdown_grace_secs: flags.shutdown_grace_secs,
            client_driven: flags.client_driven,
        }));
    }

    if cmd.starts_with('-') {
        let flags = parse_daemon_start_flags(&rest).map_err(|e| e.0)?;
        return ok(Mode::Server(ServerAction::Start {
            foreground: flags.foreground,
            port: flags.port,
            public: flags.public,
            no_local_auth: flags.no_local_auth,
            idle_exit_minutes: flags.idle_exit_minutes,
            shutdown_grace_secs: flags.shutdown_grace_secs,
            client_driven: flags.client_driven,
        }));
    }

    let extra: Vec<String> = rest[1..].to_vec();
    let unexpected = |arg: &str| {
        Err(format!(
            "unexpected argument '{arg}' found for 'nuo {cmd}'"
        ))
    };

    if resolve(&cmd, COMMANDS).is_none() && !cmd.starts_with('-') {
        let tip = suggest_command(&cmd);
        if let Some(s) = tip {
            return Err(format!("unrecognized command '{cmd}'\n\n  tip: a similar command exists: '{s}'"));
        } else {
            // Unrecognized word without command suggestion: treat as interactive session prompt
            return ok(Mode::Interactive(args.to_vec()));
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
                return Err("nuo server needs a subcommand: `nuo server start|stop|restart|status|token`".into());
            }
            let sub = match resolve(&extra[0], SERVER_SUBS) {
                Some(sub) => sub,
                None => return unexpected(&extra[0]),
            };
            let sub_extra = &extra[1..];
            match sub.name {
                "start" => {
                    let flags = parse_daemon_start_flags(sub_extra).map_err(|e| e.0)?;
                    Mode::Server(ServerAction::Start {
                        foreground: flags.foreground,
                        port: flags.port,
                        public: flags.public,
                        no_local_auth: flags.no_local_auth,
                        idle_exit_minutes: flags.idle_exit_minutes,
                        shutdown_grace_secs: flags.shutdown_grace_secs,
                        client_driven: flags.client_driven,
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
                    let flags = parse_daemon_start_flags(&remaining).map_err(|e| e.0)?;
                    Mode::Server(ServerAction::Restart {
                        force,
                        port: flags.port,
                        public: flags.public,
                        no_local_auth: flags.no_local_auth,
                        idle_exit_minutes: flags.idle_exit_minutes,
                        shutdown_grace_secs: flags.shutdown_grace_secs,
                        client_driven: flags.client_driven,
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
        "serve" => {
            let flags = parse_daemon_start_flags(&extra).map_err(|e| e.0)?;
            Mode::Server(ServerAction::Start {
                foreground: true,
                port: flags.port,
                public: flags.public,
                no_local_auth: flags.no_local_auth,
                idle_exit_minutes: flags.idle_exit_minutes,
                shutdown_grace_secs: flags.shutdown_grace_secs,
                client_driven: flags.client_driven,
            })
        }
        "run" | "attach" | "dashboard" | "settings" => {
            Mode::Interactive(args.to_vec())
        }
        "start" => {
            let flags = parse_daemon_start_flags(&extra).map_err(|e| e.0)?;
            Mode::Server(ServerAction::Start {
                foreground: flags.foreground,
                port: flags.port,
                public: flags.public,
                no_local_auth: flags.no_local_auth,
                idle_exit_minutes: flags.idle_exit_minutes,
                shutdown_grace_secs: flags.shutdown_grace_secs,
                client_driven: flags.client_driven,
            })
        }
        "stop" => {
            let args: &[String] = &extra;
            match args {
                [] => Mode::Server(ServerAction::Stop),
                [bad, ..] => return unexpected(bad),
            }
        }
        "reload" => {
            let args: &[String] = &extra;
            match args {
                [] => Mode::Server(ServerAction::Reload),
                [bad, ..] => return unexpected(bad),
            }
        }
        "restart" => {
            let mut force = false;
            let mut remaining = Vec::new();
            for arg in &extra {
                if arg == "--force" {
                    force = true;
                } else {
                    remaining.push(arg.clone());
                }
            }
            let flags = parse_daemon_start_flags(&remaining).map_err(|e| e.0)?;
            Mode::Server(ServerAction::Restart {
                force,
                port: flags.port,
                public: flags.public,
                no_local_auth: flags.no_local_auth,
                idle_exit_minutes: flags.idle_exit_minutes,
                shutdown_grace_secs: flags.shutdown_grace_secs,
                client_driven: flags.client_driven,
            })
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
        "token" => match extra.as_slice() {
            [] => Mode::Server(ServerAction::Token),
            [bad, ..] => return unexpected(bad),
        },
        "session" => {
            if extra.is_empty() {
                return Err("nuo session needs a subcommand: `nuo session rm <id>` \
                     (to list sessions, use `nuo status`)"
                    .into());
            }
            let sub = match resolve(&extra[0], SESSION_SUBS) {
                Some(sub) => sub,
                None => return unexpected(&extra[0]),
            };
            let sub_extra = &extra[1..];
            match sub.name {
                "rm" => {
                    let args: &[String] = sub_extra;
                    match args {
                        [id] if !id.starts_with('-') => {
                            Mode::Session(SessionAction::Delete(id.clone()))
                        }
                        [] => return Err("session rm requires a session id".into()),
                        [bad, ..] => return unexpected(bad),
                    }
                }
                _ => unreachable!("session subcommands are closed"),
            }
        }
        "config" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] | ["list"] | ["show"] => Mode::Config(ConfigAction::List),
                ["path"] => Mode::Config(ConfigAction::Path),
                ["check"] => Mode::Config(ConfigAction::Check),
                ["migrate"] => Mode::Config(ConfigAction::Migrate),
                ["get", key] => Mode::Config(ConfigAction::Get((*key).to_string())),
                ["set", key, value] => Mode::Config(ConfigAction::Set {
                    key: (*key).to_string(),
                    value: (*value).to_string(),
                }),
                ["get"] => return Err("config get requires a key name".into()),
                ["set"] | ["set", _] => {
                    return Err("config set requires <key> and <value>".into());
                }
                [bad, ..] => return unexpected(bad),
            }
        }
        "context" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                ["migrate", "--legacy", legacy, "--target", target] => {
                    Mode::Context(ContextAction::Migrate {
                        legacy: (*legacy).to_string(),
                        target: (*target).to_string(),
                    })
                }
                ["migrate", ..] => {
                    return Err("context migrate requires --legacy <db> and --target <db>".into());
                }
                ["verify", "--db", db] => Mode::Context(ContextAction::Verify {
                    db: (*db).to_string(),
                    json,
                }),
                ["verify", ..] => {
                    return Err("context verify requires --db <db> [--json]".into());
                }
                [bad, ..] => return unexpected(bad),
                [] => return Err("context requires a subcommand (migrate|verify)".into()),
            }
        }
        "auth" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] | ["list"] | ["status"] => Mode::Auth(AuthAction::List),
                ["show", provider] => Mode::Auth(AuthAction::Show((*provider).to_string())),
                ["set", provider, key] => Mode::Auth(AuthAction::Set {
                    provider: (*provider).to_string(),
                    key: (*key).to_string(),
                }),
                ["show"] => return Err("auth show requires a provider name".into()),
                ["set"] | ["set", _] => {
                    return Err("auth set requires <provider> and <key>".into());
                }
                [bad, ..] => return unexpected(bad),
            }
        }
        "mcp" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                // A bare `nuo mcp` teaches the subcommand rather than
                // silently running the only one (ADR-0119's noun-verb
                // shape; `config`/`auth` default to `list` because they
                // have several — `mcp` now does too, but the lesson
                // stays worth the keystroke).
                [] => return Err("nuo mcp needs a subcommand: `nuo mcp ls`, `nuo mcp get <name>`, `nuo mcp probe <name>`".into()),
                ["ls"] | ["list"] => Mode::Mcp(McpAction::List),
                ["get", name] => Mode::Mcp(McpAction::Get {
                    name: (*name).to_string(),
                }),
                ["get"] => return Err("nuo mcp get requires a server name".into()),
                ["probe", name] => Mode::Mcp(McpAction::Probe {
                    name: (*name).to_string(),
                }),
                ["probe"] => return Err("nuo mcp probe requires a server name".into()),
                ["add", ..] | ["rm", ..] | ["remove", ..] | ["enable", ..] | ["disable", ..] | ["import", ..] => {
                    return Err(
                        "nuo mcp mutation commands (add, rm, enable, disable, import) are retired (ADR-0252). \
                         Configure MCP servers declaratively in ~/.config/nuo/config.toml or .nuo/config.toml."
                            .into(),
                    );
                }
                [bad, ..] => return unexpected(bad),
            }
        }
        "skill" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] => return Err("nuo skill needs a subcommand: `nuo skill ls`, `nuo skill show <name>`, `nuo skill info <name>`, `nuo skill init <name>`".into()),
                ["ls"] | ["list"] => Mode::Skill(SkillAction::List),
                ["show", name] => Mode::Skill(SkillAction::Show { name: (*name).to_string() }),
                ["info", name] => Mode::Skill(SkillAction::Info { name: (*name).to_string() }),
                ["init", name] => Mode::Skill(SkillAction::Init { name: (*name).to_string(), user: false }),
                ["init", name, "--user"] | ["init", "--user", name] => Mode::Skill(SkillAction::Init { name: (*name).to_string(), user: true }),
                [bad, ..] => return unexpected(bad),
            }
        }
        "doctor" => {
            if extra.is_empty() {
                Mode::Doctor
            } else {
                return unexpected(&extra[0]);
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
        "start" => &[
            ("--fg", "stay in the foreground (default: detach)"),
            ("--port <n>", "TCP port (default: NUO_PORT, else 9800)"),
            ("--public", "bind all interfaces; requires the bearer token"),
            (
                "--no-local-auth",
                "drop the loopback bearer-token requirement",
            ),
            (
                "--idle-exit <min>",
                "auto-exit after <min> idle minutes (0 = never)",
            ),
            ("--grace <secs>", "graceful-drain budget in seconds"),
        ],
        "status" => &[
            ("--watch", "keep streaming live updates"),
            ("--json", "emit one JSON frame per update"),
            ("--all", "include idle sessions"),
            ("--diagnostic", "report discovery/lock/socket/log health"),
        ],
        "session" => &[("rm <id>", "terminate a hosted session by id")],
        "mcp" => &[
            ("ls", "list configured MCP servers"),
            ("get <name>", "print one server's config entry"),
            ("probe <name>", "connect once and list the advertised tools"),
        ],
        "skill" => &[("ls", "list discovered skills")],
        _ => &[],
    }
}

/// The help text for a topic: `None` is the top-level help, a command
/// name its per-command text. Returns `None` for unknown topics.
pub fn help_text(topic: Option<&str>) -> Option<String> {
    let mut out = String::new();
    match topic {
        None => {
            out.push_str("nuo — session daemon and control plane\n\n");
            out.push_str("Usage: nuo [OPTIONS]\n       nuo [OPTIONS] <COMMAND>\n\nCommands:\n");
            let width = COMMANDS.iter().map(|s| s.name.len()).max().unwrap_or(0);
            for spec in COMMANDS {
                out.push_str(&format!("  {:<width$}  {}\n", spec.name, spec.about));
            }
            out.push_str("\nOptions:\n");
            out.push_str("  -j, --json             emit structured JSON where supported\n");
            out.push_str("      --project <path>   operate on the project at <path>\n");
            out.push_str("  -h, --help             print help ('nuo help <command>' for more)\n");
            out.push_str("  -V, --version          print the version and exit\n");
            out.push_str("\nEnvironment:\n");
            out.push_str(
                "  NUO_HOME              instance root for isolated execution (<dir>/nuo)\n",
            );
            out.push_str(
                "  NUO_PORT              override default daemon TCP port (default: 9800)\n",
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
            if spec.name == "start" {
                out.push_str(
                    "\nThe daemon hosts every session across every project. `start` runs it\n",
                );
                out.push_str("detached by default (--fg for systemd/tmux-style supervision);\n");
                out.push_str(
                    "`stop` drains within the daemon's --grace budget before escalating;\n",
                );
                out.push_str("`status` observes without ever spawning one.\n");
                out.push_str(
                    "\nNUO_HOME points the whole instance (config, data, daemon files,\n",
                );
                out.push_str("port default via NUO_PORT) at an isolated root — the dev/test\n");
                out.push_str("sandbox shape. See `nuo --help` for the config paths.\n");
            }
        }
    }
    Some(out)
}

fn subs_of(cmd: &str) -> Option<&'static [Spec]> {
    match cmd {
        "session" => Some(SESSION_SUBS),
        "config" => Some(CONFIG_SUBS),
        "auth" => Some(AUTH_SUBS),
        "mcp" => Some(MCP_SUBS),
        "skill" => Some(SKILL_SUBS),
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
        assert!(matches!(
            parse(&[]).unwrap().mode,
            Mode::Interactive(_)
        ));
    }

    #[test]
    fn serve_starts_foreground_daemon() {
        assert!(matches!(
            parse(&["serve"]).unwrap().mode,
            Mode::Server(ServerAction::Start {
                foreground: true,
                ..
            })
        ));
    }

    #[test]
    fn start_defaults_to_foreground_daemon() {
        let parsed = parse(&["start"]).unwrap();
        assert!(matches!(
            parsed.mode,
            Mode::Server(ServerAction::Start {
                foreground: true,
                client_driven: false,
                ..
            })
        ));
    }

    #[test]
    fn start_with_detach_sets_background() {
        let parsed = parse(&["start", "--detach"]).unwrap();
        assert!(matches!(
            parsed.mode,
            Mode::Server(ServerAction::Start {
                foreground: false,
                ..
            })
        ));
    }

    #[test]
    fn start_with_client_driven_sets_flag() {
        let parsed = parse(&["start", "--client-driven"]).unwrap();
        assert!(matches!(
            parsed.mode,
            Mode::Server(ServerAction::Start {
                client_driven: true,
                ..
            })
        ));
    }

    #[test]
    fn top_level_daemon_verbs_are_canonical() {
        assert!(matches!(
            parse(&["start"]).unwrap().mode,
            Mode::Server(ServerAction::Start { .. })
        ));
        assert!(matches!(
            parse(&["stop"]).unwrap().mode,
            Mode::Server(ServerAction::Stop)
        ));
        assert!(matches!(
            parse(&["restart"]).unwrap().mode,
            Mode::Server(ServerAction::Restart { .. })
        ));
        assert!(matches!(
            parse(&["status"]).unwrap().mode,
            Mode::Server(ServerAction::Status { .. })
        ));
        assert!(matches!(
            parse(&["token"]).unwrap().mode,
            Mode::Server(ServerAction::Token)
        ));
    }

    #[test]
    fn server_subcommands_parse() {
        assert!(matches!(
            parse(&["server", "start"]).unwrap().mode,
            Mode::Server(ServerAction::Start { .. })
        ));
        assert!(matches!(
            parse(&["server", "stop"]).unwrap().mode,
            Mode::Server(ServerAction::Stop)
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
    fn daemon_noun_is_not_accepted() {
        let command = "daemon";
        assert!(parse(&[command]).is_err(), "{command}");
        assert!(parse(&[command, "start"]).is_err(), "{command} start");
        assert!(parse(&[command, "stop"]).is_err(), "{command} stop");
        assert!(parse(&[command, "status"]).is_err(), "{command} status");
        assert!(parse(&[command, "token"]).is_err(), "{command} token");
    }

    #[test]
    fn tui_commands_route_to_interactive() {
        for command in ["run", "attach", "dashboard", "settings"] {
            assert!(
                matches!(parse(&[command]).unwrap().mode, Mode::Interactive(_)),
                "{command}"
            );
        }
    }

    #[test]
    fn mcp_verbs_parse() {
        assert!(matches!(
            parse(&["mcp", "ls"]).unwrap().mode,
            Mode::Mcp(McpAction::List)
        ));
        assert!(matches!(
            parse(&["mcp", "get", "aegis"]).unwrap().mode,
            Mode::Mcp(McpAction::Get { ref name }) if name == "aegis"
        ));
        assert!(matches!(
            parse(&["mcp", "probe", "aegis"]).unwrap().mode,
            Mode::Mcp(McpAction::Probe { ref name }) if name == "aegis"
        ));
    }

    #[test]
    fn mcp_mutations_are_retired_with_informative_error() {
        // ADR-0252: imperative mutations are retired in favor of declarative TOML configuration.
        let add_err = parse(&["mcp", "add", "aegis", "--", "cmd"]).unwrap_err();
        assert!(add_err.contains("retired (ADR-0252)"));

        let rm_err = parse(&["mcp", "rm", "aegis"]).unwrap_err();
        assert!(rm_err.contains("retired (ADR-0252)"));

        let enable_err = parse(&["mcp", "enable", "aegis"]).unwrap_err();
        assert!(enable_err.contains("retired (ADR-0252)"));

        let import_err = parse(&["mcp", "import", "-"]).unwrap_err();
        assert!(import_err.contains("retired (ADR-0252)"));
    }
}
