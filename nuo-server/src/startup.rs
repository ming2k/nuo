//! Process bootstrap for the runtime: tracing init, the session-start
//! vocabulary the harness assembles from, and the slash-command table that
//! distinguishes built-in commands from user-defined ones.
//!
//! The *command-line parser* no longer lives here (ADR-0116): parsing is a
//! frontend concern and moved to `mutx::cli`, which also owns help
//! text and shell completions. What remains is what the session runtime
//! itself needs to start a session and interpret slash commands.

use tracing_appender::non_blocking::WorkerGuard;

use crate::log_rotate::RetainedRollingFile;
use nuo_persistence::paths;

/// The CLI default control-plane port (ADR-0105): fixed so browser clients
/// (which cannot read the discovery record) have a well-known endpoint; the
/// daemon falls back to an ephemeral port when it is taken.
pub const DEFAULT_SERVE_PORT: u16 = 9800;

/// The port a daemon binds when no `--port` was given, honouring
/// `MUTA_PORT` (ADR-0121): an isolated instance — `MUTA_HOME` sandbox,
/// second user session, container — must not fight the host daemon over the
/// well-known port. Explicit `--port` still wins over the env var. An
/// unparsable value falls back to the well-known default (an env var is
/// ambient configuration; failing the daemon over a typo would be worse
/// than the collision it prevents).
pub fn env_default_port() -> u16 {
    std::env::var("MUTA_PORT")
        .ok()
        .and_then(|raw| raw.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_SERVE_PORT)
}

/// How a hosted session begins (ADR-0116: the session-runtime half of what
/// used to be the CLI's `StartupMode`). Only the shapes the harness can
/// assemble exist here; one-shot CLI modes (`run`, `daemon`, `config`, …)
/// are frontend concerns and never reach a session assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStart {
    /// A brand-new session.
    Fresh,
    /// A fresh session with the first prompt already queued.
    FreshWithPrompt(String),
    /// Resume a specific stored session.
    Resume(String),
    /// Open with the sessions picker over an empty carrier session.
    Picker,
}

/// Category of a slash command for grouping and visual badging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCategory {
    Session,
    Model,
    Config,
    Tools,
    Agent,
    Automation,
    Project,
    System,
    Debug,
}

impl CommandCategory {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Model => "Model",
            Self::Config => "Config",
            Self::Tools => "Tools",
            Self::Agent => "Agent",
            Self::Automation => "Automation",
            Self::Project => "Project",
            Self::System => "System",
            Self::Debug => "Debug",
        }
    }
}

/// Rich metadata and specification for a harness command.
///
/// Contains:
/// - `name`: The canonical slash-prefixed name (e.g. `"/schedule"`)
/// - `summary`: THE single prose introduction (<= 30-60 chars); used verbatim
///   in compact lists, the inspector panel, and `/help`
/// - `usage`: Syntax signatures and parameters
/// - `examples`: Concrete practical invocations with inline explanations
/// - `intent_keywords`: Synonyms and intent cues used to guess what command the user wants
/// - `category`: Logical grouping
/// - `subcommands`: First-token verbs with their own introductions
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub usage: &'static [&'static str],
    pub examples: &'static [(&'static str, &'static str)],
    pub intent_keywords: &'static [&'static str],
    pub category: CommandCategory,
    pub subcommands: &'static [(&'static str, &'static str)],
}

