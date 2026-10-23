//! Client-side presentation adapter for server-owned composer completion.
//!
//! Matching, intent steering, project scanning, and path resolution happen in
//! Nuo. The TUI only requests results, translates wire offsets into Rust byte
//! offsets, and renders/applies the returned edits.
use crate::App;
use crate::composer::{composer_text_width, composer_wrapped_pos};
use crate::design::COMPOSER_PROMPT_PREFIX_COLS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDoc {
    pub name: String,
    /// The single prose introduction (contract has exactly one field).
    pub summary: String,
    pub usage: Vec<String>,
    pub category: Option<String>,
    /// First-token verbs (`/schedule list` → `list`) with their own
    /// introductions; rendered after the parent's usage block.
    pub subcommands: Vec<(String, String)>,
}

impl CommandDoc {
    pub fn from_spec(spec: &nuo_wire::CommandSpec) -> Self {
        Self {
            name: spec.name.clone(),
            summary: spec.summary.clone(),
            usage: spec.usage.clone(),
            category: spec.category.clone(),
            subcommands: spec
                .subcommands
                .iter()
                .map(|sub| (sub.name.clone(), sub.summary.clone()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionKind {
    #[default]
    None,
    Slash,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CompletionItemKind {
    #[default]
    Slash,
    SlashAlias,
    IntentSuggestion {
        matched_intent: String,
        reason: String,
    },
    PathFile,
    PathDir,
    PathExplicit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub description: String,
    pub insert_text: String,
    pub replace_start: usize,
    pub replace_end: usize,
    pub kind: CompletionItemKind,
    /// Canonical command when this row is an alias (`/yolo` → `/unattended`).
    /// Drives the distinct row rendering; the accepted edit already commits
    /// the canonical spelling via `insert_text`.
    pub alias_of: Option<String>,
    pub doc: Option<CommandDoc>,
}

impl Completion {
    pub fn whole_input(label: &str, description: &str, input_len: usize) -> Self {
        Self {
            label: label.to_string(),
            description: description.to_string(),
            insert_text: label.to_string(),
            replace_start: 0,
            replace_end: input_len,
            kind: CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        }
    }

    fn from_backend(input: &str, item: &nuo_wire::InputCompletion) -> Option<Self> {
        let replace_start = char_to_byte(input, item.replace_start)?;
        let replace_end = char_to_byte(input, item.replace_end)?;
        if replace_start > replace_end {
            return None;
        }
        let kind = match item.kind {
            nuo_wire::InputCompletionKind::Slash => CompletionItemKind::Slash,
            nuo_wire::InputCompletionKind::SlashAlias => CompletionItemKind::SlashAlias,
            nuo_wire::InputCompletionKind::Intent => CompletionItemKind::IntentSuggestion {
                matched_intent: String::new(),
                reason: item.description.clone(),
            },
            nuo_wire::InputCompletionKind::PathFile => CompletionItemKind::PathFile,
            nuo_wire::InputCompletionKind::PathDir => CompletionItemKind::PathDir,
            nuo_wire::InputCompletionKind::PathExplicit => CompletionItemKind::PathExplicit,
        };
        Some(Self {
            label: item.label.clone(),
            description: item.description.clone(),
            insert_text: item.insert_text.clone(),
            replace_start,
            replace_end,
            kind,
            alias_of: item.alias_of.clone(),
            doc: item.command.as_ref().map(CommandDoc::from_spec),
        })
    }
}

fn char_to_byte(input: &str, char_index: usize) -> Option<usize> {
    if char_index == input.chars().count() {
        Some(input.len())
    } else {
        input.char_indices().nth(char_index).map(|(byte, _)| byte)
    }
}

pub fn completion_anchor(
    input: &str,
    byte_cursor: usize,
    input_rect: nuotc::Rect,
    input_scroll: usize,
    kind: CompletionKind,
) -> nuotc::Rect {
    let text_width = composer_text_width(input_rect.width as usize);
    let trigger_byte = match kind {
        CompletionKind::Path => mention_range_at(input, byte_cursor)
            .map(|(start, _)| start)
            .unwrap_or(0),
        _ => 0,
    };
    let (row, col) = composer_wrapped_pos(input, text_width, trigger_byte);
    let x = input_rect.x + COMPOSER_PROMPT_PREFIX_COLS as u16 + col.min(text_width) as u16;

    let visible_rows = (input_rect.height as usize)
        .saturating_sub(crate::design::COMPOSER_VERTICAL_CHROME_ROWS as usize)
        .max(1);

    let anchor_y = if row <= input_scroll {
        input_rect.y
    } else {
        let visible_row = (row - input_scroll).min(visible_rows.saturating_sub(1));
        input_rect.y + crate::design::COMPOSER_TEXT_ROW_OFFSET + visible_row as u16
    };

    nuotc::Rect::new(x, anchor_y, 1, 1)
}

pub fn completion_anchor_x(
    input: &str,
    byte_cursor: usize,
    input_rect: nuotc::Rect,
    kind: CompletionKind,
) -> u16 {
    completion_anchor(input, byte_cursor, input_rect, 0, kind).x
}

pub fn resolved_slash_command_len(
    input: &str,
    catalog: &nuo_wire::CommandCatalog,
) -> Option<usize> {
    if !input.starts_with('/') {
        return None;
    }
    let token = input
        .trim()
        .split_once(char::is_whitespace)
        .map(|(name, _)| name)
        .unwrap_or_else(|| input.trim());
    (token.len() > 1 && catalog.recognizes(token)).then_some(token.len())
}

pub(super) fn mention_range_at(input: &str, cursor_byte: usize) -> Option<(usize, usize)> {
    nuo_wire::mention::mention_range_at(input, cursor_byte)
}

impl App {
    pub fn completion_kind(&self) -> CompletionKind {
        if self.input.starts_with('/') {
            CompletionKind::Slash
        } else if self.active_mention_range().is_some() {
            CompletionKind::Path
        } else {
            CompletionKind::None
        }
    }

    pub fn completion_trigger_text_present(&self) -> bool {
        match self.completion_kind() {
            CompletionKind::None => false,
            CompletionKind::Slash => {
                !self.input.trim().is_empty() && !self.known_exact_slash_input()
            }
            CompletionKind::Path => true,
        }
    }

    fn known_exact_slash_input(&self) -> bool {
        let Some(first) = self.input.split_whitespace().next() else {
            return false;
        };
        self.input.starts_with('/')
            && self.input.trim() == first
            && self.command_catalog.recognizes(first)
    }

    pub fn anchor_completion_selection(&mut self, completions: &[Completion]) {
        let input_len = self.input.len();
        let exact = completions.iter().any(|item| {
            item.replace_start == 0 && item.replace_end == input_len && item.label == self.input
        });
        let visible = !completions.is_empty() && !exact;
        match (visible, self.suggestion_index) {
            (false, _) => self.suggestion_index = None,
            (true, None) => self.suggestion_index = Some(0),
            (true, Some(index)) => {
                self.suggestion_index = Some(index.min(completions.len() - 1));
            }
        }
    }

    /// Current completions translated into renderer-native byte edits.
    ///
    /// Implements Two-Tier Completion:
    /// - Tier 1: Zero-latency synchronous execution for slash and harness commands.
    /// - Tier 2: Path mentions and dynamic queries with SWR (Stale-While-Revalidate) cache retention.
    pub fn completions(&self) -> Vec<Completion> {
        let cursor = self.cursor_position;

        // Tier 1: Synchronous zero-latency matching for slash and harness commands
        if self.input.starts_with('/') {
            let cursor_byte = char_to_byte(&self.input, cursor).unwrap_or(self.input.len());
            let items = complete_slash_items(&self.command_catalog, &self.input, cursor_byte);
            return items
                .iter()
                .filter_map(|item| Completion::from_backend(&self.input, item))
                .collect();
        }

        // Tier 2: Path mentions and dynamic queries from backend (SWR with cache retention)
        let items = if self.completion_response_input.as_deref() == Some(self.input.as_str())
            && self.completion_response_cursor == cursor
        {
            self.backend_completions.clone()
        } else if self.active_mention_range().is_some() && !self.backend_completions.is_empty() {
            // SWR: Retain active backend completions while in-flight
            self.backend_completions.clone()
        } else {
            #[cfg(test)]
            {
                complete_for_frontend_test(
                    self.command_catalog.clone(),
                    self.cwd.clone(),
                    &self.input,
                    cursor,
                )
            }
            #[cfg(not(test))]
            {
                Vec::new()
            }
        };

        items
            .iter()
            .filter_map(|item| Completion::from_backend(&self.input, item))
            .collect()
    }

    /// Send a completion request when the composer state changed.
    pub fn refresh_backend_completion_request(&mut self) {
        let cursor = self.cursor_position;
        let state = (self.input.clone(), cursor);
        if self.completion_requested.as_ref() == Some(&state) {
            return;
        }
        self.completion_requested = Some(state.clone());

        // When not inside an active mention, clean up stale backend completions.
        // Slash commands are handled synchronously by Tier 1 with zero latency.
        if !self.input.starts_with('/') && self.active_mention_range().is_none() {
            self.backend_completions.clear();
            self.completion_response_input = None;
            self.completion_response_cursor = 0;
            return;
        }

        // Retain `backend_completions` during typing (SWR).
        // Only bump generation request ID and send request if dynamic path completion is needed.
        if self.active_mention_range().is_some() {
            self.completion_request_id = self.completion_request_id.wrapping_add(1);
            self.send_intent(nuo_wire::AgentRequest::CompleteComposer {
                request_id: self.completion_request_id,
                text: state.0,
                cursor,
            });
        }
    }

    pub fn apply_backend_completions(
        &mut self,
        request_id: u64,
        input: String,
        cursor: usize,
        items: Vec<nuo_wire::InputCompletion>,
    ) {
        if request_id != self.completion_request_id
            || input != self.input
            || cursor != self.cursor_position
        {
            return;
        }
        self.completion_response_input = Some(input);
        self.completion_response_cursor = cursor;
        self.backend_completions = items;
    }

    pub fn active_mention_range(&self) -> Option<(usize, usize)> {
        mention_range_at(&self.input, self.byte_cursor())
    }
}

/// Standard slash command catalog for frontend client auto-completion.
/// One slash-command spec row: `(command, help, aliases, key=value flags)`.
type RawCommandSpec = (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [(&'static str, &'static str)],
);

pub fn command_catalog(custom: &[(String, String)]) -> nuo_wire::CommandCatalog {
    let raw_specs: &[RawCommandSpec] = &[
        (
            "/models",
            "Switch the active model (context preserved)",
            &[
                "model",
                "llm",
                "switch",
                "provider",
                "gpt",
                "claude",
                "gemini",
                "deepseek",
                "change-model",
            ],
            &[],
        ),
        (
            "/connections",
            "Manage LLM provider connections",
            &[
                "connection",
                "provider",
                "api-key",
                "auth",
                "endpoint",
                "credentials",
                "token",
                "login",
            ],
            &[],
        ),
        (
            "/tools",
            "Manage session tools (enable/disable)",
            &[
                "tools",
                "tool",
                "function",
                "bash",
                "toggle",
                "disable",
                "enable",
                "mcp-tools",
            ],
            &[],
        ),
        (
            "/mcp",
            "Manage MCP servers (status, reconnect)",
            &[
                "mcp",
                "server",
                "protocol",
                "context",
                "reconnect",
                "mcp-server",
            ],
            &[],
        ),
        (
            "/compact",
            "Compact older rounds into durable context memory",
            &[
                "compact",
                "compress",
                "summarize",
                "prune",
                "truncate",
                "shrink",
                "clean-context",
                "context",
            ],
            &[],
        ),
        (
            "/new",
            "Start a new session, keeping history",
            &[
                "clear",
                "reset",
                "clean",
                "restart",
                "fresh",
                "cls",
                "wipe",
                "blank",
                "new-session",
            ],
            &[],
        ),
        (
            "/permissions",
            "Show or clear always-allowed tool rules",
            &[
                "permission",
                "allow",
                "rule",
                "policy",
                "security",
                "approve",
                "always-allow",
                "grant",
            ],
            &[("clear", "Clear all always-allowed tool rules")],
        ),
        (
            "/unattended",
            "Toggle unattended execution posture",
            &[
                "unattended",
                "auto",
                "autopilot",
                "yolo",
                "delegate",
                "autonomous",
                "headless",
                "skip-confirm",
                "auto-approve",
                "bypass",
            ],
            &[
                ("on", "Enable unattended decisions and tool auto-approval"),
                ("off", "Return to interactive confirmation mode"),
            ],
        ),
        (
            "/delegate",
            "Delegate tasks to subagents",
            &["delegate", "subagent"],
            &[("on", "Enable delegation"), ("off", "Disable delegation")],
        ),
        (
            "/confinement",
            "Toggle workspace filesystem confinement (confine file tools to workspace)",
            &[
                "confinement",
                "confined",
                "unconfine",
                "unconfined",
                "jail",
                "escape",
                "sandbox",
            ],
            &[
                (
                    "on",
                    "Enable workspace confinement (confine file tools to workspace)",
                ),
                (
                    "off",
                    "Disable confinement (allow full host filesystem access)",
                ),
            ],
        ),
        (
            "/role",
            "Switch agent role (identity, capability, and workspace)",
            &[
                "role",
                "preset",
                "mode",
                "identity",
                "developer",
                "philosophist",
                "ops",
                "switch",
                "switch-role",
            ],
            &[
                (
                    "developer",
                    "the default developer role (full native capabilities with workspace)",
                ),
                (
                    "ops",
                    "system administration, infrastructure maintenance & remote operations (workspace-free)",
                ),
                (
                    "philosophist",
                    "philosophical inquiry & reflection (workspace-free)",
                ),
            ],
        ),
        (
            "/search",
            "Semantic search over session history",
            &[
                "search",
                "find",
                "query",
                "grep",
                "history",
                "lookup",
                "recall",
                "past-messages",
            ],
            &[],
        ),
        (
            "/threads",
            "Browse or resume past threads",
            &[
                "threads",
                "thread",
                "sessions",
                "session",
                "resume",
                "continue",
                "history",
                "list",
                "reopen",
                "browse",
                "switch-thread",
            ],
            &[],
        ),
        (
            "/fork",
            "Fork thread into a sovereign child thread tab (round/turn granular)",
            &[
                "fork",
                "branch",
                "clone",
                "duplicate",
                "split",
                "copy-thread",
                "aside",
                "side",
                "btw",
            ],
            &[],
        ),
        (
            "/tree",
            "Visual DAG session tree and branch navigation",
            &[
                "tree",
                "dag",
                "branch",
                "branches",
                "lineage",
                "timeline",
                "history-tree",
                "checkout",
            ],
            &[],
        ),
        (
            "/diff",
            "View workspace modifications made in this session",
            &[
                "diff",
                "changes",
                "modified",
                "patch",
                "git-diff",
                "review-changes",
            ],
            &[],
        ),
        (
            "/undo",
            "Undo the last thread turn and file changes",
            &["undo", "revert", "rollback", "back", "pop", "discard-turn"],
            &[],
        ),
        (
            "/stats",
            "View context token accounting and thread stats",
            &[
                "stats",
                "tokens",
                "context",
                "thread-stats",
                "tps",
                "streaming-rate",
            ],
            &[],
        ),
        (
            "/trace",
            "Inspect session execution trace and latency waterfall",
            &[
                "trace",
                "waterfall",
                "latency",
                "ttft",
                "tps",
                "profiler",
                "execution",
            ],
            &[],
        ),
        (
            "/usage",
            "Cross-session token usage statistics overlay",
            &[
                "usage",
                "stats",
                "statistics",
                "tokens",
                "tokens-per-day",
                "daily",
                "consumption",
                "spend",
                "quota",
            ],
            &[],
        ),
        (
            "/jobs",
            "Inspect and manage background processes and sub-subagents",
            &[
                "jobs",
                "job",
                "background",
                "process",
                "task",
                "tasks",
                "running",
                "kill",
                "ps",
                "async",
            ],
            &[
                ("kill", "Terminate an active background job"),
                (
                    "logs",
                    "Show recent stdout/stderr output of a background job",
                ),
            ],
        ),
        (
            "/skills",
            "Browse available skills and view trust status",
            &["skills", "skill", "capabilities", "extensions"],
            &[
                ("list", "List discovered project and user skills"),
                ("status", "Show skills health and trust state"),
            ],
        ),
        (
            "/init",
            "Scaffold a project-local .nuo/ config tree",
            &["init", "scaffold", "bootstrap", "create-config"],
            &[],
        ),
        (
            "/trust",
            "Trust project-authored asset domains",
            &[
                "trust",
                "authorize",
                "asset-trust",
                "project-assets",
                "security",
            ],
            &[
                ("all", "Trust every present project asset domain"),
                ("instructions", "Trust project instructions and AGENTS.md"),
                ("ex-workspace", "Trust project external workspace roots"),
                ("mcp", "Trust project MCP definitions only"),
                ("skills", "Trust project skills only"),
                ("hooks", "Trust project hooks only"),
                ("status", "Show trust state for every asset domain"),
                ("revoke", "Revoke every asset-domain grant"),
            ],
        ),
        (
            "/untrust",
            "Revoke project asset trust",
            &["untrust", "revoke", "quarantine", "asset-trust"],
            &[],
        ),
        (
            "/export",
            "Export thread to clipboard as Markdown",
            &[
                "export",
                "copy",
                "share",
                "clipboard",
                "markdown",
                "dump",
                "save",
            ],
            &[],
        ),
        (
            "/retry",
            "Retry last failed model request",
            &["retry", "again", "resend", "redo", "re-run"],
            &[],
        ),
        (
            "/help",
            "Show available commands and keybindings",
            &[
                "help",
                "man",
                "docs",
                "guide",
                "info",
                "usage",
                "?",
                "shortcuts",
                "keybindings",
            ],
            &[],
        ),
        (
            "/exit",
            "Exit the program",
            &["exit", "quit", "q", "leave", "bye", "shutdown"],
            &[],
        ),
    ];

    let mut catalog = nuo_wire::CommandCatalog::default();
    for &(name, summary, kws, subcmds) in raw_specs {
        catalog.commands.push(nuo_wire::CommandSpec {
            name: name.to_string(),
            summary: summary.to_string(),
            usage: vec![name.to_string()],
            examples: vec![],
            intent_keywords: kws.iter().map(|s| s.to_string()).collect(),
            category: Some(if name == "/models" {
                "Model".into()
            } else {
                "system".into()
            }),
            subcommands: subcmds
                .iter()
                .map(|(s, desc)| nuo_wire::CommandSubcommandSpec {
                    name: s.to_string(),
                    summary: desc.to_string(),
                })
                .collect(),
        });
    }

    catalog.aliases.push(nuo_wire::CommandAlias {
        name: "/setup".to_string(),
        target: "/init".to_string(),
    });
    catalog.aliases.push(nuo_wire::CommandAlias {
        name: "/resume".to_string(),
        target: "/threads".to_string(),
    });
    catalog.aliases.push(nuo_wire::CommandAlias {
        name: "/sessions".to_string(),
        target: "/threads".to_string(),
    });

    catalog.suggestions.push(nuo_wire::CommandSuggestion {
        trigger: "/clear".into(),
        target: "/new".into(),
        reason: "Start a fresh session".into(),
    });
    catalog.suggestions.push(nuo_wire::CommandSuggestion {
        trigger: "/continue".into(),
        target: "/threads".into(),
        reason: "Resume a previous thread".into(),
    });

    for (name, desc) in custom {
        catalog.commands.push(nuo_wire::CommandSpec {
            name: name.clone(),
            summary: desc.clone(),
            usage: vec![name.clone()],
            examples: vec![],
            intent_keywords: vec![],
            category: Some("custom".into()),
            subcommands: vec![],
        });
    }
    catalog
}

fn slash_item(
    label: &str,
    description: &str,
    replace_end: usize,
    command: &nuo_wire::CommandSpec,
    intent: bool,
) -> nuo_wire::InputCompletion {
    nuo_wire::InputCompletion {
        label: label.to_string(),
        description: description.to_string(),
        insert_text: label.to_string(),
        replace_start: 0,
        replace_end,
        kind: if intent {
            nuo_wire::InputCompletionKind::Intent
        } else {
            nuo_wire::InputCompletionKind::Slash
        },
        alias_of: None,
        command: Some(command.clone()),
    }
}

pub fn complete_slash_items(
    catalog: &nuo_wire::CommandCatalog,
    input: &str,
    cursor_byte: usize,
) -> Vec<nuo_wire::InputCompletion> {
    let current = input[..cursor_byte.min(input.len())].to_lowercase();
    let Some(trigger) = current.strip_prefix('/') else {
        return Vec::new();
    };
    let replace_end = input.chars().count();

    if current.contains(char::is_whitespace) {
        return complete_subcommand_items(catalog, input, &current, replace_end);
    }

    let mut items = Vec::new();
    for spec in &catalog.commands {
        if spec.name.to_lowercase().starts_with(&current) {
            items.push(slash_item(
                &spec.name,
                &spec.summary,
                replace_end,
                spec,
                false,
            ));
        }
    }
    for alias in &catalog.aliases {
        let name = alias.name.to_lowercase();
        if !name.starts_with(&current) {
            continue;
        }
        let target_spec = catalog.find(&alias.target);
        items.push(nuo_wire::InputCompletion {
            label: alias.name.clone(),
            description: target_spec
                .map(|spec| spec.summary.clone())
                .unwrap_or_else(|| alias.target.clone()),
            insert_text: alias.target.clone(),
            replace_start: 0,
            replace_end,
            kind: nuo_wire::InputCompletionKind::SlashAlias,
            alias_of: Some(alias.target.clone()),
            command: target_spec.cloned(),
        });
    }

    let trigger_suggestion = catalog
        .suggestions
        .iter()
        .find(|suggestion| suggestion.trigger.eq_ignore_ascii_case(trigger))
        .map(|suggestion| {
            let command = catalog.find(&suggestion.target).cloned();
            nuo_wire::InputCompletion {
                label: suggestion.target.clone(),
                description: suggestion.reason.clone(),
                insert_text: suggestion.target.clone(),
                replace_start: 0,
                replace_end,
                kind: nuo_wire::InputCompletionKind::Intent,
                alias_of: None,
                command,
            }
        });
    let trigger_target = trigger_suggestion.as_ref().map(|item| item.label.clone());
    items.extend(trigger_suggestion);

    let intent: Vec<nuo_wire::InputCompletion> = (!trigger.is_empty())
        .then(|| {
            catalog.commands.iter().filter_map(|spec| {
                if spec.name.to_lowercase().starts_with(&current)
                    || trigger_target.as_deref() == Some(spec.name.as_str())
                    || items.iter().any(|item| item.label == spec.name)
                {
                    return None;
                }
                let matched = spec.intent_keywords.iter().find(|keyword| {
                    keyword.eq_ignore_ascii_case(trigger)
                        || (trigger.len() >= 3 && keyword.to_lowercase().starts_with(trigger))
                })?;
                Some(slash_item(
                    &spec.name,
                    &format!("(via '{matched}') {}", spec.summary),
                    replace_end,
                    spec,
                    true,
                ))
            })
        })
        .into_iter()
        .flatten()
        .collect();

    items.extend(intent);
    items
}

fn complete_subcommand_items(
    catalog: &nuo_wire::CommandCatalog,
    input: &str,
    current_lower: &str,
    replace_end: usize,
) -> Vec<nuo_wire::InputCompletion> {
    let mut tokens = current_lower.split_whitespace();
    let command_name = tokens.next().unwrap_or_default().to_string();
    let Some(spec) = catalog.find(&command_name) else {
        return Vec::new();
    };
    let cursor_in_trailing_space = current_lower.ends_with(char::is_whitespace);
    let token_count = current_lower.split_whitespace().count();
    let matches_second_token_position = matches!(
        (cursor_in_trailing_space, token_count),
        (true, 1) | (false, 2)
    );
    let trailing = if cursor_in_trailing_space {
        ""
    } else {
        current_lower.split_whitespace().nth(1).unwrap_or("")
    };

    if matches_second_token_position && !spec.subcommands.is_empty() {
        let typed_parent_len = command_name.len();
        return spec
            .subcommands
            .iter()
            .filter(|sub| sub.name.starts_with(trailing))
            .map(|sub| nuo_wire::InputCompletion {
                label: format!("{} {}", command_name, sub.name),
                description: sub.summary.clone(),
                insert_text: format!(
                    "{} {}",
                    &input[..typed_parent_len.min(input.len())],
                    sub.name
                ),
                replace_start: 0,
                replace_end,
                kind: nuo_wire::InputCompletionKind::Slash,
                alias_of: None,
                command: Some(spec.clone()),
            })
            .collect();
    }

    let mut candidates = Vec::new();
    for usage in &spec.usage {
        for expanded in expand_usage_options(usage) {
            if !candidates.contains(&expanded) {
                candidates.push(expanded);
            }
        }
    }
    let canonical_input = catalog
        .alias(&command_name)
        .map(|alias| current_lower.replacen(command_name.as_str(), &alias.target, 1))
        .unwrap_or_else(|| current_lower.to_string());
    candidates
        .into_iter()
        .filter(|cand| cand.to_lowercase().starts_with(&canonical_input))
        .map(|cand| slash_item(&cand, &spec.summary, replace_end, spec, false))
        .collect()
}

pub(crate) fn expand_usage_options(usage: &str) -> Vec<String> {
    let mut words = Vec::new();
    for word in usage.split_whitespace() {
        if word.starts_with('<') && word.ends_with('>') {
            continue;
        }
        if word.starts_with('[') && word.ends_with(']') && !word.contains('|') {
            continue;
        }
        words.push(word);
    }

    if words.len() <= 1 {
        return Vec::new();
    }

    let mut current_expansions = vec![String::new()];

    for word in words {
        let cleaned = if (word.starts_with('[') && word.ends_with(']'))
            || (word.starts_with('(') && word.ends_with(')'))
        {
            if word.contains('|') {
                &word[1..word.len() - 1]
            } else {
                word
            }
        } else {
            word
        };

        let variants: Vec<&str> = if cleaned.contains('|') {
            cleaned.split('|').filter(|s| !s.is_empty()).collect()
        } else {
            vec![word]
        };

        let mut next_expansions = Vec::new();
        for prefix in &current_expansions {
            for variant in &variants {
                if prefix.is_empty() {
                    next_expansions.push(variant.to_string());
                } else {
                    next_expansions.push(format!("{prefix} {variant}"));
                }
            }
        }
        current_expansions = next_expansions;
    }

    current_expansions
        .into_iter()
        .filter(|s| s.contains(' '))
        .collect()
}

pub fn complete_for_frontend_test(
    catalog: nuo_wire::CommandCatalog,
    project_root: std::path::PathBuf,
    input: &str,
    cursor: usize,
) -> Vec<nuo_wire::InputCompletion> {
    if input.starts_with('/') {
        return complete_slash_items(&catalog, input, cursor);
    }
    if let Some(at_idx) = input[..cursor.min(input.len())].rfind('@') {
        let query = &input[at_idx + 1..cursor.min(input.len())];
        if query.is_empty() {
            return vec![
                nuo_wire::InputCompletion {
                    label: "@file:".into(),
                    description: "Reference a workspace file".into(),
                    insert_text: "@file:".into(),
                    replace_start: at_idx,
                    replace_end: cursor,
                    kind: nuo_wire::ComposerCompletionKind::PathDir,
                    alias_of: None,
                    command: None,
                },
                nuo_wire::InputCompletion {
                    label: "@skill:".into(),
                    description: "Invoke a skill".into(),
                    insert_text: "@skill:".into(),
                    replace_start: at_idx,
                    replace_end: cursor,
                    kind: nuo_wire::ComposerCompletionKind::PathDir,
                    alias_of: None,
                    command: None,
                },
            ];
        }

        if query.starts_with("../") || query.starts_with("./") || query.starts_with('/') {
            let (base_dir, query_tail) = if let Some(stripped) = query.strip_prefix("../") {
                (project_root.parent().map(|p| p.to_path_buf()), stripped)
            } else if let Some(stripped) = query.strip_prefix("./") {
                (Some(project_root.clone()), stripped)
            } else {
                (
                    Some(std::path::PathBuf::from("/")),
                    query.strip_prefix('/').unwrap_or(query),
                )
            };
            if let Some(dir) = base_dir {
                let mut items = Vec::new();
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        let filename = path.file_name().unwrap_or_default().to_string_lossy();
                        if filename.starts_with(query_tail) {
                            let abs_str = path.to_string_lossy().to_string();
                            items.push(nuo_wire::InputCompletion {
                                label: abs_str.clone(),
                                description: "Explicit path".into(),
                                insert_text: format!("{abs_str} "),
                                replace_start: at_idx,
                                replace_end: cursor,
                                kind: nuo_wire::ComposerCompletionKind::PathExplicit,
                                alias_of: None,
                                command: None,
                            });
                        }
                    }
                }
                return items;
            }
        }

        if let Some(file_query) = query.strip_prefix("file:") {
            let mut items = Vec::new();

            if file_query.is_empty() {
                if let Ok(entries) = std::fs::read_dir(&project_root) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        let filename = path.file_name().unwrap_or_default().to_string_lossy();
                        if filename == ".git" {
                            continue;
                        }
                        let is_dir = path.is_dir();
                        let label = if is_dir {
                            format!("@file:{filename}/")
                        } else {
                            format!("@file:{filename}")
                        };
                        let needs_space = input
                            .get(cursor..)
                            .and_then(|suffix| suffix.chars().next())
                            .map(|ch| !ch.is_whitespace())
                            .unwrap_or(true);
                        let insert = if !is_dir && needs_space {
                            format!("{label} ")
                        } else {
                            label.clone()
                        };
                        items.push(nuo_wire::InputCompletion {
                            label,
                            description: if is_dir {
                                "Workspace directory".into()
                            } else {
                                "Workspace file".into()
                            },
                            insert_text: insert,
                            replace_start: at_idx,
                            replace_end: cursor,
                            kind: if is_dir {
                                nuo_wire::ComposerCompletionKind::PathDir
                            } else {
                                nuo_wire::ComposerCompletionKind::PathFile
                            },
                            alias_of: None,
                            command: None,
                        });
                    }
                }
                return items;
            }

            if file_query.ends_with('/') {
                let target_dir = project_root.join(file_query);
                if target_dir.is_dir() {
                    items.push(nuo_wire::InputCompletion {
                        label: format!("@file:{file_query}"),
                        description: "Workspace directory".into(),
                        insert_text: format!("@file:{file_query}"),
                        replace_start: at_idx,
                        replace_end: cursor,
                        kind: nuo_wire::ComposerCompletionKind::PathDir,
                        alias_of: None,
                        command: None,
                    });
                }
                fn collect_descendants(
                    base: &std::path::Path,
                    current: &std::path::Path,
                    input: &str,
                    items: &mut Vec<nuo_wire::InputCompletion>,
                    at_idx: usize,
                    cursor: usize,
                ) {
                    if let Ok(entries) = std::fs::read_dir(current) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.file_name().unwrap_or_default() == ".git" {
                                continue;
                            }
                            if let Ok(rel) = path.strip_prefix(base) {
                                let rel_str = rel.to_string_lossy().replace('\\', "/");
                                let is_dir = path.is_dir();
                                let label = if is_dir {
                                    format!("@file:{rel_str}/")
                                } else {
                                    format!("@file:{rel_str}")
                                };
                                let needs_space = input
                                    .get(cursor..)
                                    .and_then(|suffix| suffix.chars().next())
                                    .map(|ch| !ch.is_whitespace())
                                    .unwrap_or(true);
                                let insert = if !is_dir && needs_space {
                                    format!("{label} ")
                                } else {
                                    label.clone()
                                };
                                items.push(nuo_wire::InputCompletion {
                                    label,
                                    description: if is_dir {
                                        "Workspace directory".into()
                                    } else {
                                        "Workspace file".into()
                                    },
                                    insert_text: insert,
                                    replace_start: at_idx,
                                    replace_end: cursor,
                                    kind: if is_dir {
                                        nuo_wire::ComposerCompletionKind::PathDir
                                    } else {
                                        nuo_wire::ComposerCompletionKind::PathFile
                                    },
                                    alias_of: None,
                                    command: None,
                                });
                                if is_dir {
                                    collect_descendants(base, &path, input, items, at_idx, cursor);
                                }
                            }
                        }
                    }
                }
                collect_descendants(
                    &project_root,
                    &target_dir,
                    input,
                    &mut items,
                    at_idx,
                    cursor,
                );
                return items;
            }

            fn collect_matching(
                base: &std::path::Path,
                current: &std::path::Path,
                query: &str,
                input: &str,
                items: &mut Vec<nuo_wire::InputCompletion>,
                at_idx: usize,
                cursor: usize,
            ) {
                if let Ok(entries) = std::fs::read_dir(current) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.file_name().unwrap_or_default() == ".git" {
                            continue;
                        }
                        if let Ok(rel) = path.strip_prefix(base) {
                            let rel_str = rel.to_string_lossy().replace('\\', "/");
                            let is_dir = path.is_dir();
                            if rel_str.contains(query) {
                                let label = if is_dir {
                                    format!("@file:{rel_str}/")
                                } else {
                                    format!("@file:{rel_str}")
                                };
                                let needs_space = input
                                    .get(cursor..)
                                    .and_then(|suffix| suffix.chars().next())
                                    .map(|ch| !ch.is_whitespace())
                                    .unwrap_or(true);
                                let insert = if !is_dir && needs_space {
                                    format!("{label} ")
                                } else {
                                    label.clone()
                                };
                                items.push(nuo_wire::InputCompletion {
                                    label,
                                    description: if is_dir {
                                        "Workspace directory".into()
                                    } else {
                                        "Workspace file".into()
                                    },
                                    insert_text: insert,
                                    replace_start: at_idx,
                                    replace_end: cursor,
                                    kind: if is_dir {
                                        nuo_wire::ComposerCompletionKind::PathDir
                                    } else {
                                        nuo_wire::ComposerCompletionKind::PathFile
                                    },
                                    alias_of: None,
                                    command: None,
                                });
                            }
                            if is_dir {
                                collect_matching(base, &path, query, input, items, at_idx, cursor);
                            }
                        }
                    }
                }
            }
            collect_matching(
                &project_root,
                &project_root,
                file_query,
                input,
                &mut items,
                at_idx,
                cursor,
            );
            return items;
        }

        if let Some(skill_query) = query.strip_prefix("skill:") {
            let mut items = vec![nuo_wire::InputCompletion {
                label: "skill-creator".into(),
                description: "Built-in skill creator".into(),
                insert_text: "@skill:skill-creator".into(),
                replace_start: at_idx,
                replace_end: cursor,
                kind: nuo_wire::ComposerCompletionKind::PathFile,
                alias_of: None,
                command: None,
            }];
            items.retain(|i| i.label.starts_with(skill_query));
            return items;
        }
    }
    Vec::new()
}
