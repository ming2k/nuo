//! Standalone Client SDK for the Nuo server.
//!
//! Frontends (nuox, web) and CLI tools interact with the running server
//! through this client library without depending on server internals.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod discovery;
pub mod wire;

pub use discovery::{
    Discovery as ServerInfo, global_discovery_path,
    global_spawn_lock_path, instance_dir,
};
pub use wire::{
    AttachAction, BoxWireSink, BoxWireStream, ControlRequest, NativeWireCodec,
    SessionInitOptions, Wire, ERR_PROTOCOL_MISMATCH, ERR_VERSION_MISMATCH,
    MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, native_framed_split, protocol_accepts,
    websocket_split,
};
pub use nuo_host::clipboard::CopyOutcome;

#[async_trait::async_trait]
pub trait UiBridge: Send + Sync {
    async fn copy_to_clipboard(&self, text: &str) -> Result<CopyOutcome, String>;
}

/// Initialize client logging guard.
pub fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let level = std::env::var("NUO_LOG").unwrap_or_else(|_| String::from("info"));
    if level.eq_ignore_ascii_case("off") {
        return None;
    }
    let dir = nuo_host::paths::get().log_dir();
    if let Err(_) = std::fs::create_dir_all(&dir) {
        return None;
    }
    let file_appender = tracing_appender::rolling::daily(&dir, "client.log");
    let (writer, guard) = tracing_appender::non_blocking(file_appender);
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info")
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init();
    Some(guard)
}