/// Single source of truth for the built-in slash-command vocabulary.
///
/// Each entry generates a [`BuiltinCmd`] enum variant, a row in [`BuiltinCmd::ALL`]
/// and [`BuiltinCmd::SPECS`] (consumed by input completion, `/help`, inspector,
/// and the custom-command filter), and an arm of [`BuiltinCmd::from_slash`].
///
/// The dispatch `match` in `main.rs` is over `Option<BuiltinCmd>` and is kept
/// non-exhaustive (no `Some(_)` catch-all). Adding a variant here without a
/// matching handler arm is therefore a **compile error**, so completion,
/// `/help`, and dispatch can never drift — a command appears in all three or
/// the build breaks.
macro_rules! define_builtin_commands {
    ( $(
        $variant:ident = $name:literal : {
            summary: $summary:literal,
            usage: [ $( $usage:literal ),* $(,)? ],
            examples: [ $( ($ex_cmd:literal, $ex_desc:literal) ),* $(,)? ],
            intent_keywords: [ $( $kw:literal ),* $(,)? ],
            category: $cat:ident,
            $( subcommands: [ $( ($sub_name:literal, $sub_summary:literal) ),* $(,)? ] ,)?
        }
    ),+ $(,)? ) => {
        /// The set of built-in slash commands. Generated from a single
        /// declarative list — see `define_builtin_commands`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum BuiltinCmd {
            $( $variant ),+
        }

        impl BuiltinCmd {
            /// Every built-in command as `(slash_name, summary)`, in
            /// declaration order.
            pub const ALL: &[(&'static str, &'static str)] = &[ $( ($name, $summary) ),+ ];

            /// Every built-in command specification with usage signatures,
            /// examples, intent keywords, and declared subcommands.
            pub const SPECS: &'static [CommandSpec] = &[
                $(
                    CommandSpec {
                        name: $name,
                        summary: $summary,
                        usage: &[ $( $usage ),* ],
                        examples: &[ $( ($ex_cmd, $ex_desc) ),* ],
                        intent_keywords: &[ $( $kw ),* ],
                        category: CommandCategory::$cat,
                        subcommands: &subcommands![ $($($sub_name, $sub_summary),*)? ],
                    }
                ),+
            ];

            /// Get the full specification for this command.
            pub fn spec(self) -> &'static CommandSpec {
                match self {
                    $( BuiltinCmd::$variant => Self::find_spec($name).expect("builtin spec exists"), )+
                }
            }

            /// Canonical slash command name (e.g. "/models").
            pub fn slash_name(self) -> &'static str {
                match self {
                    $( BuiltinCmd::$variant => $name, )+
                }
            }

            /// Find the command specification by its canonical slash name.
            pub fn find_spec(name: &str) -> Option<&'static CommandSpec> {
                Self::SPECS.iter().find(|s| s.name == name)
            }

            /// Parse a `/<name>` token into a variant, or `None` when it is
            /// not a built-in (i.e. a custom command).
            pub fn from_slash(input: &str) -> Option<Self> {
                $( if input == $name { return Some(BuiltinCmd::$variant); } )+
                Self::from_alias(input)
            }
        }
    };
}

/// Helper behind `define_builtin_commands!`: an omitted `subcommands: […]`
/// entry becomes an empty slice instead of a macro syntax error.
macro_rules! subcommands {
    () => { &[] as &[(&'static str, &'static str)] };
    ( $($name:literal, $summary:literal),* $(,)? ) => {
        &( [ $( ($name, $summary) ),* ] ) as &[(&'static str, &'static str)]
    };
}

define_builtin_commands! {
    Models = "/models" : {
        summary: "Switch the active model (context preserved)",
        usage: ["/models"],
        examples: [("/models", "Open model selector modal")],
        intent_keywords: ["model", "llm", "switch", "provider", "gpt", "claude", "gemini", "deepseek", "change-model"],
        category: Model,
    },
    Connections = "/connections" : {
        summary: "Manage LLM provider connections",
        usage: ["/connections"],
        examples: [("/connections", "Open provider connection manager")],
        intent_keywords: ["connection", "provider", "api-key", "auth", "endpoint", "credentials", "token", "login"],
        category: Model,
    },
    Tools = "/tools" : {
        summary: "Manage session tools (enable/disable)",
        usage: ["/tools"],
        examples: [("/tools", "Open tool management overlay")],
        intent_keywords: ["tools", "tool", "function", "bash", "toggle", "disable", "enable", "mcp-tools"],
        category: Tools,
    },
    Mcp = "/mcp" : {
        summary: "Manage MCP servers (status, reconnect)",
        usage: ["/mcp"],
        examples: [("/mcp", "Inspect and manage MCP servers")],
        intent_keywords: ["mcp", "server", "protocol", "context", "reconnect", "mcp-server"],
        category: Tools,
    },
    Compact = "/compact" : {
        summary: "Compact older rounds into durable context memory",
        usage: ["/compact"],
        examples: [("/compact", "Trigger immediate conversation compaction")],
        intent_keywords: ["compact", "compress", "summarize", "prune", "truncate", "shrink", "clean-context", "context"],
        category: Session,
    },
    New = "/new" : {
        summary: "Start a new session, keeping history",
        usage: ["/new"],
        examples: [("/new", "Start a fresh session")],
        intent_keywords: ["clear", "reset", "clean", "restart", "fresh", "cls", "wipe", "blank", "new-session"],
        category: Session,
    },
    Permissions = "/permissions" : {
        summary: "Show or clear always-allowed tool rules",
        usage: ["/permissions", "/permissions clear"],
        examples: [("/permissions", "Show active permission rules"), ("/permissions clear", "Clear process-local auto-allow rules")],
        intent_keywords: ["permission", "allow", "rule", "policy", "security", "approve", "always-allow", "grant"],
        category: Config,
        subcommands: [
            ("clear", "Clear all always-allowed tool rules"),
        ],
    },
    Settings = "/settings" : {
        summary: "Open Settings overlay (theme, appearance)",
        usage: ["/settings"],
        examples: [("/settings", "Open Settings overlay")],
        intent_keywords: ["settings", "config", "preferences", "theme", "themes", "appearance", "options", "color", "layout", "conf"],
        category: Config,
        subcommands: [],
    },
    Unattended = "/unattended" : {
        summary: "Toggle unattended execution posture",
        usage: ["/unattended", "/unattended on", "/unattended off"],
        examples: [("/unattended on", "Enable unattended execution mode"), ("/unattended off", "Return to interactive confirmation mode")],
        intent_keywords: ["unattended", "auto", "autopilot", "yolo", "delegate", "autonomous", "headless", "skip-confirm", "auto-approve", "bypass"],
        category: Automation,
        subcommands: [
            ("on", "Enable unattended decisions and tool auto-approval"),
            ("off", "Return to interactive confirmation mode"),
        ],
    },
    Confinement = "/confinement" : {
        summary: "Toggle workspace filesystem confinement (confine file tools to workspace)",
        usage: ["/confinement", "/confinement on", "/confinement off"],
        examples: [
            ("/confinement on", "Enable workspace confinement (restrict file tools to workspace)"),
            ("/confinement off", "Disable workspace confinement (allow full host filesystem access)"),
        ],
        intent_keywords: ["confinement", "confined", "unconfine", "unconfined", "jail", "escape", "sandbox"],
        category: Automation,
        subcommands: [
            ("on", "Enable workspace confinement (confine file tools to workspace)"),
            ("off", "Disable confinement (allow full host filesystem access)"),
        ],
    },
    Role = "/role" : {
        summary: "Switch agent role (identity, capability, and workspace)",
        usage: [
            "/role",
            "/role ops",
            "/role philosophist",
            "/role developer <workspace>",
        ],
        examples: [
            ("/role developer /path/to/project", "Switch to developer role with workspace"),
            ("/role ops", "Switch to ops role (workspace-free, systems & remote operations)"),
            ("/role philosophist", "Switch to philosophist role (workspace-free)"),
        ],
        intent_keywords: [
            "role", "preset", "mode", "identity", "developer", "philosophist", "ops",
            "switch", "switch-role",
        ],
        category: Agent,
        subcommands: [
            ("developer", "the default developer role (full native capabilities with workspace)"),
            ("ops", "system administration, infrastructure maintenance & remote operations (workspace-free)"),
            ("philosophist", "philosophical inquiry & reflection (workspace-free)"),
        ],
    },
    Search = "/search" : {
        summary: "Semantic search over session history",
        usage: ["/search <query>"],
        examples: [("/search auth token handling", "Find past discussions on authentication")],
        intent_keywords: ["search", "find", "query", "grep", "history", "lookup", "recall", "past-messages"],
        category: Session,
    },
    Sessions = "/sessions" : {
        summary: "Browse or resume past sessions",
        usage: ["/sessions", "/sessions <id>"],
        examples: [("/sessions", "Open interactive session picker"), ("/sessions 0195", "Resume session matching prefix")],
        intent_keywords: ["sessions", "session", "resume", "continue", "history", "list", "reopen", "browse", "switch-session"],
        category: Session,
    },
    Fork = "/fork" : {
        summary: "Fork conversation into a child session",
        usage: ["/fork"],
        examples: [("/fork", "Branch current conversation into a child session")],
        intent_keywords: ["fork", "branch", "clone", "duplicate", "split", "copy-session"],
        category: Session,
    },
    Tree = "/tree" : {
        summary: "Visual DAG session tree and branch navigation",
        usage: ["/tree"],
        examples: [("/tree", "Open interactive DAG session tree")],
        intent_keywords: ["tree", "dag", "branch", "branches", "lineage", "timeline", "history-tree", "checkout"],
        category: Session,
    },
    Diff = "/diff" : {
        summary: "View workspace modifications made in this session",
        usage: ["/diff"],
        examples: [("/diff", "View workspace file changes")],
        intent_keywords: ["diff", "changes", "modified", "patch", "git-diff", "review-changes"],
        category: Session,
    },
    Undo = "/undo" : {
        summary: "Undo the last conversation turn and file changes",
        usage: ["/undo"],
        examples: [("/undo", "Undo last turn")],
        intent_keywords: ["undo", "revert", "rollback", "back", "pop", "discard-turn"],
        category: Session,
    },
    Dashboard = "/dashboard" : {
        summary: "Session daemon control dashboard",
        usage: ["/dashboard"],
        examples: [("/dashboard", "Open full-screen session dashboard")],
        intent_keywords: ["dashboard", "host", "daemon", "monitor", "status", "overview", "dock", "fleet"],
        category: System,
    },
    Usage = "/usage" : {
        summary: "Cross-session token usage statistics overlay",
        usage: ["/usage"],
        examples: [("/usage", "Open the usage statistics overlay")],
        intent_keywords: ["usage", "stats", "statistics", "tokens", "tokens-per-day", "daily", "consumption", "spend", "quota"],
        category: System,
    },
    Btw = "/btw" : {
        summary: "Open a background side conversation (aside)",
        usage: ["/btw", "/btw <prompt>", "/btw list"],
        examples: [("/btw explain this regex", "Ask a quick side question"), ("/btw list", "Open active asides list modal")],
        intent_keywords: ["btw", "aside", "side", "subtask", "parallel", "quick", "note", "by-the-way"],
        category: Session,
        subcommands: [
            ("list", "Open the active asides modal"),
        ],
    },
    Jobs = "/jobs" : {
        summary: "Inspect and manage background processes and sub-subagents",
        usage: ["/jobs", "/jobs kill <id>", "/jobs logs <id>"],
        examples: [
            ("/jobs", "List all active and recent background jobs"),
            ("/jobs kill job_01a2b3c4", "Terminate a running background job"),
            ("/jobs logs job_01a2b3c4", "Show tail logs for a background job"),
        ],
        intent_keywords: ["jobs", "job", "background", "process", "task", "tasks", "running", "kill", "ps", "async"],
        category: Automation,
        subcommands: [
            ("kill", "Terminate an active background job"),
            ("logs", "Show recent stdout/stderr output of a background job"),
        ],
    },
    Skills = "/skills" : {
        summary: "Browse available skills and view trust status",
        usage: ["/skills", "/skills list", "/skills status"],
        examples: [("/skills", "List available skills"), ("/skills status", "Show skills health and trust state")],
        intent_keywords: ["skills", "skill", "capabilities", "extensions"],
        category: Tools,
        subcommands: [
            ("list", "List discovered project and user skills"),
            ("status", "Show skills health and trust state"),
        ],
    },
    Init = "/init" : {
        summary: "Scaffold a project-local .nuo/ config tree",
        usage: ["/init [path]"],
        examples: [("/init", "Initialize .nuo in current directory")],
        intent_keywords: ["init", "scaffold", "bootstrap", "create-config"],
        category: Project,
    },
    Trust = "/trust" : {
        summary: "Trust project-authored asset domains",
        usage: ["/trust", "/trust [all|instructions|ex-workspace|mcp|skills|hooks|status|revoke]"],
        examples: [
            ("/trust", "Trust every present project asset domain"),
            ("/trust instructions", "Trust project instructions and AGENTS.md"),
            ("/trust ex-workspace", "Trust project external workspace roots"),
            ("/trust mcp", "Trust project MCP definitions only"),
            ("/trust skills", "Trust project skills only"),
            ("/trust hooks", "Trust project hooks only"),
            ("/trust status", "Show trust state for every asset domain"),
            ("/trust revoke", "Revoke every asset-domain grant for this workspace"),
        ],
        intent_keywords: ["trust", "authorize", "asset-trust", "project-assets", "security"],
        category: Project,
        subcommands: [
            ("all", "Trust every present project asset domain"),
            ("instructions", "Trust project instructions and AGENTS.md"),
            ("ex-workspace", "Trust project external workspace roots"),
            ("mcp", "Trust project MCP definitions only"),
            ("skills", "Trust project skills only"),
            ("hooks", "Trust project hooks only"),
            ("status", "Show trust state for every asset domain"),
            ("revoke", "Revoke every asset-domain grant"),
        ],
    },
    Untrust = "/untrust" : {
        summary: "Revoke project asset trust",
        usage: ["/untrust"],
        examples: [("/untrust", "Revoke all project asset trust")],
        intent_keywords: ["untrust", "revoke", "quarantine", "asset-trust"],
        category: Project,
    },
    Export = "/export" : {
        summary: "Export conversation to clipboard as Markdown",
        usage: ["/export"],
        examples: [("/export", "Copy conversation markdown to clipboard")],
        intent_keywords: ["export", "copy", "share", "clipboard", "markdown", "dump", "save"],
        category: Session,
    },
    Debug = "/debug" : {
        summary: "Dev tools: request tracing and body preview",
        usage: ["/debug trace [on|off]", "/debug preview"],
        examples: [("/debug trace on", "Enable request tracing"), ("/debug preview", "Dry run next LLM request payload")],
        intent_keywords: ["debug", "trace", "log", "dry-run", "inspect", "troubleshoot"],
        category: Debug,
        subcommands: [
            ("trace", "Toggle provider round-trip capture on/off"),
            ("preview", "Dry-run next provider wire body to disk"),
        ],
    },
    Retry = "/retry" : {
        summary: "Retry last failed model request",
        usage: ["/retry"],
        examples: [("/retry", "Retry last request")],
        intent_keywords: ["retry", "again", "resend", "redo", "re-run"],
        category: Session,
    },
    Help = "/help" : {
        summary: "Show available commands and keybindings",
        usage: ["/help [topic]"],
        examples: [("/help", "Open help guide"), ("/help sessions", "Show help for /sessions")],
        intent_keywords: ["help", "man", "docs", "guide", "info", "usage", "?", "shortcuts", "keybindings"],
        category: System,
    },
    Exit = "/exit" : {
        summary: "Exit the program",
        usage: ["/exit"],
        examples: [("/exit", "Exit application")],
        intent_keywords: ["exit", "quit", "q", "leave", "bye", "shutdown"],
        category: System,
    },
}