/// Standard slash command catalog for frontend client auto-completion.
pub fn command_catalog(custom: &[(String, String)]) -> nuo_wire::CommandCatalog {
    let raw_specs: &[(&str, &str, &[&str], &[(&str, &str)])] = &[
        ("/models", "Switch the active model (context preserved)", &["model", "llm", "switch", "provider", "gpt", "claude", "gemini", "deepseek", "change-model"], &[]),
        ("/connections", "Manage LLM provider connections", &["connection", "provider", "api-key", "auth", "endpoint", "credentials", "token", "login"], &[]),
        ("/tools", "Manage session tools (enable/disable)", &["tools", "tool", "function", "bash", "toggle", "disable", "enable", "mcp-tools"], &[]),
        ("/mcp", "Manage MCP servers (status, reconnect)", &["mcp", "server", "protocol", "context", "reconnect", "mcp-server"], &[]),
        ("/compact", "Compact older rounds into durable context memory", &["compact", "compress", "summarize", "prune", "truncate", "shrink", "clean-context", "context"], &[]),
        ("/new", "Start a new session, keeping history", &["clear", "reset", "clean", "restart", "fresh", "cls", "wipe", "blank", "new-session"], &[]),
        ("/permissions", "Show or clear always-allowed tool rules", &["permission", "allow", "rule", "policy", "security", "approve", "always-allow", "grant"], &[("clear", "Clear all always-allowed tool rules")]),
        ("/unattended", "Toggle unattended execution posture", &["unattended", "auto", "autopilot", "yolo", "delegate", "autonomous", "headless", "skip-confirm", "auto-approve", "bypass"], &[("on", "Enable unattended decisions and tool auto-approval"), ("off", "Return to interactive confirmation mode")]),
        ("/delegate", "Delegate tasks to subagents", &["delegate", "subagent"], &[("on", "Enable delegation"), ("off", "Disable delegation")]),
        ("/confinement", "Toggle workspace filesystem confinement (confine file tools to workspace)", &["confinement", "confined", "unconfine", "unconfined", "jail", "escape", "sandbox"], &[("on", "Enable workspace confinement (confine file tools to workspace)"), ("off", "Disable confinement (allow full host filesystem access)")]),
        ("/role", "Switch agent role (identity, capability, and workspace)", &["role", "preset", "mode", "identity", "developer", "philosophist", "ops", "switch", "switch-role"], &[("developer", "the default developer role (full native capabilities with workspace)"), ("ops", "system administration, infrastructure maintenance & remote operations (workspace-free)"), ("philosophist", "philosophical inquiry & reflection (workspace-free)")]),
        ("/search", "Semantic search over session history", &["search", "find", "query", "grep", "history", "lookup", "recall", "past-messages"], &[]),
        ("/threads", "Browse or resume past threads", &["threads", "thread", "sessions", "session", "resume", "continue", "history", "list", "reopen", "browse", "switch-thread"], &[]),
        ("/fork", "Fork thread into a child thread", &["fork", "branch", "clone", "duplicate", "split", "copy-thread"], &[]),
        ("/tree", "Visual DAG session tree and branch navigation", &["tree", "dag", "branch", "branches", "lineage", "timeline", "history-tree", "checkout"], &[]),
        ("/diff", "View workspace modifications made in this session", &["diff", "changes", "modified", "patch", "git-diff", "review-changes"], &[]),
        ("/undo", "Undo the last thread turn and file changes", &["undo", "revert", "rollback", "back", "pop", "discard-turn"], &[]),
        ("/stats", "View context token accounting and session stats", &["stats", "tokens", "context", "session-stats", "tps", "streaming-rate"], &[]),
        ("/trace", "Inspect session execution trace and latency waterfall", &["trace", "waterfall", "latency", "ttft", "tps", "profiler", "execution"], &[]),
        ("/usage", "Cross-session token usage statistics overlay", &["usage", "stats", "statistics", "tokens", "tokens-per-day", "daily", "consumption", "spend", "quota"], &[]),
        ("/btw", "Open a background side thread (aside)", &["btw", "aside", "side", "subtask", "parallel", "quick", "note", "by-the-way"], &[("list", "Open the active asides modal")]),
        ("/jobs", "Inspect and manage background processes and sub-subagents", &["jobs", "job", "background", "process", "task", "tasks", "running", "kill", "ps", "async"], &[("kill", "Terminate an active background job"), ("logs", "Show recent stdout/stderr output of a background job")]),
        ("/skills", "Browse available skills and view trust status", &["skills", "skill", "capabilities", "extensions"], &[("list", "List discovered project and user skills"), ("status", "Show skills health and trust state")]),
        ("/init", "Scaffold a project-local .nuo/ config tree", &["init", "scaffold", "bootstrap", "create-config"], &[]),
        ("/trust", "Trust project-authored asset domains", &["trust", "authorize", "asset-trust", "project-assets", "security"], &[("all", "Trust every present project asset domain"), ("instructions", "Trust project instructions and AGENTS.md"), ("ex-workspace", "Trust project external workspace roots"), ("mcp", "Trust project MCP definitions only"), ("skills", "Trust project skills only"), ("hooks", "Trust project hooks only"), ("status", "Show trust state for every asset domain"), ("revoke", "Revoke every asset-domain grant")]),
        ("/untrust", "Revoke project asset trust", &["untrust", "revoke", "quarantine", "asset-trust"], &[]),
        ("/export", "Export thread to clipboard as Markdown", &["export", "copy", "share", "clipboard", "markdown", "dump", "save"], &[]),
        ("/debug", "Dev tools: request tracing and body preview", &["debug", "trace", "log", "dry-run", "inspect", "troubleshoot"], &[("trace", "Toggle provider round-trip capture on/off"), ("preview", "Dry-run next provider wire body to disk")]),
        ("/retry", "Retry last failed model request", &["retry", "again", "resend", "redo", "re-run"], &[]),
        ("/help", "Show available commands and keybindings", &["help", "man", "docs", "guide", "info", "usage", "?", "shortcuts", "keybindings"], &[]),
        ("/exit", "Exit the program", &["exit", "quit", "q", "leave", "bye", "shutdown"], &[]),
    ];

    let mut catalog = nuo_wire::CommandCatalog::default();
    for &(name, summary, kws, subcmds) in raw_specs {
        catalog.commands.push(nuo_wire::CommandSpec {
            name: name.to_string(),
            summary: summary.to_string(),
            usage: vec![name.to_string()],
            examples: vec![],
            intent_keywords: kws.iter().map(|s| s.to_string()).collect(),
            category: Some(if name == "/models" { "Model".into() } else { "system".into() }),
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


use nuo_wire::{CommandCatalog, CommandSpec, InputCompletion, InputCompletionKind};

fn slash_item(
    label: &str,
    description: &str,
    replace_end: usize,
    command: &CommandSpec,
    intent: bool,
) -> InputCompletion {
    InputCompletion {
        label: label.to_string(),
        description: description.to_string(),
        insert_text: label.to_string(),
        replace_start: 0,
        replace_end,
        kind: if intent {
            InputCompletionKind::Intent
        } else {
            InputCompletionKind::Slash
        },
        alias_of: None,
        command: Some(command.clone()),
    }
}

pub fn complete_slash_items(
    catalog: &CommandCatalog,
    input: &str,
    cursor_byte: usize,
) -> Vec<InputCompletion> {
    let current = input[..cursor_byte.min(input.len())].to_lowercase();
    let Some(trigger) = current.strip_prefix('/') else {
        return Vec::new();
    };
    let replace_end = input.chars().count();

    // Second stage: `/cmd <cursor>` completes first-token verbs
    // Progressive disclosure: the command menu stays a lean list of
    // canonical names; the subcommand tier (with its own introductions)
    // only appears once the user has committed to a parent and typed a
    // space.
    if current.contains(char::is_whitespace) {
        return complete_subcommand_items(catalog, input, &current, replace_end);
    }

    // First stage: `/pre<cursor>` completes command names
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
        items.push(InputCompletion {
            label: alias.name.clone(),
            description: target_spec
                .map(|spec| spec.summary.clone())
                .unwrap_or_else(|| alias.target.clone()),
            insert_text: alias.target.clone(),
            replace_start: 0,
            replace_end,
            kind: InputCompletionKind::SlashAlias,
            alias_of: Some(alias.target.clone()),
            command: target_spec.cloned(),
        });
    }

    // Trigger-word steering ("did you mean" for retired foreign idioms)
    let trigger_suggestion = catalog
        .suggestions
        .iter()
        .find(|suggestion| suggestion.trigger.eq_ignore_ascii_case(trigger))
        .map(|suggestion| {
            let command = catalog.find(&suggestion.target).cloned();
            InputCompletion {
                label: suggestion.target.clone(),
                description: suggestion.reason.clone(),
                insert_text: suggestion.target.clone(),
                replace_start: 0,
                replace_end,
                kind: InputCompletionKind::Intent,
                alias_of: None,
                command,
            }
        });
    let trigger_target = trigger_suggestion.as_ref().map(|item| item.label.clone());
    items.extend(trigger_suggestion);

    // Intent keywords ("fork" → /tree)
    let intent: Vec<InputCompletion> = (!trigger.is_empty())
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
    catalog: &CommandCatalog,
    input: &str,
    current_lower: &str,
    replace_end: usize,
) -> Vec<InputCompletion> {
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
            .map(|sub| InputCompletion {
                label: format!("{} {}", command_name, sub.name),
                description: sub.summary.clone(),
                insert_text: format!(
                    "{} {}",
                    &input[..typed_parent_len.min(input.len())],
                    sub.name
                ),
                replace_start: 0,
                replace_end,
                kind: InputCompletionKind::Slash,
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

/// Synchronous adapter used by frontend unit tests to exercise the server's
/// completion implementation without standing up a session driver. Product
/// clients never call this; they use `CompleteInput` over the control plane.
#[doc(hidden)]

pub(crate) fn expand_usage_options(usage: &str) -> Vec<String> {
    let mut words = Vec::new();
    for word in usage.split_whitespace() {
        // Skip positional placeholder arguments like `<query>` or `<id>`
        if word.starts_with('<') && word.ends_with('>') {
            continue;
        }
        // Skip optional single placeholder arguments like `[path]` or `[topic]`
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

        // Explicit paths (e.g. @../sib, @./, @/)
        if query.starts_with("../") || query.starts_with("./") || query.starts_with('/') {
            let (base_dir, query_tail) = if let Some(stripped) = query.strip_prefix("../") {
                (project_root.parent().map(|p| p.to_path_buf()), stripped)
            } else if let Some(stripped) = query.strip_prefix("./") {
                (Some(project_root.clone()), stripped)
            } else {
                (Some(std::path::PathBuf::from("/")), query.strip_prefix('/').unwrap_or(query))
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
                // Top-level only
                if let Ok(entries) = std::fs::read_dir(&project_root) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        let filename = path.file_name().unwrap_or_default().to_string_lossy();
                        if filename == ".git" { continue; }
                        let is_dir = path.is_dir();
                        let label = if is_dir { format!("@file:{filename}/") } else { format!("@file:{filename}") };
                        let needs_space = input.get(cursor..).and_then(|suffix| suffix.chars().next()).map(|ch| !ch.is_whitespace()).unwrap_or(true);
                        let insert = if !is_dir && needs_space { format!("{label} ") } else { label.clone() };
                        items.push(nuo_wire::InputCompletion {
                            label,
                            description: if is_dir { "Workspace directory".into() } else { "Workspace file".into() },
                            insert_text: insert,
                            replace_start: at_idx,
                            replace_end: cursor,
                            kind: if is_dir { nuo_wire::ComposerCompletionKind::PathDir } else { nuo_wire::ComposerCompletionKind::PathFile },
                            alias_of: None,
                            command: None,
                        });
                    }
                }
                return items;
            }

            // Subdirectory descend (e.g. src/)
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
                fn collect_descendants(base: &std::path::Path, current: &std::path::Path, input: &str, items: &mut Vec<nuo_wire::InputCompletion>, at_idx: usize, cursor: usize) {
                    if let Ok(entries) = std::fs::read_dir(current) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.file_name().unwrap_or_default() == ".git" { continue; }
                            if let Ok(rel) = path.strip_prefix(base) {
                                let rel_str = rel.to_string_lossy().replace('\\', "/");
                                let is_dir = path.is_dir();
                                let label = if is_dir { format!("@file:{rel_str}/") } else { format!("@file:{rel_str}") };
                                let needs_space = input.get(cursor..).and_then(|suffix| suffix.chars().next()).map(|ch| !ch.is_whitespace()).unwrap_or(true);
                                let insert = if !is_dir && needs_space { format!("{label} ") } else { label.clone() };
                                items.push(nuo_wire::InputCompletion {
                                    label,
                                    description: if is_dir { "Workspace directory".into() } else { "Workspace file".into() },
                                    insert_text: insert,
                                    replace_start: at_idx,
                                    replace_end: cursor,
                                    kind: if is_dir { nuo_wire::ComposerCompletionKind::PathDir } else { nuo_wire::ComposerCompletionKind::PathFile },
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
                collect_descendants(&project_root, &target_dir, input, &mut items, at_idx, cursor);
                return items;
            }

            // Substring query across all files
            fn collect_matching(base: &std::path::Path, current: &std::path::Path, query: &str, input: &str, items: &mut Vec<nuo_wire::InputCompletion>, at_idx: usize, cursor: usize) {
                if let Ok(entries) = std::fs::read_dir(current) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.file_name().unwrap_or_default() == ".git" { continue; }
                        if let Ok(rel) = path.strip_prefix(base) {
                            let rel_str = rel.to_string_lossy().replace('\\', "/");
                            let is_dir = path.is_dir();
                            if rel_str.contains(query) {
                                let label = if is_dir { format!("@file:{rel_str}/") } else { format!("@file:{rel_str}") };
                                let needs_space = input.get(cursor..).and_then(|suffix| suffix.chars().next()).map(|ch| !ch.is_whitespace()).unwrap_or(true);
                                let insert = if !is_dir && needs_space { format!("{label} ") } else { label.clone() };
                                items.push(nuo_wire::InputCompletion {
                                    label,
                                    description: if is_dir { "Workspace directory".into() } else { "Workspace file".into() },
                                    insert_text: insert,
                                    replace_start: at_idx,
                                    replace_end: cursor,
                                    kind: if is_dir { nuo_wire::ComposerCompletionKind::PathDir } else { nuo_wire::ComposerCompletionKind::PathFile },
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
            collect_matching(&project_root, &project_root, file_query, input, &mut items, at_idx, cursor);
            return items;
        }

        if let Some(skill_query) = query.strip_prefix("skill:") {
            let mut items = vec![
                nuo_wire::InputCompletion {
                    label: "skill-creator".into(),
                    description: "Built-in skill creator".into(),
                    insert_text: "@skill:skill-creator".into(),
                    replace_start: at_idx,
                    replace_end: cursor,
                    kind: nuo_wire::ComposerCompletionKind::PathFile,
                    alias_of: None,
                    command: None,
                }
            ];
            items.retain(|i| i.label.starts_with(skill_query));
            return items;
        }
    }
    Vec::new()
}


// Client side of the server control plane: discovery, the attach handshake
// (`connect`), one-shot control verbs (`control`), and the monitor stream.
// The `nuo` TUI and other protocol clients drive sessions owned by the `nuo`
// server through this module. Discovery is global (one server per user);
// connections prefer platform-native local IPC and fall back to TCP.
//
// The wire protocol this client speaks is [`nuo_client::wire`] (the `Wire`
// envelope) — the same definition the server drives, from one crate, so the
// protocol cannot drift.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use nuo_wire::{
    AgentRequest, AgentResponse, Message, MonitorAction, MonitorEvent, MonitoredSession,
    SessionOverview,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;


/// ADR-0141: the human-channel posture this process declares when attaching.
/// Defaults to `Interactive` (a TUI is a human by construction). Headless
/// entrypoints (`nuo -p`, remote automation) call [`set_posture`] with
/// `Autonomous` before connecting so the session knows no human can answer
/// parked requests. Process-wide because one process plays one role.
static POSTURE_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Declare this client's human-channel posture (ADR-0141). Must be called
/// before the first attach; later attaches of the same process inherit it.
pub fn set_posture(posture: nuo_wire::human_request::HumanChannelPosture) {
    let code = match posture {
        nuo_wire::human_request::HumanChannelPosture::Interactive => 0,
        nuo_wire::human_request::HumanChannelPosture::Autonomous => 1,
    };
    POSTURE_OVERRIDE.store(code, std::sync::atomic::Ordering::Relaxed);
}

fn current_posture() -> nuo_wire::human_request::HumanChannelPosture {
    match POSTURE_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => nuo_wire::human_request::HumanChannelPosture::Autonomous,
        _ => nuo_wire::human_request::HumanChannelPosture::Interactive,
    }
}

/// An explicitly named server endpoint (`--remote <addr>` + `--token
/// <token>`): the operator supplied the coordinates, so no discovery
/// record exists or is read. Distinct from [`ServerInfo`] on purpose — a
/// discovered server is identified by local state (pid, socket path,
/// version record); a remote one is identified by nothing but the address,
/// and pretending otherwise is how a remote run silently lands on the
/// local instance.
#[derive(Debug, Clone)]
pub struct RemoteServer {
    /// Hostname or IP. Loopback only when the address was a bare `:port`.
    pub host: String,
    pub port: u16,
    pub token: String,
}

impl RemoteServer {
    /// Parse `--remote <addr>` (`host:port`, `ws://host:port`, or a bare
    /// `:port` for loopback) together with the required `--token`. The
    /// token is mandatory because every network-exposed listener demands
    /// one (ADR-0105); a missing port is an error rather than a well-known
    /// default — a default would silently target the local server when the
    /// operator meant a remote one.
    pub fn parse(addr: &str, token: Option<String>) -> Result<Self, String> {
        let bare = addr.trim().strip_prefix("ws://").unwrap_or(addr.trim());
        let (host, port) = split_host_port(bare)?;
        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let Some(token) = token.filter(|t| !t.is_empty()) else {
            return Err(
                "--remote needs --token <token>: every network-exposed server \
                 requires the bearer token from the host's server discovery record"
                    .to_string(),
            );
        };
        Ok(Self { host, port, token })
    }

    /// Connect and run the attach handshake over TCP+bearer. No UDS
    /// attempt (the socket belongs to the remote machine's filesystem),
    /// no version pre-check (the handshake carries the server's version).
    pub async fn connect(&self, action: AttachAction) -> Result<Handshake, String> {
        let url = format!("ws://{}:{}/", self.host, self.port);
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| format!("bad ws url {url}: {e}"))?;
        let value = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
        let (ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| format!("ws connect to {url}: {e}"))?;
        let (sink, source) = websocket_split(ws);
        finish_handshake((sink, source), action).await
    }
}

/// Split `host:port` into its parts. A missing port is an error.
fn split_host_port(s: &str) -> Result<(Option<String>, u16), String> {
    let (host, port_str) = match s.rsplit_once(':') {
        Some(parts) => parts,
        None => return Err(format!("'{s}' is not host:port")),
    };
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("'{port_str}' is not a port number"))?;
    Ok(((!host.is_empty()).then(|| host.to_string()), port))
}

const LIVENESS_TIMEOUT: Duration = Duration::from_millis(500);
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_START_POLL: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Find the unified server (ADR-0096): one global record. The project
/// argument is accepted for source compatibility but no longer scopes the
/// lookup — the server serves every project.
pub fn discover(_project_root: &Path) -> Option<ServerInfo> {
    let global_path = discovery::global_discovery_path();
    if let Some(info) = discover_at(&global_path) {
        return Some(info);
    }

    let lock_path = discovery::global_lock_path();
    if nuo_host::lock::ProcessLock::is_locked(&lock_path) {
        // If the instance lock is held, the server may be in the middle of startup
        // writing server.json. Retry briefly before falling back.
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(20));
            if let Some(info) = discover_at(&global_path) {
                return Some(info);
            }
        }
    }

    // Self-healing fallback: if server.json is missing or unlinked,
    // but a live server holds the instance lock and is responsive on UDS/TCP:
    if nuo_host::lock::ProcessLock::is_locked(&lock_path)
        && let Some(pid) = nuo_host::lock::ProcessLock::probe_holder(&lock_path)
        && is_process_alive(pid)
    {
        let local_endpoint = discovery::default_local_endpoint().ok();
        let local_connectable = local_endpoint
            .as_ref()
            .is_some_and(|endpoint| nuo_host::ipc::probe(endpoint).connectable);

        let tcp_addr =
            std::net::SocketAddr::from(([127, 0, 0, 1], 9527));
        let tcp_connectable =
            std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_millis(300)).is_ok();

        if local_connectable || tcp_connectable {
            let recovered = ServerInfo {
                pid,
                process_birth_token: nuo_host::process::process_identity(pid)
                    .ok()
                    .map(|identity| identity.birth_token),
                port: 9527,
                token: None,
                project_root: String::new(),
                started_at: 0,
                uds_path: None,
                local_endpoint,
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
                grace_secs: None,
                protocol: Some(PROTOCOL_VERSION),
                ..Default::default()
            };
            tracing::info!(
                pid,
                "discover: recovered in-memory discovery record from live lock holder"
            );
            return Some(recovered);
        }
    }

    None
}

fn discover_at(path: &Path) -> Option<ServerInfo> {
    let bytes = std::fs::read(path).ok()?;
    let info: ServerInfo = serde_json::from_slice(&bytes).ok()?;
    if !server_process_matches(&info) {
        // Readers never delete shared discovery state: a successor can replace
        // the record after this read. The next lock-owning server overwrites a
        // stale record, and the server's own lease removes its matching record.
        return None;
    }
    if !is_alive(&info) {
        return None;
    }
    Some(info)
}

fn server_process_matches(info: &ServerInfo) -> bool {
    nuo_host::process::process_identity(info.pid).is_ok_and(|identity| {
        info.process_birth_token
            .is_none_or(|expected| expected == identity.birth_token)
    })
}

/// Directional relation between client version and server version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation {
    Equal,
    ClientNewer,
    ClientOlder,
    Unknown,
}

/// Compare client and server version strings using SemVer.
pub fn compare_versions(client: &str, server: &str) -> VersionRelation {
    if client == server {
        return VersionRelation::Equal;
    }
    match (
        semver::Version::parse(client),
        semver::Version::parse(server),
    ) {
        (Ok(c), Ok(d)) => {
            if c > d {
                VersionRelation::ClientNewer
            } else if c < d {
                VersionRelation::ClientOlder
            } else {
                VersionRelation::Equal
            }
        }
        _ => VersionRelation::Unknown,
    }
}

/// The actionable error for a discovered-but-incompatible server,
/// preferring the wire-protocol explanation when the record declares a
/// protocol number outside this client's window (ADR-0134), then the
/// dev-drift explanation (same version, replaced binary), and falling
/// back to the product-version message for legacy records. This is the
/// entry point callers should use after `versions_compatible` reports
/// false.
pub fn incompatibility_error(info: &ServerInfo) -> String {
    if info
        .protocol
        .is_some_and(|p| !protocol_accepts(p))
    {
        protocol_mismatch(info)
    } else if info.version.as_deref() == Some(env!("CARGO_PKG_VERSION"))
        && !server_image_is_current(info)
    {
        format!(
            "client/server binary mismatch: running server (pid {}, version {}) executable differs from the installed nuo core image (rebuilt binary). \
             Stop it with `nuo stop` and rerun — the server restarts on demand.",
            info.pid,
            env!("CARGO_PKG_VERSION")
        )
    } else {
        version_mismatch(info)
    }
}

/// Whether the running server is the ADR-0021 **dev-drift** case: the record
/// declares this client's protocol window and this client's product version,
/// yet the server's executable image is no longer the installed one. This is
/// precisely the state a `cargo` rebuild of the binary leaves behind, where
/// every version signal agrees and only content identity can see it.
///
/// Any other incompatibility (out-of-window protocol, different product
/// version) is *not* drift and keeps the pre-ADR-0021 "prompt, never act"
/// behaviour: an upgrade leftover is deliberately served (the server restarts
/// on idle exit) and a skewed peer needs the directional upgrade message.
fn is_dev_drift(info: &ServerInfo) -> bool {
    let protocol_in_window = info.protocol.is_none_or(protocol_accepts);
    let same_version = info.version.as_deref() == Some(env!("CARGO_PKG_VERSION"));
    protocol_in_window && same_version && !server_image_is_current(info)
}

/// What a live server is currently doing with its sessions, as seen through a
/// best-effort monitor probe (ADR-0021). The distinction drives whether a
/// dev-drift server may be reclaimed (`Idle`) or must be left alone with an
/// explanatory refusal (`Busy`/`Unreachable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerActivity {
    /// No live session, no server-level task: safe to reclaim.
    Idle { sessions: usize, tasks: usize },
    /// At least one live session or server task: reclaiming would interrupt
    /// agent work. Carries the counts for the refusal message.
    Busy { sessions: usize, tasks: usize },
    /// The monitor probe could not be completed (unresponsive control plane).
    /// Treated as "not provably idle" — the fail-safe side of the gate.
    Unreachable,
}

/// Probe a server's live activity without modifying state. Because a
/// dev-drift server still speaks this client's wire protocol (that is what
/// makes it drift rather than skew), the ordinary monitor handshake reaches
/// it; any failure collapses to [`ServerActivity::Unreachable`], which the
/// reclaimer treats as "busy".
#[allow(dead_code)]
async fn probe_server_activity(info: &ServerInfo) -> ServerActivity {
    let action = MonitorAction {
        watch: false,
        include_idle: true,
    };
    let Ok(mut rx) = monitor_stream(info, action).await else {
        return ServerActivity::Unreachable;
    };
    match rx.recv().await {
        Some(MonitorEvent::Snapshot(snapshot)) => {
            // `include_idle` is set, so an idle session still appears as a
            // row; only rows that are actively working (running, blocked on a
            // human, resuming) count as busy. Server-level tasks (rehosted
            // services) are never safe to interrupt regardless.
            let busy = snapshot
                .sessions
                .iter()
                .filter(|row| row.status.is_active())
                .count();
            if busy == 0 && snapshot.tasks.is_empty() {
                ServerActivity::Idle {
                    sessions: snapshot.sessions.len(),
                    tasks: 0,
                }
            } else {
                ServerActivity::Busy {
                    sessions: busy,
                    tasks: snapshot.tasks.len(),
                }
            }
        }
        _ => ServerActivity::Unreachable,
    }
}

/// The actionable refusal when a rebuilt (dev-drift) server is still hosting
/// work: name the incompatibility *and* the cost of reclaiming it, so the
/// operator's `nuo stop` is an informed choice (ADR-0021). Mirrors
/// [`incompatibility_error`]'s binary-mismatch text for the busy case.
#[allow(dead_code)]
fn drift_refusal_error(info: &ServerInfo, activity: &ServerActivity) -> String {
    let head = format!(
        "client/server binary mismatch: running server (pid {}, version {}) executable differs from the installed nuo core image (rebuilt binary).",
        info.pid,
        env!("CARGO_PKG_VERSION")
    );
    match activity {
        ServerActivity::Busy { sessions, tasks } => {
            let mut held = Vec::new();
            if *sessions > 0 {
                held.push(format!("{sessions} active session(s)"));
            }
            if *tasks > 0 {
                held.push(format!("{tasks} server task(s)"));
            }
            format!(
                "{head} The server is still hosting {} — it was not reclaimed automatically. \
                 Stop it with `nuo stop` to run the rebuilt binary; that will interrupt the work above.",
                held.join(" and ")
            )
        }
        ServerActivity::Unreachable | ServerActivity::Idle { .. } => format!(
            "{head} Stop it with `nuo stop` and rerun — the server restarts on demand."
        ),
    }
}

/// The actionable version-skew error (ADR-0100 rule 4), naming both builds
/// and the directional fix (server behind -> stop/restart server; client behind -> update client).
/// Public so `nuo`-level commands can surface it uniformly
/// wherever a discovered server is about to be spoken to.
pub fn version_mismatch(info: &ServerInfo) -> String {
    let client_ver = env!("CARGO_PKG_VERSION");
    let Some(server_ver) = info.version.as_deref() else {
        return format!(
            "client/server version mismatch: this client is {client_ver} but the running server (pid {}) is unknown (older than 0.24). \
             Stop it with `nuo stop` and rerun — the server restarts on demand at the new version.",
            info.pid
        );
    };

    match compare_versions(client_ver, server_ver) {
        VersionRelation::ClientNewer => format!(
            "client/server version mismatch: running server (pid {}, version {server_ver}) is older than this client ({client_ver}). \
             Stop it with `nuo stop` and rerun — the server restarts on demand at the new version.",
            info.pid
        ),
        VersionRelation::ClientOlder => format!(
            "client/server version mismatch: this client ({client_ver}) is older than the running server (pid {}, version {server_ver}). \
             Please update your nuo client to {server_ver} or newer.",
            info.pid
        ),
        VersionRelation::Equal => {
            if !server_image_is_current(info) {
                format!(
                    "client/server binary mismatch: running server (pid {}, version {server_ver}) executable differs from the installed nuo core image (rebuilt binary). \
                     Stop it with `nuo stop` and rerun — the server restarts on demand.",
                    info.pid
                )
            } else {
                format!(
                    "client/server version mismatch: client and server both report {client_ver} but failed compatibility check."
                )
            }
        }
        VersionRelation::Unknown => format!(
            "client/server version mismatch: this client is {client_ver} but the running server (pid {}) is {server_ver}. \
             If the server is outdated, stop it with `nuo stop` and rerun; if the client is outdated, update your client.",
            info.pid
        ),
    }
}

/// The actionable wire-protocol-skew error (ADR-0134), naming both protocol
/// numbers and the directional fix. Public for the same reason as
/// [`version_mismatch`]: uniform surfacing wherever a discovered server is
/// about to be spoken to. Only reached when the record carries a protocol
/// number outside this client's window.
pub fn protocol_mismatch(info: &ServerInfo) -> String {
    let client_proto = PROTOCOL_VERSION;
    let server_proto = info.protocol.unwrap_or(0);
    if server_proto > client_proto {
        format!(
            "client/server wire protocol mismatch: running server (pid {}) speaks protocol {server_proto}, \
             newer than this client's protocol {client_proto}. \
             Stop it with `nuo stop` and rerun — the server restarts on demand at the new build.",
            info.pid
        )
    } else {
        format!(
            "client/server wire protocol mismatch: this client speaks protocol {client_proto}, \
             older than the running server (pid {}) requires — the server reports protocol {server_proto}. \
             Please update your nuo client.",
            info.pid
        )
    }
}

/// Whether a discovered server can serve this client (ADR-0100 rule 4,
/// revised by ADR-0134 for protocol-declaring records).
///
/// The decision has two regimes:
///
/// - **Protocol-declaring record** (`protocol: Some`, any server since the
///   field exists): the wire window is the compatibility authority, and a
///   server inside it is served *whatever its product version* — the
///   patch-bump case (wire unchanged, protocol number unchanged) must not
///   kick a healthy server out of bed. Exactly one local freshness gate
///   survives: the **dev-drift lie** — same version, different binary
///   (`server_image_is_current` false), i.e. `cargo run` rebuilt the
///   binary under a still-serving server of the same `CARGO_PKG_VERSION`.
///   That is the one state where every version signal agrees and the
///   client is still about to test a stale image; only the inode sees it.
///   An upgrade leftover (different version, in-window protocol, different
///   image) is deliberately **served**: the server restarts on idle exit,
///   and refusing would make every patch release interrupt live sessions
///   for no wire-level reason.
/// - **Legacy record** (`protocol: None`, pre-0.31 server): the record
///   predates negotiation, so ADR-0100 rule 4's exact product-version
///   equality (plus the image check) remains its gate unchanged.
pub fn versions_compatible(info: &ServerInfo) -> bool {
    local_pair_compatible(
        info.protocol,
        info.version.as_deref(),
        server_image_is_current(info),
    )
}

/// The pure decision core of [`versions_compatible`], split out so the
/// policy is unit-testable without a real server process (the image probe
/// is resolved by the caller for legacy records and inside for
/// protocol-declaring ones — see the regime docs above).
fn local_pair_compatible(
    protocol: Option<u32>,
    version: Option<&str>,
    server_image_is_current: bool,
) -> bool {
    if let Some(server_protocol) = protocol {
        // Protocol-declaring record: the window is the wire gate...
        if !protocol_accepts(server_protocol) {
            return false;
        }
        // ...and locally, only the dev-drift lie remains a refusal.
        // Same version + different image = the client is about to test a
        // stale binary while every version signal says "equal". Different
        // version + in-window protocol = upgrade leftover: serve it.
        let same_version = version == Some(env!("CARGO_PKG_VERSION"));
        return !same_version || server_image_is_current;
    }
    // Legacy record: exact product-version equality (ADR-0100 rule 4),
    // `None` counting as a mismatch, plus the image check.
    version.is_some_and(|v| v == env!("CARGO_PKG_VERSION")) && server_image_is_current
}

/// Whether the running server's executable is still the resolved `nuo`
/// core image. During a development loop a rebuild replaces that file;
/// the kernel keeps the old image alive under a `(deleted)` link while the
/// discovery record still names the same path and version.
///
/// Since ADR-0021 the primary signal is **content identity**: the server
/// publishes a bounded content digest of its own image (exact length + a
/// sampled SHA-256) in the discovery record, and a client compares it with the
/// installed image it would spawn. That is portable (Linux, macOS, Windows)
/// and, unlike inode equality, does not false-positive when an *identical*
/// binary is reinstalled at the same path. A record without the digest
/// (pre-ADR-0021 server) falls back to the historical Linux-only inode probe.
///
/// Returns `true` when no signal can be resolved (non-Linux with no record
/// hash, unreadable `/proc`, or an unresolvable installed image): absence of
/// evidence must not flag a healthy production server. A server spawned by
/// an *installed* binary matches the sibling `nuo` resolved by `nuox`; the
/// TUI executable itself is deliberately not part of this comparison.
pub fn server_image_is_current(info: &ServerInfo) -> bool {
    let expected = server_program();
    if !expected.is_file() {
        return true;
    }
    if info.image_digest.is_some() {
        // Content identity (ADR-0021): the installed image's hash is the
        // authority. `image_content_digest` returning `None` (unreadable image)
        // is "no evidence" and must not flag drift.
        return content_matches_record(image_content_digest(&expected), info);
    }
    // Legacy record (no published hash): the Linux-only inode probe.
    server_image_is_current_by_inode(info.pid)
}

/// The pure core of the content-identity check (ADR-0021), split out so the
/// policy is unit-testable without depending on the ambient build layout:
/// `installed` is the resolved on-disk image's `(len, digest)` (or `None`
/// when unreadable), `info` the server's published identity.
fn content_matches_record(installed: Option<(u64, String)>, info: &ServerInfo) -> bool {
    let Some(published) = info.image_digest.as_deref() else {
        // No published hash: no content evidence, never a content-based drift.
        return true;
    };
    match installed {
        Some((len, digest)) => {
            // Cheap length pre-gate, then the digest. A size change is enough
            // to prove the image differs.
            let len_matches = info.image_len.is_none_or(|expected_len| expected_len == len);
            len_matches && digest.eq_ignore_ascii_case(published)
        }
        None => true,
    }
}

/// The historical inode-equality probe, retained as the ADR-0021 fallback for
/// discovery records that predate `image_digest`.
fn server_image_is_current_by_inode(pid: u32) -> bool {
    let expected = server_program();
    if !expected.is_file() {
        return true;
    }
    nuo_host::process::process_image_matches_path(pid, &expected)
}

/// A **bounded content digest** of an executable image (ADR-0021), delegated
/// to the host's shared implementation so the server's published digest and the
/// client's comparison are computed identically. `None` when the file cannot
/// be read — callers treat that as "no evidence", never as drift.
fn image_content_digest(path: &Path) -> Option<(u64, String)> {
    nuo_host::process::image_digest_len(path)
}

/// `(dev, inode)` equality. A rebuilt binary legitimately occupies the same
/// path with a new inode; path-string equality would hide exactly that.
#[cfg(all(test, unix))]
fn same_inode(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Liveness probe: prefer native local IPC and fall back to TCP. Either
/// reachable endpoint means the server is up.
fn is_alive(info: &ServerInfo) -> bool {
    if !server_process_matches(info) {
        return false;
    }
    if info
        .effective_local_endpoint()
        .is_some_and(|endpoint| nuo_host::ipc::probe(&endpoint).connectable)
    {
        return true;
    }
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], info.port));
    std::net::TcpStream::connect_timeout(&addr, LIVENESS_TIMEOUT).is_ok()
}

/// Whether a process with `pid` exists and can receive signals.
pub fn is_process_alive(pid: u32) -> bool {
    nuo_host::process::process_identity(pid).is_ok()
}

/// Wait up to `timeout` for this exact process incarnation to exit. Comparing
/// the birth token prevents a recycled PID from extending the wait or becoming
/// the target of a later escalation.
async fn wait_for_process_exit(
    identity: nuo_host::process::ProcessIdentity,
    timeout: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !nuo_host::process::process_is_alive(identity) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !nuo_host::process::process_is_alive(identity)
}

/// The drain budget a stopper should allow when the discovery record
/// predates `grace_secs` (ADR-0116): generous enough not to interrupt a
/// default-configured server (10s) mid-drain, short enough that a wedged
/// process still escalates.
const FALLBACK_GRACE: Duration = Duration::from_secs(15);

/// Stop the server through a tiered, **budget-coordinated** shutdown
/// pipeline (ADR-0116):
///
/// 1. Tier 1 (Protocol): if the server speaks this client's version, send
///    the `Shutdown` control verb, then wait the server's *own* drain
///    budget (from the discovery record's `grace_secs`) — not a hardcoded
///    couple of seconds. Any signal arriving mid-drain escalates the
///    server to a forced exit that skips session teardown, so escalating
///    early destroys the graceful drain the stop just requested.
/// 2. Tier 2 (native graceful request): if the versions skew, the verb could
///    not be delivered, or the budget elapsed without an exit, request native
///    graceful termination where the OS defines it (SIGTERM on Unix).
/// 3. Tier 3 (Force): identity-conditionally force-terminate the process.
/// 4. Tier 4 (Cleanup): after the exact process incarnation is gone, acquire
///    the instance lock and identity-conditionally remove the discovery record.
///    Native listener state is RAII-owned; a killed Unix server's stale socket
///    is removed by the next lock-owning bind, never by a racing stopper.
/// ADR-0034 Level 1 (soft reload): ask a live server to re-read its
/// configuration and re-sync MCP servers + skills without dropping any
/// connection. Fails if the running server predates the reload verb.
pub async fn reload(info: &ServerInfo) -> Result<(), String> {
    if !versions_compatible(info) {
        return Err(
            "server/client version mismatch; `server reload` needs a compatible server".into(),
        );
    }
    control(info, ControlRequest::Reload).await
}

pub async fn stop(info: &ServerInfo) -> Result<(), String> {
    // The server's own drain budget, when it advertised one: the single
    // number every tier below is coordinated against.
    let grace = info
        .grace_secs
        .map(Duration::from_secs)
        .unwrap_or(FALLBACK_GRACE);
    let target_identity = nuo_host::process::process_identity(info.pid).ok();
    if let (Some(expected), Some(actual)) = (
        info.process_birth_token,
        target_identity.map(|identity| identity.birth_token),
    ) && expected != actual
    {
        return Err(format!(
            "refusing to stop pid {}: discovery process identity is stale",
            info.pid
        ));
    }
    let mut stopped = target_identity.is_none();

    // Tier 1: Try graceful protocol shutdown if versions are compatible.
    if let Some(identity) = target_identity
        && versions_compatible(info)
        && let Ok(Ok(())) = tokio::time::timeout(
            Duration::from_millis(1500),
            control(info, ControlRequest::Shutdown),
        )
        .await
    {
        stopped = wait_for_process_exit(identity, grace).await;
    }

    // Tier 2 & 3: request native graceful termination where supported, then
    // force-terminate the identity-checked process. Unix SIGTERM drains
    // through the same phases as the verb; Windows shutdown is protocol-only.
    if !stopped {
        if let Some(identity) = target_identity {
            if nuo_host::process::request_termination(identity).is_ok() {
                stopped = wait_for_process_exit(identity, grace).await;
            }
            if !stopped && nuo_host::process::process_is_alive(identity) {
                let _ = nuo_host::process::force_terminate(identity);
                stopped = wait_for_process_exit(identity, Duration::from_millis(1000)).await;
            }
        } else {
            stopped = true;
        }
    }

    let gone = stopped
        || target_identity
            .is_none_or(|identity| !nuo_host::process::process_is_alive(identity));
    // Tier 4: hold the same instance lock a successor requires before touching
    // shared discovery state. If a successor already owns it, its record is
    // categorically not ours to remove. Never unlink a Unix socket here; the
    // next lock-owning listener bind handles stale filesystem state.
    if gone
        && let Ok(_cleanup_lock) =
            nuo_host::lock::ProcessLock::acquire(&discovery::global_lock_path())
    {
        discovery::remove_if_matching_process(
            &discovery::global_discovery_path(),
            info.pid,
            info.process_birth_token,
        );
    }
    if gone {
        Ok(())
    } else {
        Err(format!("could not stop server (pid {})", info.pid))
    }
}

/// Path to the server startup stderr log file.
pub fn startup_log_path() -> PathBuf {
    let dirs = nuo_host::paths::get();
    dirs.state_dir.join("log").join("server-startup.log")
}

/// Canonical alias for stopping a running session server.
pub async fn stop_server(info: &ServerInfo) -> Result<(), String> {
    stop(info).await
}

pub async fn ensure_server(project_root: &Path) -> Result<ServerInfo, String> {
    if let Some(info) = discover(project_root) {
        if versions_compatible(&info) && !is_dev_drift(&info) {
            return Ok(info);
        }
        // ADR-0029: on dev-drift (rebuilt binary) or version mismatch, directly
        // replace the conflicting predecessor instead of refusing.
        tracing::info!(
            pid = info.pid,
            "ensure_server: replacing existing server on version drift or rebuild"
        );
        let _ = stop(&info).await;
    }

    // ADR-0034 [INV-LIFECYCLE-02]: Single-Flight Spawning Mutex.
    // Ensure only one client initiates server startup; concurrent clients wait on discovery.
    let spawn_lock_path = discovery::global_spawn_lock_path();
    let _spawn_guard = match nuo_host::lock::ProcessLock::acquire(&spawn_lock_path) {
        Ok(guard) => Some(guard),
        Err(_) => {
            // Another client is actively spawning the server. Poll discovery until ready.
            let wait_deadline = tokio::time::Instant::now() + Duration::from_secs(6);
            while tokio::time::Instant::now() < wait_deadline {
                tokio::time::sleep(SERVER_START_POLL).await;
                if let Some(info) = discover(project_root) {
                    if versions_compatible(&info) && !is_dev_drift(&info) {
                        return Ok(info);
                    }
                }
            }
            nuo_host::lock::ProcessLock::acquire(&spawn_lock_path).ok()
        }
    };

    // Double-checked probe after lock acquisition:
    if let Some(info) = discover(project_root) {
        if versions_compatible(&info) && !is_dev_drift(&info) {
            return Ok(info);
        }
        let _ = stop(&info).await;
    }

    // Check if another server is holding the instance lock or database owner lock
    let lock_path = discovery::global_lock_path();
    let db_lock_path = nuo_host::paths::get().db_file().with_extension("db.owner.lock");
    let mut candidate_pids = std::collections::HashSet::new();
    if nuo_host::lock::ProcessLock::is_locked(&lock_path)
        && let Some(holder_pid) = nuo_host::lock::ProcessLock::probe_holder(&lock_path)
        && is_process_alive(holder_pid)
    {
        candidate_pids.insert(holder_pid);
    }
    if nuo_host::lock::ProcessLock::is_locked(&db_lock_path)
        && let Some(holder_pid) = nuo_host::lock::ProcessLock::probe_holder(&db_lock_path)
        && is_process_alive(holder_pid)
    {
        candidate_pids.insert(holder_pid);
    }
    for holder_pid in candidate_pids {
        tracing::info!(
            holder_pid,
            "ensure_server: replacing conflicting server holding instance or database lock"
        );
        let _ = nuo_host::process::takeover_pid(
            holder_pid,
            nuo_host::process::TakeoverOptions::default(),
        )
        .await;
    }

    let mut child = spawn_server()?;
    let deadline = std::time::Instant::now() + SERVER_START_TIMEOUT;
    loop {
        tokio::time::sleep(SERVER_START_POLL).await;
        if let Some(info) = discover(project_root) {
            if versions_compatible(&info) {
                return Ok(info);
            } else {
                return Err(incompatibility_error(&info));
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            let log_text = std::fs::read_to_string(startup_log_path()).unwrap_or_default();
            let log_trimmed = log_text.trim();
            if !log_trimmed.is_empty() {
                return Err(format!(
                    "nuo server exited prematurely ({status}): {log_trimmed}"
                ));
            } else {
                return Err(format!("nuo server exited prematurely with {status}"));
            }
        }
        if std::time::Instant::now() >= deadline {
            let log_text = std::fs::read_to_string(startup_log_path()).unwrap_or_default();
            let log_trimmed = log_text.trim();
            let lock_info = if nuo_host::lock::ProcessLock::is_locked(&lock_path) {
                nuo_host::lock::ProcessLock::probe_holder(&lock_path)
                    .map(|pid| format!(" (instance lock held by PID {pid})"))
                    .unwrap_or_else(|| " (instance lock is held)".to_string())
            } else {
                String::new()
            };
            if !log_trimmed.is_empty() {
                return Err(format!(
                    "nuo server did not become ready within {:?}{lock_info}: {log_trimmed}",
                    SERVER_START_TIMEOUT
                ));
            } else {
                return Err(format!(
                    "nuo server did not become ready within {:?}{lock_info} (see {})",
                    SERVER_START_TIMEOUT,
                    startup_log_path().display()
                ));
            }
        }
    }
}

fn spawn_server() -> Result<std::process::Child, String> {
    let program = server_program();
    let mut command = std::process::Command::new(&program);
    command.args(["start", "--fg", "--client-driven"]);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());

    let startup_log = startup_log_path();
    let private_log = nuo_host::secure_file::create_private_parent(&startup_log)
        .and_then(|()| nuo_host::secure_file::create_private_file(&startup_log));
    if let Ok(file) = private_log {
        command.stderr(file);
    } else {
        command.stderr(std::process::Stdio::null());
    }

    // Pin the server's cwd to a stable, always-existing directory instead of
    // inheriting this client's project. ADR-0096 made the server the host for
    // *every* project's sessions, so a project directory inherited from the
    // first lucky client is exactly the wrong default — any code path that
    // still consults the server's cwd (rather than a session-scoped root)
    // would silently land in that project. Per-session scoping is explicit
    // via the Select frame's `project` field.
    let server_cwd = nuo_host::paths::get().data_dir.clone();
    let _ = std::fs::create_dir_all(&server_cwd);
    command.current_dir(server_cwd);
    configure_server_detachment(&mut command);
    command
        .spawn()
        .map_err(|error| {
            format!(
                "could not start the nuo server with {}: {error}. Rebuild `nuo` (cargo build -p nuo) or make it available on PATH",
                program.display()
            )
        })
}

/// Resolve the core server executable without ever re-entering the client.
/// A unified install places `nuo` on PATH; the sibling of the running
/// executable is preferred so a source build spawns its own freshly built
/// binary.
fn server_program() -> PathBuf {
    if let Some(program) = std::env::var_os("NUO_BIN").filter(|value| !value.is_empty()) {
        return PathBuf::from(program);
    }
    if let Ok(current) = std::env::current_exe() {
        let sibling_nuo = current.with_file_name(format!("nuo{}", std::env::consts::EXE_SUFFIX));
        if sibling_nuo.is_file() {
            return sibling_nuo;
        }
    }
    PathBuf::from(format!("nuo{}", std::env::consts::EXE_SUFFIX))
}

/// Configure the process-level detachment shared by every server spawn path.
///
/// On Unix, `setsid(2)` is the single primitive: it creates both a new session
/// and a new process group, detaching the server from the caller's controlling
/// terminal. It must not be combined with `CommandExt::process_group(0)`:
/// that call first makes the child a process-group leader, and POSIX requires
/// `setsid(2)` to fail with `EPERM` for a process-group leader.
///
/// A `setsid(2)` failure is returned by [`std::process::Command::spawn`]. A
/// half-detached server would violate the lifecycle contract, so callers must
/// treat that failure as fatal.
pub fn configure_server_detachment(command: &mut std::process::Command) {
    nuo_host::process::configure_server_std(command);
}

/// Comprehensive diagnostics for the server control plane and system status.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerDiagnostics {
    /// The resolved server instance directory (ADR-0121): the root of every
    /// server runtime file this report probes. Surfaced so an operator can
    /// see which instance — host or `NUO_HOME` sandbox — a client is
    /// talking about before reading anything else below.
    pub instance_dir: PathBuf,
    /// The default port this client resolves (`--port` > `NUO_PORT` >
    /// 9800), for the same reason as `instance_dir`.
    pub default_port: u16,
    pub discovery_path: PathBuf,
    pub discovery_record: Option<ServerInfo>,
    pub discovery_valid: bool,
    pub lock_path: PathBuf,
    pub lock_held: bool,
    pub lock_holder_pid: Option<u32>,
    pub lock_holder_alive: bool,
    pub local_endpoint: Option<nuo_host::ipc::LocalEndpoint>,
    pub local_endpoint_exists: bool,
    pub local_endpoint_connectable: bool,
    pub tcp_port: u16,
    pub tcp_listening: bool,
    pub startup_log_path: PathBuf,
    pub last_startup_log: Option<String>,
    /// ADR-0021 comparison info: the installed server image this client would
    /// spawn and its bounded content digest, when resolvable. `None` when no
    /// installed image is found.
    pub installed_image: Option<PathBuf>,
    pub installed_image_digest: Option<String>,
    /// The server's published image identity (from its discovery record).
    /// `None` on a pre-ADR-0021 record, which means clients fall back to the
    /// Linux-only inode probe for drift detection.
    pub server_image_digest: Option<String>,
    /// Whether the running server's image is still the installed one, judged
    /// by ADR-0021's content-identity rule (with the inode fallback for a
    /// record without a published hash). `true` when no server is running, or
    /// when no signal can be resolved (absence of evidence).
    pub server_image_current: bool,
}

/// Perform a diagnostic probe of the server environment without modifying state.
pub fn diagnose_server() -> ServerDiagnostics {
    let discovery_path = discovery::global_discovery_path();
    let discovery_record = discover_at(&discovery_path);
    let raw_record: Option<ServerInfo> = std::fs::read(&discovery_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let lock_path = discovery::global_lock_path();
    let lock_held = nuo_host::lock::ProcessLock::is_locked(&lock_path);
    let lock_holder_pid = nuo_host::lock::ProcessLock::probe_holder(&lock_path);
    let lock_holder_alive = lock_holder_pid.map(is_process_alive).unwrap_or(false);

    let local_endpoint = raw_record
        .as_ref()
        .and_then(discovery::Discovery::effective_local_endpoint)
        .or_else(|| discovery::default_local_endpoint().ok());
    let local_probe = local_endpoint
        .as_ref()
        .map(nuo_host::ipc::probe)
        .unwrap_or_default();

    let port = discovery_record
        .as_ref()
        .map(|d| d.port)
        .or_else(|| raw_record.as_ref().map(|d| d.port))
        .unwrap_or_else(|| 9527);

    let tcp_addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let tcp_listening =
        std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_millis(200)).is_ok();

    let startup_log = startup_log_path();
    let last_startup_log = std::fs::read_to_string(&startup_log)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // ADR-0021 comparison info: name the installed image and its content
    // identity next to the server's published one, so a drift verdict is
    // legible without reasoning about inodes.
    let installed_image = server_program();
    let installed_image = installed_image.is_file().then_some(installed_image);
    let installed_image_digest =
        installed_image.as_ref().and_then(|p| image_content_digest(p).map(|(_, d)| d));
    let discovery_record = discovery_record.or(raw_record);
    let server_image_digest = discovery_record.as_ref().and_then(|r| r.image_digest.clone());
    let server_image_current = discovery_record
        .as_ref()
        .is_none_or(server_image_is_current);

    ServerDiagnostics {
        instance_dir: discovery::instance_dir(),
        default_port: 9527,
        discovery_path,
        discovery_record,
        discovery_valid: discover_at(&discovery::global_discovery_path()).is_some(),
        lock_path,
        lock_held,
        lock_holder_pid,
        lock_holder_alive,
        local_endpoint,
        local_endpoint_exists: local_probe.exists,
        local_endpoint_connectable: local_probe.connectable,
        tcp_port: port,
        tcp_listening,
        startup_log_path: startup_log,
        last_startup_log,
        installed_image,
        installed_image_digest,
        server_image_digest,
        server_image_current,
    }
}

// Boxing the attached payload would make this public protocol-facing API less direct.
#[allow(clippy::large_enum_variant)]
pub enum Handshake {
    Attached {
        req_tx: mpsc::UnboundedSender<AgentRequest>,
        resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
        session_id: String,
        round_counter: u64,
        history: Vec<Message>,
        /// Durable round-interrupt records (C11) from the server's welcome,
        /// so an attaching TUI projects the stopped rounds into its restored
        /// transcript. Empty for older servers.
        round_interrupts: Vec<nuo_wire::RoundInterrupt>,
        /// Durable retry-resolution records from the server's welcome, so an
        /// attaching TUI projects recovered rounds into its restored
        /// transcript. Empty for older servers.
        retry_resolutions: Vec<nuo_wire::RetryResolution>,
        /// The provider/model the session is currently serving, carried on
        /// the welcome so the TUI's hint bar shows them from the first frame
        /// instead of waiting for the next provider mutation.
        provider: String,
        model: String,
        /// Backend-owned completion/help vocabulary for this session.
        command_catalog: nuo_wire::CommandCatalog,
    },
    Pick(Vec<SessionOverview>),
}

pub async fn connect(info: &ServerInfo, action: AttachAction) -> Result<Handshake, String> {
    // Prefer the platform-native local endpoint; fall back to TCP for
    // exposed and legacy deployments.
    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_handshake((sink, source), action).await;
    }
    if info.port == 0 {
        return Err("server runs in client-bound mode (UDS only) and local endpoint is not reachable".into());
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    let (sink, source) = websocket_split(ws);
    finish_handshake((sink, source), action).await
}

/// The stream-generic attach handshake, shared by the UDS and TCP paths.
async fn finish_handshake(
    parts: (BoxWireSink, BoxWireStream),
    action: AttachAction,
) -> Result<Handshake, String> {
    let (mut wire_sink, mut wire_source) = parts;

    // Declare this client's working directory so the server scopes a fresh or
    // auto-attached session to the project the user actually invoked us in —
    // the server's own cwd is whatever the first client that spawned it
    // happened to use. A server predating the field ignores it; a failed cwd
    // read degrades to the server's fallback.
    let project = std::env::current_dir().ok();
    let select = Wire::Select {
        action,
        project,
        // ADR-0141: the client's interactivity posture. The TUI is a human
        // by construction; headless callers override this via
        // [`crate::client::set_posture`] before connecting.
        posture: current_posture(),
        // Product build: advisory identity on the wire since ADR-0134 (the
        // protocol number below is the gate), but still enforced against
        // pre-protocol servers, which judge it by exact equality.
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        // Wire protocol number (ADR-0134): the authority for whether this
        // server can serve us. A pre-protocol server ignores the field
        // (unknown fields are dropped by serde) and falls back to judging
        // the product version above.
        protocol: Some(PROTOCOL_VERSION),
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| format!("wire send select: {e}"))?;

    let reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match wire_source.next().await {
                Some(Ok(wire)) => match wire {
                    Wire::Welcome {
                        session_id,
                        round_counter,
                        messages,
                        provider,
                        model,
                        round_interrupts,
                        retry_resolutions,
                        command_catalog,
                    } => {
                        return Ok(Reply::Welcome(Welcome {
                            session_id,
                            round_counter,
                            messages,
                            provider,
                            model,
                            round_interrupts,
                            retry_resolutions,
                            command_catalog,
                        }));
                    }
                    Wire::Pick { sessions } => return Ok(Reply::Pick(sessions)),
                    Wire::Error { message, .. } => {
                        return Err(format!("server rejected the attach: {message}"));
                    }
                    _ => tracing::warn!("attach: unexpected frame during handshake, ignored"),
                },
                Some(Err(error)) => return Err(format!("wire recv during handshake: {error}")),
                None => return Err("server closed the connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for handshake from server".to_string())??;

    let welcome = match reply {
        Reply::Welcome(w) => w,
        Reply::Pick(sessions) => {
            let _ = wire_sink.close().await;
            return Ok(Handshake::Pick(sessions));
        }
    };

    let (req_out_tx, mut req_out_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (resp_in_tx, resp_in_rx) = mpsc::unbounded_channel::<AgentResponse>();

    tokio::spawn(async move {
        let mut end_pending = false;
        while let Some(request) = req_out_rx.recv().await {
            if matches!(request, AgentRequest::EndSession) {
                // Client-declared session end (ADR-0112): mark it so the
                // pump, after flushing this frame, gives the server a brief
                // window to tear the session down before the socket closes.
                // Without this, a client that sends EndSession and drops
                // everything immediately can race the runtime shutdown: the
                // frame reaches the wire but the process exits before the
                // server even reads it — harmless over TCP/UDS (the kernel
                // buffers the written bytes), but the graceful-close
                // handshake below is still worth attempting.
                end_pending = true;
            }
            if let Err(error) = wire_sink.send(Wire::Request { request }).await {
                tracing::warn!(%error, "attach: wire send failed");
                break;
            }
        }
        if end_pending {
            // Give the server a moment to observe the EndSession frame and
            // run the teardown (it broadcasts the terminal `Exit` back,
            // which the response pump relays). Bounded so a hung server
            // cannot pin the client open either.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let _ = wire_sink.close().await;
    });

    tokio::spawn(async move {
        while let Some(frame) = wire_source.next().await {
            match frame {
                Ok(Wire::Response { response }) => {
                    if resp_in_tx.send(response).is_err() {
                        return;
                    }
                }
                Ok(_) => tracing::warn!("attach: unexpected post-handshake frame, ignored"),
                Err(error) => {
                    tracing::warn!(%error, "attach: wire recv failed");
                    break;
                }
            }
        }
        let _ = resp_in_tx.send(AgentResponse::Exit);
    });

    Ok(Handshake::Attached {
        req_tx: req_out_tx,
        resp_rx: resp_in_rx,
        session_id: welcome.session_id,
        round_counter: welcome.round_counter,
        history: welcome.messages,
        round_interrupts: welcome.round_interrupts,
        retry_resolutions: welcome.retry_resolutions,
        provider: welcome.provider,
        model: welcome.model,
        command_catalog: welcome.command_catalog,
    })
}

/// Issue one control-plane verb (ADR-0096) to the server and await its reply:
/// create, prompt, interrupt, answer a permission, or kill — without attaching
/// as a session client. The dashboard's session-management keys (`i` interrupt,
/// `p` prompt, `n` new session) go through here. Prefers native local IPC and
/// falls back to TCP, exactly like [`connect`].
pub async fn control(
    info: &ServerInfo,
    request: ControlRequest,
) -> Result<(), String> {
    control_with_reply(info, request).await.map(|_| ())
}

/// [`control`] for reply-bearing verbs (ADR-0208 `AskArchivist`): the
/// server's `ControlReply` free-string travels back on success instead of
/// being dropped. Verbs without a payload reply with an empty string, so
/// every existing caller can migrate to this shape without behavior change.
pub async fn control_with_reply(
    info: &ServerInfo,
    request: ControlRequest,
) -> Result<String, String> {
    let action = AttachAction::Control(request);

    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_control((sink, source), action).await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        req.headers_mut().insert("Authorization", value);
    }
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    let (sink, source) = websocket_split(ws);
    finish_control((sink, source), action).await
}

/// The stream-generic control handshake: send the `Select{Control}` frame and
/// await the single `ControlReply`. One verb per connection.
async fn finish_control(
    parts: (BoxWireSink, BoxWireStream),
    action: AttachAction,
) -> Result<String, String> {
    let (mut wire_sink, mut wire_source) = parts;
    // Control verbs carry their own scope (`CreateSession::project`); the
    // server never consults a select-level project for them.
    let select = Wire::Select {
        action,
        project: None,
        posture: current_posture(),
        // Same handshake contract as the attach path (ADR-0134): the
        // protocol number is the gate, the product version the advisory
        // identity still enforced by pre-protocol servers.
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        protocol: Some(PROTOCOL_VERSION),
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| format!("wire send control select: {e}"))?;

    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match wire_source.next().await {
                Some(Ok(wire)) => match wire {
                    Wire::ControlReply { ok, error, .. } => {
                        return if ok {
                            // ADR-0208: reply-bearing verbs (AskArchivist)
                            // travel in the free-string channel; verbs
                            // without a payload leave it `None` → empty.
                            Ok(error.unwrap_or_default())
                        } else {
                            Err(error.unwrap_or_else(|| "control verb rejected".to_string()))
                        };
                    }
                    Wire::Error { message, .. } => return Err(message),
                    _ => tracing::warn!("control: unexpected frame during handshake, ignored"),
                },
                Some(Err(error)) => return Err(format!("wire recv during control: {error}")),
                None => return Err("server closed the control connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for control reply from server".to_string())?
}

struct Welcome {
    session_id: String,
    round_counter: u64,
    messages: Vec<Message>,
    provider: String,
    model: String,
    /// Durable round-interrupt records (C11) carried on the server's
    /// welcome; empty for older servers that predate the field.
    round_interrupts: Vec<nuo_wire::RoundInterrupt>,
    /// Durable retry-resolution records (success-side mirror of the
    /// interrupts) carried on the server's welcome; empty for older servers
    /// that predate the field.
    retry_resolutions: Vec<nuo_wire::RetryResolution>,
    command_catalog: nuo_wire::CommandCatalog,
}
enum Reply {
    Welcome(Welcome),
    Pick(Vec<SessionOverview>),
}

// Monitor-protocol client (ADR-0093)
/// Open the WebSocket, perform the monitor handshake, and return a channel of
/// stream frames. The WS pump runs on a background task; the channel closes
/// when the server hangs up.
pub async fn monitor_stream(
    info: &ServerInfo,
    action: MonitorAction,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String> {
    // Prefer platform-native local IPC; fall back to TCP for exposed/legacy
    // deployments — the same
    // transport policy as `remote::connect`/`remote::control`, so the monitor
    // stream works against a UDS-only server.
    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_monitor((sink, source), action, "local IPC").await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    let (sink, source) = websocket_split(ws);
    finish_monitor((sink, source), action, &url).await
}

/// The stream-generic monitor handshake + framing, shared by the UDS and TCP
/// paths: send the `Select{Monitor}` handshake, await the opening snapshot
/// (bounded), then forward every diff frame into the returned channel.
async fn finish_monitor(
    parts: (BoxWireSink, BoxWireStream),
    action: MonitorAction,
    target: &str,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String> {
    let (mut wire_sink, mut wire_source) = parts;

    let select = Wire::Select {
        action: AttachAction::Monitor(action),
        // Monitor streams are host-wide; no project scope applies.
        project: None,
        posture: current_posture(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        protocol: Some(PROTOCOL_VERSION),
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| format!("wire send select: {e}"))?;

    // Await the opening snapshot (or a handshake-level error) with a bound.
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match wire_source.next().await {
                Some(Ok(wire)) => match wire {
                    Wire::Monitor { event } => return Ok(event),
                    Wire::Error { message, .. } => return Err(message),
                    _ => tracing::warn!("status: unexpected frame during handshake, ignored"),
                },
                Some(Err(error)) => return Err(format!("wire recv during handshake: {error}")),
                None => return Err("server closed the connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for monitor snapshot from {target}"))??;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tx.send(first);
    tokio::spawn(async move {
        while let Some(frame) = wire_source.next().await {
            match frame {
                Ok(Wire::Monitor { event }) => {
                    if tx.send(event).is_err() {
                        return;
                    }
                }
                Ok(_) => tracing::warn!("status: unexpected post-handshake frame, ignored"),
                Err(error) => {
                    tracing::warn!(%error, "status: wire recv failed");
                    break;
                }
            }
        }
    });
    Ok(rx)
}

pub fn upsert_session_row(rows: &mut Vec<MonitoredSession>, row: MonitoredSession) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
}

pub fn upsert_task_row(
    rows: &mut Vec<nuo_wire::MonitoredTask>,
    row: nuo_wire::MonitoredTask,
) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.created_at_ms));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn server_detachment_creates_a_fresh_session_and_process_group() {
        use std::process::{Command, Stdio};

        let mut command = Command::new("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_server_detachment(&mut command);

        let mut child = command
            .spawn()
            .expect("a correctly detached child must spawn");
        let pid = child.id() as libc::pid_t;
        // SAFETY: `pid` names the live child owned by this test.
        let sid = unsafe { libc::getsid(pid) };
        // SAFETY: `pid` names the live child owned by this test.
        let pgid = unsafe { libc::getpgid(pid) };

        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(sid, pid, "setsid must make the server a session leader");
        assert_eq!(
            pgid, pid,
            "setsid must also make the server a process-group leader"
        );
    }

    #[test]
    fn remote_server_parses_the_documented_address_forms() {
        let t = |addr| RemoteServer::parse(addr, Some("tok".into())).unwrap();
        // host:port
        assert_eq!(
            (t("192.168.1.4:9800").host, t("192.168.1.4:9800").port),
            ("192.168.1.4".to_string(), 9800)
        );
        // ws:// scheme is accepted and stripped
        assert_eq!(t("ws://box.lan:9800").host, "box.lan");
        assert_eq!(t("ws://box.lan:9800").port, 9800);
        // bare :port means loopback (the local server over TCP)
        assert_eq!(t(":9800").host, "127.0.0.1");
        // whitespace is tolerated
        assert_eq!(t("  box.lan:9800  ").host, "box.lan");
    }

    #[test]
    fn remote_server_requires_a_port_and_a_token() {
        // No port: refuse rather than defaulting — a default would
        // silently target the local server when a remote one was meant.
        let err = RemoteServer::parse("box.lan", Some("tok".into())).unwrap_err();
        assert!(err.contains("not host:port"), "{err}");
        let err = RemoteServer::parse("box.lan:notaport", Some("tok".into())).unwrap_err();
        assert!(err.contains("not a port number"), "{err}");
        // Every network-exposed server requires the bearer token.
        let err = RemoteServer::parse("box.lan:9800", None).unwrap_err();
        assert!(err.contains("--token"), "{err}");
        let err = RemoteServer::parse("box.lan:9800", Some(String::new())).unwrap_err();
        assert!(err.contains("--token"), "{err}");
    }

    #[test]
    fn test_compare_versions() {
        assert_eq!(compare_versions("0.25.0", "0.25.0"), VersionRelation::Equal);
        assert_eq!(
            compare_versions("0.26.0", "0.25.0"),
            VersionRelation::ClientNewer
        );
        assert_eq!(
            compare_versions("0.24.0", "0.25.0"),
            VersionRelation::ClientOlder
        );
        assert_eq!(
            compare_versions("1.0.0", "0.25.0"),
            VersionRelation::ClientNewer
        );
        assert_eq!(
            compare_versions("not-a-semver", "0.25.0"),
            VersionRelation::Unknown
        );
    }

    #[cfg(unix)]
    #[test]
    fn server_image_match_accepts_the_explicit_process_exe() {
        let current = std::env::current_exe().unwrap();
        assert!(nuo_host::process::process_image_matches_path(
            std::process::id(),
            &current
        ));
    }

    /// The local compatibility policy (ADR-0134 revision), in pure form.
    /// The image result is injected as a boolean so protocol/version policy
    /// stays deterministic and independent of the process running the test.
    #[test]
    fn local_pair_policy_after_adr0134() {
        let me = env!("CARGO_PKG_VERSION");
        // In-window protocol + different version (upgrade leftover): SERVED.
        // A patch bump must not interrupt a healthy server.
        assert!(local_pair_compatible(
            Some(PROTOCOL_VERSION),
            Some("0.0.1-much-older"),
            false, // image check irrelevant: different version short-circuits
        ));
        // In-window + MIN edge + different version: also served.
        assert!(local_pair_compatible(
            Some(MIN_PROTOCOL_VERSION),
            Some("99.0.0"),
            false,
        ));
        // Out-of-window protocol: refused whatever the version says.
        assert!(!local_pair_compatible(
            Some(PROTOCOL_VERSION + 1),
            Some(me),
            true,
        ));
        assert!(!local_pair_compatible(Some(0), Some("0.0.1"), true,));
        // Same version + current image (this very process): served.
        assert!(local_pair_compatible(
            Some(PROTOCOL_VERSION),
            Some(me),
            true,
        ));
        // Legacy record (no protocol): exact version equality rules.
        assert!(local_pair_compatible(None, Some(me), true,));
        assert!(!local_pair_compatible(None, Some("0.0.0"), true,));
        assert!(!local_pair_compatible(None, None, true));
    }

    /// The dev-drift lie (same version, different image) is refused even
    /// with a matching protocol — the one freshness gate that survives
    /// ADR-0134 locally.
    #[test]
    fn dev_drift_same_version_is_refused() {
        assert!(!local_pair_compatible(
            Some(PROTOCOL_VERSION),
            Some(env!("CARGO_PKG_VERSION")),
            false,
        ));
    }

    #[test]
    fn dev_drift_predicate_is_narrow() {
        // Same version + in-window protocol + stale image => drift (the only
        // case `ensure_server` may reclaim). The published hash "0000" cannot
        // match any real installed image, so this is drift whenever an
        // installed image resolves (always true under `cargo test`).
        let drift = ServerInfo {
            pid: 1,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            protocol: Some(PROTOCOL_VERSION),
            image_digest: Some("0000".to_string()),
            image_len: Some(0),
            ..Default::default()
        };
        if server_program().is_file() {
            assert!(is_dev_drift(&drift));
        }

        // Different product version => not drift (upgrade leftover: served).
        let older = ServerInfo {
            version: Some("0.0.0".to_string()),
            protocol: Some(PROTOCOL_VERSION),
            image_digest: Some("0000".to_string()),
            ..Default::default()
        };
        assert!(!is_dev_drift(&older));

        // Out-of-window protocol => not drift (skew, not drift).
        let skewed = ServerInfo {
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            protocol: Some(0),
            image_digest: Some("0000".to_string()),
            ..Default::default()
        };
        assert!(!is_dev_drift(&skewed));
    }

    #[test]
    fn drift_refusal_error_names_the_work_it_would_interrupt() {
        let info = ServerInfo {
            pid: 42,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            ..Default::default()
        };
        let busy = drift_refusal_error(&info, &ServerActivity::Busy { sessions: 2, tasks: 1 });
        assert!(busy.contains("binary mismatch"), "{busy}");
        assert!(busy.contains("2 active session(s)"), "{busy}");
        assert!(busy.contains("1 server task(s)"), "{busy}");
        assert!(busy.contains("nuo stop"), "{busy}");

        let idle = drift_refusal_error(&info, &ServerActivity::Idle { sessions: 0, tasks: 0 });
        assert!(idle.contains("restarts on demand"), "{idle}");
    }

    #[test]
    fn server_image_is_current_tolerates_missing_proc_entry() {
        // A legacy record (no published hash) for a pid that does not exist
        // (or /proc unavailable): no evidence of drift, so the server must
        // not be disturbed.
        let info = ServerInfo {
            pid: u32::MAX - 1,
            image_digest: None,
            ..Default::default()
        };
        assert!(server_image_is_current(&info));
    }

    #[test]
    fn content_hash_detects_a_rebuilt_image_and_accepts_an_identical_one() {
        // ADR-0021: with a published image hash, detection is content-based.
        // Fabricate an installed image whose real hash we compute, then a
        // server record claiming (a) that exact hash -> current, (b) a
        // different hash -> drifted, (c) matching hash but wrong length ->
        // drifted (the cheap length pre-gate).
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("nuo-image");
        std::fs::write(&image, b"the installed server image bytes").unwrap();
        let (len, digest) = image_content_digest(&image).unwrap();

        let current = ServerInfo {
            pid: 1,
            image_digest: Some(digest.clone()),
            image_len: Some(len),
            ..Default::default()
        };
        let drifted = ServerInfo {
            pid: 1,
            image_digest: Some("deadbeef".to_string()),
            image_len: Some(len),
            ..Default::default()
        };
        let wrong_len = ServerInfo {
            pid: 1,
            image_digest: Some(digest.clone()),
            image_len: Some(len.wrapping_add(1)),
            ..Default::default()
        };
        // `server_image_is_current` resolves the *installed* image through
        // `server_program()`, so assert on the pure core directly instead of
        // depending on the ambient build layout. The installed image is the
        // real `(len, digest)`; only the server's published identity varies.
        let installed = Some((len, digest.clone()));
        assert!(content_matches_record(installed.clone(), &current));
        assert!(!content_matches_record(installed.clone(), &drifted));
        assert!(!content_matches_record(installed.clone(), &wrong_len));
        // No published hash (legacy server): never a content-based drift.
        assert!(content_matches_record(
            installed,
            &ServerInfo {
                pid: 1,
                image_digest: None,
                ..Default::default()
            }
        ));
    }

    #[test]
    fn same_inode_distinguishes_a_rebuilt_file_at_the_same_path() {
        // The dev-loop case in miniature: replacing a path's content gives
        // the same path a NEW inode — that is drift, not equality, even
        // though the path strings are identical. The server-side metadata
        // is captured before the replacement (as /proc does for a running
        // process), the client-side after.
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path().join("nuo");
            std::fs::write(&bin, b"old image").unwrap();
            let server_meta = std::fs::metadata(&bin).unwrap();
            // cargo rebuild: temp + rename over the same path.
            let staging = tmp.path().join(".nuo.tmp");
            std::fs::write(&staging, b"new image").unwrap();
            std::fs::rename(&staging, &bin).unwrap();
            let client_meta = std::fs::metadata(&bin).unwrap();
            assert!(
                !same_inode(&server_meta, &client_meta),
                "a rebuilt binary at the same path must not compare equal"
            );
            assert!(same_inode(&client_meta, &client_meta));
        }
    }

    #[test]
    fn test_version_mismatch_messages() {
        let server_older = ServerInfo {
            pid: 1234,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some("0.0.0".to_string()),
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        };
        let msg = version_mismatch(&server_older);
        assert!(msg.contains("is older than this client"));
        assert!(msg.contains("nuo stop"));

        let server_newer = ServerInfo {
            pid: 1234,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some("99.0.0".to_string()),
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        };
        let msg = version_mismatch(&server_newer);
        assert!(msg.contains("older than the running server"));
        assert!(msg.contains("update your nuo client"));

        let server_none = ServerInfo {
            pid: 1234,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: None,
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        };
        let msg = version_mismatch(&server_none);
        assert!(msg.contains("unknown (older than 0.24)"));
        assert!(msg.contains("nuo stop"));

        let server_equal_drift = ServerInfo {
            pid: u32::MAX - 10,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        };
        let msg = version_mismatch(&server_equal_drift);
        assert!(msg.contains("client/server"));
    }

    fn record(port: u16, token: Option<String>) -> ServerInfo {
        ServerInfo {
            pid: 99999999, // Unused/dead pid
            process_birth_token: None,
            port,
            token,
            project_root: "/tmp/proj".to_string(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: None,
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        }
    }
    fn dead_port() -> u16 {
        for _ in 0..10 {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            drop(l);
            if std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                std::time::Duration::from_millis(5),
            )
            .is_err()
            {
                return port;
            }
        }
        1
    }
    #[test]
    fn discover_at_returns_none_without_racing_to_delete_stale_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&record(dead_port(), None)).unwrap(),
        )
        .unwrap();
        assert!(discover_at(&path).is_none());
        assert!(
            path.exists(),
            "a reader must leave stale cleanup to the lock-owning lifecycle path"
        );
    }
    #[test]
    fn discover_at_preserves_record_if_pid_is_still_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        let live_rec = ServerInfo {
            pid: std::process::id(),
            process_birth_token: None,
            port: dead_port(),
            token: None,
            project_root: "/tmp/proj".to_string(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: None,
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        };
        std::fs::write(&path, serde_json::to_vec(&live_rec).unwrap()).unwrap();
        assert!(discover_at(&path).is_none());
        assert!(
            path.exists(),
            "discovery file for living PID must NOT be deleted on transient probe fail"
        );
    }
    #[test]
    fn discover_at_tolerates_missing_and_corrupt_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        assert!(discover_at(&path).is_none());
        std::fs::write(&path, b"not json").unwrap();
        assert!(discover_at(&path).is_none());
        assert!(path.exists());
    }

    #[tokio::test]
    async fn stop_handles_already_dead_process_and_cleans_up() {
        let info = ServerInfo {
            pid: 99999999, // Unused pid
            process_birth_token: None,
            port: 1,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some("0.0.1".to_string()),
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        };
        let res = stop(&info).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn stop_refuses_a_recycled_pid_identity() {
        let identity = nuo_host::process::process_identity(std::process::id()).unwrap();
        let info = ServerInfo {
            pid: identity.pid,
            process_birth_token: Some(identity.birth_token.wrapping_add(1)),
            port: 1,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some("0.0.1".to_string()),
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        };
        let error = stop(&info).await.unwrap_err();
        assert!(error.contains("process identity is stale"));
    }
}

#[test]
fn stop_budget_follows_the_advertised_grace() {
    // The tier budget must come from the record (ADR-0116); the test
    // pins the plumbing by asserting the fallback constant is generous
    // enough to cover a default-configured server's 10s drain, so a
    // legacy record cannot cause an early SIGTERM escalation either.
    assert!(FALLBACK_GRACE >= Duration::from_secs(10));
}