impl BuiltinCmd {
    /// Backward-compatible aliases for renamed commands. Unlike the legacy
    /// behaviour, aliases are **first-class completion candidates** (see
    /// `CommandAlias` in `muta-contracts`): typing `/set` offers a `setup`
    /// row that stays `/setup` when selected — the alias is only resolved to
    /// its target at dispatch time. They remain absent from
    /// [`BuiltinCmd::ALL`] so `/help` keeps listing canonical names only.
    fn from_alias(input: &str) -> Option<Self> {
        match input {
            // `/host` was the pre-dashboard name (it leaked the daemon "host"
            // concept, ADR-0096); the surface is now the session dashboard.
            "/host" => Some(BuiltinCmd::Dashboard),
            // `/resume` was a second spelling of `/session resume` — an
            // identical help line with no ADR justifying the duplication, and
            // an arm that skipped the provider-pin reapply. Retired as an
            // alias of `/sessions`: bare `/resume` opens the picker, and
            // `/resume <id>` opens that session.
            "/resume" => Some(BuiltinCmd::Sessions),
            // `/session` grew subcommands that all duplicate better surfaces:
            // `status`/`list` are read-only reports, `resume`/`open` fold
            // into `/sessions`, `new` is `/new`, and `fork` is now
            // top-level. The alias keeps old invocations working (the
            // handler translates the legacy grammar).
            "/session" => Some(BuiltinCmd::Sessions),
            // `/config` was renamed to `/settings`; the legacy alias keeps old invocations working.
            "/config" => Some(BuiltinCmd::Settings),
            // `/auto`, `/delegate`, `/autopilot`, `/yolo` are aliases for `/unattended`.
            "/auto" | "/delegate" | "/autopilot" | "/yolo" => Some(BuiltinCmd::Unattended),
            // `/unconfine`, `/unconfined`, `/jail`, `/escape` are aliases for `/confinement`.
            "/unconfine" | "/unconfined" | "/jail" | "/escape" => Some(BuiltinCmd::Confinement),
            _ => None,
        }
    }
}

/// Trigger-word → command suggestions ("did you mean …"), the
/// **presentation-only** counterpart of `BuiltinCmd::from_alias`.
///
/// Each row is `(trigger, target_slash, reason)`:
///
/// - `trigger` — a bare word the user might type after `/` (no slash, no
///   arguments). It is **never parsed as a command**: unlike a `from_alias`
///   alias it does not dispatch, does not accept arguments, and is invisible
///   to every consumer of [`BuiltinCmd::ALL`] — it only produces a completion
///   popup row pointing at the real command.
/// - `target_slash` — the canonical built-in (leading slash) the suggestion
///   accepts to; it must resolve through [`BuiltinCmd::from_slash`].
/// - `reason` — the one-line hint shown next to the target in the popup.
///
/// This is the place to catch retired commands, common synonyms, and foreign
/// idioms and steer them onto the supported vocabulary without growing the
/// executable surface. Adding a row here is all it takes: completion reads
/// the table through [`suggest_for_trigger`].
pub const TRIGGER_WORD_SUGGESTIONS: &[(&str, &str, &str)] = &[
    // `/clear` used to wipe the live transcript in place — a destructive,
    // data-losing gesture. It is retired in favour of `/new`, which opens a
    // fresh session and keeps the old one on disk. Steer the muscle memory.
    (
        "clear",
        "/new",
        "Clearing in place is gone: /new starts a fresh session and keeps this one",
    ),
    (
        "reset",
        "/new",
        "/new starts a fresh session, keeping the current one in history",
    ),
    (
        "continue",
        "/sessions",
        "/sessions picks the session up where it left off",
    ),
    (
        "preferences",
        "/settings",
        "/settings opens the Settings overlay (theme, layout, behavior)",
    ),
    (
        "options",
        "/settings",
        "/settings opens the Settings overlay",
    ),
    (
        "theme",
        "/settings",
        "/settings lets you select and customize color themes",
    ),
    (
        "themes",
        "/settings",
        "/settings lets you select and customize color themes",
    ),
    (
        "appearance",
        "/settings",
        "/settings lets you customize UI appearance and layout",
    ),
];

/// Resolve a bare word typed after `/` into a "did you mean" suggestion from
/// [`TRIGGER_WORD_SUGGESTIONS`]. Returns `(target_slash, reason)` for an exact
/// match, `None` otherwise. Exact-match only: a partial trigger (`/cle`) is
/// still prose-in-progress, and a non-trigger word is an unknown command.
pub fn suggest_for_trigger(word: &str) -> Option<(&'static str, &'static str)> {
    TRIGGER_WORD_SUGGESTIONS
        .iter()
        .find(|(trigger, _, _)| *trigger == word)
        .map(|(_, target, reason)| (*target, *reason))
}

/// Build the frontend-neutral command catalog published by the daemon during
/// attach. Built-ins, compatibility aliases, trigger steering, and trusted
/// project commands all originate here so TUI and Web never maintain their
/// own command vocabulary.
pub fn command_catalog(custom: &[(String, String)]) -> nuo_wire::CommandCatalog {
    let mut commands = BuiltinCmd::SPECS
        .iter()
        .map(|spec| nuo_wire::CommandSpec {
            name: spec.name.to_string(),
            summary: spec.summary.to_string(),
            usage: spec
                .usage
                .iter()
                .map(|usage| (*usage).to_string())
                .collect(),
            examples: spec
                .examples
                .iter()
                .map(|(command, description)| nuo_wire::CommandExample {
                    command: (*command).to_string(),
                    description: (*description).to_string(),
                })
                .collect(),
            intent_keywords: spec
                .intent_keywords
                .iter()
                .map(|keyword| (*keyword).to_string())
                .collect(),
            category: Some(spec.category.label().to_string()),
            subcommands: {
                let mut subs: Vec<nuo_wire::CommandSubcommandSpec> = spec
                    .subcommands
                    .iter()
                    .map(|(name, summary)| nuo_wire::CommandSubcommandSpec {
                        name: (*name).to_string(),
                        summary: (*summary).to_string(),
                    })
                    .collect();
                if spec.name == "/role" {
                    let user_roles = nuo_persistence::roles::RolesConfig::load();
                    for (id, p) in user_roles.roles {
                        if !subs.iter().any(|s| s.name == id) {
                            let desc = p.description.as_deref().unwrap_or(p.name.as_str());
                            subs.push(nuo_wire::CommandSubcommandSpec {
                                name: id,
                                summary: desc.to_string(),
                            });
                        }
                    }
                }
                subs
            },
        })
        .collect::<Vec<_>>();
    commands.extend(
        custom
            .iter()
            .map(|(name, summary)| nuo_wire::CommandSpec {
                name: name.clone(),
                summary: summary.clone(),
                usage: vec![name.clone()],
                examples: Vec::new(),
                intent_keywords: Vec::new(),
                category: Some("Project".to_string()),
                subcommands: Vec::new(),
            }),
    );

    nuo_wire::CommandCatalog {
        commands,
        aliases: [
            ("/host", "/dashboard"),
            ("/resume", "/sessions"),
            ("/session", "/sessions"),
            ("/config", "/settings"),
            ("/auto", "/unattended"),
            ("/delegate", "/unattended"),
            ("/autopilot", "/unattended"),
            ("/yolo", "/unattended"),
            ("/unconfine", "/confinement"),
            ("/unconfined", "/confinement"),
            ("/jail", "/confinement"),
            ("/escape", "/confinement"),
        ]
        .into_iter()
        .map(|(name, target)| nuo_wire::CommandAlias {
            name: name.to_string(),
            target: target.to_string(),
        })
        .collect(),
        suggestions: TRIGGER_WORD_SUGGESTIONS
            .iter()
            .map(
                |(trigger, target, reason)| nuo_wire::CommandSuggestion {
                    trigger: (*trigger).to_string(),
                    target: (*target).to_string(),
                    reason: (*reason).to_string(),
                },
            )
            .collect(),
    }
}

/// Initialise file-based tracing for the process.
///
/// A TUI cannot log to stdout (it would corrupt the display), so tracing
/// always writes to a **file** under the XDG state directory:
/// `$XDG_STATE_HOME/muta/log/muta.log` (daily-rotated, so each calendar
/// day rolls into its own file).
///
/// # Verbosity
///
/// `MUTA_LOG` controls the level. Recognised values:
/// - `off` — disable tracing entirely (no file, no guard).
/// - `error` / `warn` / `info` / `debug` / `trace` — global level.
/// - _unrecognised / unset_ — defaults to `info`.
///
/// `RUST_LOG` still takes precedence per-target when set (e.g.
/// `RUST_LOG=muta=debug,muta_runtime=trace`), because
/// `EnvFilter::try_from_default_env` is consulted first. This keeps the
/// familiar `RUST_LOG` ergonomics for fine-grained filtering while giving a
/// sane always-on default out of the box.
///
/// The returned guard flushes the non-blocking writer on drop and must live
/// for the whole process (main binds it to a local).
pub fn init_tracing() -> Option<WorkerGuard> {
    // Level via MUTA_LOG; "off" disables tracing entirely.
    let level = std::env::var("MUTA_LOG").unwrap_or_else(|_| String::from("info"));
    if level.eq_ignore_ascii_case("off") {
        return None;
    }

    // Resolve the XDG state log directory and create it lazily.
    let dir = paths::get().log_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        // Last-resort: never block startup over logging. Drop to stderr-free
        // no-op by returning None; diagnostics are impossible from a TUI anyway.
        eprintln!("muta: could not create log dir {}: {e}", dir.display());
        return None;
    }

    let (writer, guard) = tracing_appender::non_blocking(RetainedRollingFile::new(
        dir.clone(),
        "muta.log",
        crate::log_rotate::retention_from_env(),
    ));

    // Per-target RUST_LOG wins; otherwise apply the MUTA_LOG level to the
    // muta crates and keep everything else quiet.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        let l = level.to_ascii_lowercase();
        let lvl = matches!(l.as_str(), "error" | "warn" | "info" | "debug" | "trace")
            .then_some(l.as_str())
            .unwrap_or("info");
        tracing_subscriber::EnvFilter::new(format!(
            "muta={lvl},nuo_wire={lvl},muta_runtime={lvl}"
        ))
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();
    tracing::info!(log_dir = %dir.display(), level = %level, "muta tracing initialised");
    Some(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_is_the_canonical_command() {
        assert!(matches!(
            BuiltinCmd::from_slash("/settings"),
            Some(BuiltinCmd::Settings)
        ));
    }

    #[test]
    fn config_resolves_as_settings_alias_but_is_not_listed() {
        assert!(matches!(
            BuiltinCmd::from_slash("/config"),
            Some(BuiltinCmd::Settings)
        ));
        assert!(!BuiltinCmd::ALL.iter().any(|(name, _)| *name == "/config"));
        assert!(BuiltinCmd::ALL.iter().any(|(name, _)| *name == "/settings"));
    }

    #[test]
    fn trigger_words_never_execute() {
        // The retired `/clear` and friends are steering words, not commands:
        // none of them parse through dispatch, land in completion's canonical
        // list, or collide with a real built-in.
        for (trigger, _, _) in crate::startup::TRIGGER_WORD_SUGGESTIONS {
            let slashed = format!("/{trigger}");
            assert!(
                BuiltinCmd::from_slash(&slashed).is_none(),
                "trigger word {slashed} must not resolve to a command"
            );
            assert!(
                !BuiltinCmd::ALL.iter().any(|(name, _)| *name == slashed),
                "trigger word {slashed} must not shadow a built-in"
            );
        }
        // …and `/clear` in particular is gone from the executable surface.
        assert!(BuiltinCmd::from_slash("/clear").is_none());
    }

    #[test]
    fn trigger_word_suggestions_point_at_real_commands() {
        // Every suggestion target must resolve to a listed built-in and carry
        // an explanation, so the popup never steers to a dead or cryptic row.
        for (trigger, target, reason) in crate::startup::TRIGGER_WORD_SUGGESTIONS {
            assert!(
                BuiltinCmd::from_slash(target).is_some(),
                "suggestion target {target} for trigger /{trigger} is not a command"
            );
            assert!(
                BuiltinCmd::ALL.iter().any(|(name, _)| name == target),
                "suggestion target {target} for trigger /{trigger} is not a listed built-in"
            );
            assert!(
                !reason.is_empty(),
                "trigger /{trigger} must explain the steer"
            );
        }
        // The lookup itself: exact, bare-word only.
        assert_eq!(
            crate::startup::suggest_for_trigger("clear").map(|(t, _)| t),
            Some("/new")
        );
        assert_eq!(
            crate::startup::suggest_for_trigger("continue").map(|(t, _)| t),
            Some("/sessions")
        );
        assert!(crate::startup::suggest_for_trigger("cle").is_none());
        assert!(crate::startup::suggest_for_trigger("new").is_none());
        assert!(crate::startup::suggest_for_trigger("").is_none());
    }

    #[test]
    fn command_specs_are_complete_and_non_empty() {
        assert_eq!(BuiltinCmd::SPECS.len(), BuiltinCmd::ALL.len());
        for spec in BuiltinCmd::SPECS {
            assert!(
                spec.name.starts_with('/'),
                "command name must start with slash: {}",
                spec.name
            );
            assert!(
                !spec.summary.is_empty(),
                "summary must not be empty for {}",
                spec.name
            );
            // Single-prose-field invariant: summaries carry the whole
            // introduction now, so they must be self-sufficient.
            assert!(
                spec.summary.chars().count() >= 12,
                "summary is too short to stand alone for {}: {}",
                spec.name,
                spec.summary
            );
            // Subcommand declarations must be unique per command: duplicates
            // would render as two completion rows with the same insert text.
            let mut seen_subs = std::collections::HashSet::new();
            for (sub_name, sub_summary) in spec.subcommands {
                assert!(
                    seen_subs.insert(*sub_name),
                    "duplicate subcommand '{}' on {}",
                    sub_name,
                    spec.name
                );
                assert!(
                    !sub_summary.is_empty(),
                    "subcommand summary must not be empty for {} {}",
                    spec.name,
                    sub_name
                );
            }
            assert!(
                !spec.usage.is_empty(),
                "usage must not be empty for {}",
                spec.name
            );
            assert!(
                !spec.intent_keywords.is_empty(),
                "intent keywords must not be empty for {}",
                spec.name
            );
            assert!(
                !spec.category.label().is_empty(),
                "category label must not be empty for {}",
                spec.name
            );

            // Spec lookup matches
            let found = BuiltinCmd::find_spec(spec.name).expect("find_spec must locate command");
            assert_eq!(found.name, spec.name);
            assert_eq!(found.summary, spec.summary);
        }
    }

    /// The docs-sync gate (the markdown drift this replaces could only be
    /// caught by a human): `docs/reference/commands.md`'s built-in command
    /// table must list exactly `BuiltinCmd::ALL` — no phantom commands, no
    /// undocumented ones. Hidden aliases (`/config` → `/settings`) are
    /// explicitly *not* advertised, so the doc must not list them either.
    #[test]
    fn commands_reference_table_matches_builtin_registry() {
        let Ok(doc) = std::fs::read_to_string("../../docs/reference/commands.md") else {
            // The doc lives at the workspace root; running from another
            // cwd (e.g. an installed crate) skips rather than fails.
            eprintln!("commands.md not reachable from this cwd; skipping");
            return;
        };
        // Only the *main* built-in command table is the advertised surface:
        // the block of table rows between "## Built-in commands" and the
        // next "##"/"###" heading. Later tables (trigger-word suggestions,
        // per-command detail tables) are deliberately out of scope.
        let mut in_table = false;
        let mut documented: Vec<String> = Vec::new();
        for line in doc.lines() {
            if line.starts_with("## ") {
                in_table = line.trim() == "## Built-in commands";
                continue;
            }
            if line.starts_with("###") {
                in_table = false;
                continue;
            }
            if !in_table || !line.starts_with('|') {
                continue;
            }
            let Some(start) = line.find('`') else {
                continue;
            };
            let rest = &line[start + 1..];
            let Some(end) = rest.find('`') else { continue };
            let cell = &rest[..end];
            if let Some(name) = cell.split_whitespace().next()
                && name.starts_with('/')
                && !name.contains('\\')
            {
                documented.push(name.to_string());
            }
        }
        let advertised: Vec<String> = BuiltinCmd::ALL
            .iter()
            .map(|(name, _)| format!("/{}", name.trim_start_matches('/')))
            .collect();
        for cmd in &advertised {
            assert!(
                documented.contains(cmd),
                "`{cmd}` is registered (appears in completion and /help) but missing \
                 from docs/reference/commands.md's table"
            );
        }
        for cmd in &documented {
            assert!(
                advertised.contains(cmd),
                "`{cmd}` is documented but not in BuiltinCmd::ALL — a phantom \
                 entry (renamed, removed, or a hidden alias leaked into the doc)"
            );
        }
    }
}
