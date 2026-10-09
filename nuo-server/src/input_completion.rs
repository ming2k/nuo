//! Backend-owned composer completion.
//!
//! Frontends send the current input and cursor, then render the returned edit
//! candidates. Command matching, intent steering, content-admitted workspace
//! commands, project-file discovery, and explicit-path expansion all live here
//! so terminal and browser apps cannot drift into separate completion products.

use ignore::WalkBuilder;
use nuo_wire::{
    AgentResponse, CommandCatalog, CommandSpec, InputCompletion, InputCompletionKind,
};
use std::path::{Path, PathBuf};
use tokio::sync::OnceCell;

const MAX_PATH_COMPLETIONS: usize = 200;

pub struct InputCompletionEngine {
    catalog: CommandCatalog,
    project_root: PathBuf,
    project_entries: OnceCell<Vec<String>>,
    skills_registry: Option<nuo_harness::skills::SkillRegistry>,
}

impl InputCompletionEngine {
    pub fn new(catalog: CommandCatalog, project_root: PathBuf) -> Self {
        Self {
            catalog,
            project_root,
            project_entries: OnceCell::new(),
            skills_registry: None,
        }
    }

    pub fn with_skills(mut self, registry: nuo_harness::skills::SkillRegistry) -> Self {
        self.skills_registry = Some(registry);
        self
    }

    /// Produce one race-safe protocol response. `cursor` is a Unicode-scalar
    /// index, matching the wire contract in `nuo-wire`.
    pub async fn complete(&self, request_id: u64, input: String, cursor: usize) -> AgentResponse {
        let items = match char_to_byte(&input, cursor) {
            Some(cursor_byte) => self.complete_items(&input, cursor_byte).await,
            None => Vec::new(),
        };
        AgentResponse::ComposerCompletions {
            request_id,
            text: input,
            cursor,
            items,
        }
    }

    async fn complete_items(&self, input: &str, cursor_byte: usize) -> Vec<InputCompletion> {
        if input.starts_with('/') {
            return self.complete_slash(input, cursor_byte);
        }
        let Some((at_start, cursor_end)) = mention_range_at(input, cursor_byte) else {
            return Vec::new();
        };
        let query = &input[at_start + 1..cursor_end];
        if is_skill_query(query) {
            self.complete_skills(input, query, at_start, cursor_end)
        } else if is_file_query(query) {
            self.complete_files(input, query, at_start, cursor_end)
                .await
        } else if is_explicit_path_prefix(query) {
            self.complete_explicit_path(input, query, at_start, cursor_end)
        } else {
            self.complete_namespaces(input, query, at_start, cursor_end)
                .await
        }
    }

    fn complete_slash(&self, input: &str, cursor_byte: usize) -> Vec<InputCompletion> {
        complete_slash_items(&self.catalog, input, cursor_byte)
    }

    fn complete_skills(
        &self,
        input: &str,
        query: &str,
        at_start: usize,
        cursor_end: usize,
    ) -> Vec<InputCompletion> {
        let Some(registry) = &self.skills_registry else {
            return Vec::new();
        };
        let guard = registry.lock();
        let all_skills = guard.list();
        // Shared item builder: the filter/prefix derivation lives once in
        // `skill_completion_items` (also used by the frontend-test adapter).
        skill_completion_items(input, query, at_start, cursor_end, &all_skills)
    }

    async fn complete_files(
        &self,
        input: &str,
        query: &str,
        at_start: usize,
        cursor_end: usize,
    ) -> Vec<InputCompletion> {
        let root = self.project_root.clone();
        let entries = self
            .project_entries
            .get_or_init(|| async move {
                tokio::task::spawn_blocking(move || scan_project_files(&root))
                    .await
                    .unwrap_or_default()
            })
            .await;
        file_completion_items(input, query, at_start, cursor_end, &entries)
    }

    async fn complete_namespaces(
        &self,
        input: &str,
        query: &str,
        at_start: usize,
        cursor_end: usize,
    ) -> Vec<InputCompletion> {
        namespace_items(
            input,
            query,
            at_start,
            cursor_end,
            self.skills_registry.is_some(),
        )
    }

    fn complete_explicit_path(
        &self,
        input: &str,
        query: &str,
        at_start: usize,
        cursor_end: usize,
    ) -> Vec<InputCompletion> {
        let Some((dir, name_prefix)) = resolve_explicit_dir(query, &self.project_root) else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut items = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                if name == ".git" || !name.to_lowercase().starts_with(&name_prefix.to_lowercase()) {
                    return None;
                }
                let is_dir = entry.file_type().ok()?.is_dir();
                let mut label = entry.path().to_str()?.to_string();
                if is_dir && !label.ends_with('/') {
                    label.push('/');
                }
                Some(path_item(
                    input,
                    &label,
                    at_start,
                    cursor_end,
                    InputCompletionKind::PathExplicit,
                ))
            })
            .take(MAX_PATH_COMPLETIONS)
            .collect::<Vec<_>>();
        sort_path_completions(&mut items);
        items
    }
}

/// Complete slash commands and subcommands synchronously against a `CommandCatalog`.
///
/// This is a pure-domain, zero-I/O computation that guarantees instant (< 1µs) execution
/// for both the server and frontend applications without transient latency (ADR-0162).
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
pub fn complete_for_frontend_test(
    catalog: CommandCatalog,
    project_root: PathBuf,
    input: &str,
    cursor: usize,
) -> Vec<InputCompletion> {
    let engine = InputCompletionEngine::new(catalog, project_root);
    let Some(cursor_byte) = char_to_byte(input, cursor) else {
        return Vec::new();
    };
    if input.starts_with('/') {
        return engine.complete_slash(input, cursor_byte);
    }
    let Some((at_start, cursor_end)) = mention_range_at(input, cursor_byte) else {
        return Vec::new();
    };
    let query = &input[at_start + 1..cursor_end];
    if is_skill_query(query) {
        engine.complete_skills(input, query, at_start, cursor_end)
    } else if is_file_query(query) {
        let entries = scan_project_files(&engine.project_root);
        file_completion_items(input, query, at_start, cursor_end, &entries)
    } else if is_explicit_path_prefix(query) {
        engine.complete_explicit_path(input, query, at_start, cursor_end)
    } else {
        // ADR-0290: Stage 1 is namespace-only; no content pass-through.
        namespace_items(input, query, at_start, cursor_end, true)
    }
}

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

fn is_skill_query(query: &str) -> bool {
    nuo_wire::mention::SKILL_NAMESPACES
        .iter()
        .any(|ns| query.starts_with(ns))
}

fn is_file_query(query: &str) -> bool {
    nuo_wire::mention::FILE_NAMESPACES
        .iter()
        .any(|ns| query.starts_with(ns))
}

/// Build skill completion items for a `@skill:` / `@skills:` (or bare, during
/// Stage 2) query. Pure: the filter/prefix derivation and the enabled/quarantine
/// and substring checks live here, so the server engine and the frontend-test
/// adapter share one implementation (ADR-0290 `[INV-COMPLETE-03]`).
fn skill_completion_items(
    input: &str,
    query: &str,
    at_start: usize,
    cursor_end: usize,
    skills: &[nuo_harness::skills::Skill],
) -> Vec<InputCompletion> {
    // The namespace vocabulary is owned by the grammar kernel; derive the
    // filter and the re-emitted prefix from it rather than re-typing the
    // spellings here.
    use nuo_wire::mention::SKILL_NAMESPACES;
    let (filter, explicit_prefix) = SKILL_NAMESPACES
        .iter()
        .find_map(|ns| query.strip_prefix(ns).map(|rest| (rest, *ns)))
        // `complete_skills` is only reached for a skill-namespace query, so the
        // canonical (first) spelling is the correct default when none is found.
        .unwrap_or((query, SKILL_NAMESPACES[0]));
    let filter_lower = filter.to_lowercase();
    let mut items = Vec::new();
    for skill in skills {
        if !skill.enabled || skill.quarantined {
            continue;
        }
        if !filter_lower.is_empty() && !skill.name.to_lowercase().contains(&filter_lower) {
            continue;
        }
        items.push(skill_item(
            input,
            skill,
            explicit_prefix,
            at_start,
            cursor_end,
        ));
    }
    items
}

/// Build file/dir completion items for a `@file:` / `@files:` query. Pure: the
/// filter derivation and the path matching live here so the server engine and
/// the frontend-test adapter share one implementation (ADR-0290
/// `[INV-COMPLETE-03]`).
fn file_completion_items(
    input: &str,
    query: &str,
    at_start: usize,
    cursor_end: usize,
    entries: &[String],
) -> Vec<InputCompletion> {
    let filter = nuo_wire::mention::strip_file_namespace(query).unwrap_or(query);
    let mut items = entries
        .iter()
        .filter(|path| path_query_match(path, filter))
        .take(MAX_PATH_COMPLETIONS)
        .map(|path| {
            path_item(
                input,
                path,
                at_start,
                cursor_end,
                if path.ends_with('/') {
                    InputCompletionKind::PathDir
                } else {
                    InputCompletionKind::PathFile
                },
            )
        })
        .collect::<Vec<_>>();
    sort_path_completions(&mut items);
    items
}

/// Stage 1 of the two-stage `@` pipeline (ADR-0290): list only the entity
/// **namespaces** (`@file:`, `@skill:`) whose name is a prefix of what has been
/// typed so far.
///
/// This is the entire Stage 1 — there is no content pass-through. A query that
/// is not a prefix of a namespace name yields nothing, so typing `@xyz` cannot
/// leak files or skills (which live in Stage 2, reachable only *after* a
/// namespace has been committed). This is the fix for ADR-0256's
/// "fuzzy-at-mention", which let a bare `@query` bypass the namespace gate.
fn namespace_items(
    input: &str,
    query: &str,
    at_start: usize,
    cursor_end: usize,
    has_skills: bool,
) -> Vec<InputCompletion> {
    use nuo_wire::mention::{FILE_NAMESPACES, SKILL_NAMESPACES};

    let q_lower = query.to_lowercase();
    let prefixes_namespace =
        |namespaces: &[&str]| namespaces.iter().any(|ns| ns.starts_with(&q_lower));
    let mut items = Vec::new();
    if prefixes_namespace(&FILE_NAMESPACES) {
        items.push(file_namespace_item(input, at_start, cursor_end));
    }
    if has_skills && prefixes_namespace(&SKILL_NAMESPACES) {
        items.push(skill_namespace_item(input, at_start, cursor_end));
    }
    items
}

fn file_namespace_item(
    input: &str,
    at_start_byte: usize,
    replace_end_byte: usize,
) -> InputCompletion {
    InputCompletion {
        label: "@file:".to_string(),
        description: "File mention namespace".to_string(),
        insert_text: "@file:".to_string(),
        replace_start: input[..at_start_byte].chars().count(),
        replace_end: input[..replace_end_byte].chars().count(),
        kind: InputCompletionKind::PathDir,
        alias_of: None,
        command: None,
    }
}

fn skill_namespace_item(
    input: &str,
    at_start_byte: usize,
    replace_end_byte: usize,
) -> InputCompletion {
    InputCompletion {
        label: "@skill:".to_string(),
        description: "Skill mention namespace".to_string(),
        insert_text: "@skill:".to_string(),
        replace_start: input[..at_start_byte].chars().count(),
        replace_end: input[..replace_end_byte].chars().count(),
        kind: InputCompletionKind::PathDir,
        alias_of: None,
        command: None,
    }
}

fn skill_item(
    input: &str,
    skill: &nuo_harness::skills::Skill,
    explicit_prefix: &str,
    at_start_byte: usize,
    replace_end_byte: usize,
) -> InputCompletion {
    let label = format!("@{explicit_prefix}{}", skill.name);
    let mut insert_text = format!("@{explicit_prefix}{}", skill.name);
    let needs_space = input
        .get(replace_end_byte..)
        .and_then(|suffix| suffix.chars().next())
        .map(|character| !character.is_whitespace())
        .unwrap_or(true);
    if needs_space {
        insert_text.push(' ');
    }
    let desc = skill
        .description
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    InputCompletion {
        label,
        description: if desc.is_empty() {
            format!("Skill ({})", skill.scope)
        } else {
            desc
        },
        insert_text,
        replace_start: input[..at_start_byte].chars().count(),
        replace_end: input[..replace_end_byte].chars().count(),
        kind: InputCompletionKind::PathExplicit,
        alias_of: None,
        command: None,
    }
}

fn path_item(
    input: &str,
    label: &str,
    at_start_byte: usize,
    replace_end_byte: usize,
    kind: InputCompletionKind,
) -> InputCompletion {
    let (item_label, replace_start_byte, mut insert_text) = match kind {
        // A project directory keeps the `@file:` trigger so accepting it can ask
        // the backend for the next path segment.
        InputCompletionKind::PathDir => (
            format!("@file:{label}"),
            at_start_byte,
            format!("@file:{label}"),
        ),
        // Files inside the workspace are canonical `@file:...` entity mentions.
        InputCompletionKind::PathFile => (
            format!("@file:{label}"),
            at_start_byte,
            format!("@file:{label}"),
        ),
        // Explicit paths outside workspace (e.g. @../, @~/) expand to raw paths.
        InputCompletionKind::PathExplicit => (label.to_string(), at_start_byte, label.to_string()),
        InputCompletionKind::Slash
        | InputCompletionKind::SlashAlias
        | InputCompletionKind::Intent => (label.to_string(), at_start_byte, label.to_string()),
    };
    if matches!(
        kind,
        InputCompletionKind::PathFile | InputCompletionKind::PathExplicit
    ) {
        let needs_space = input
            .get(replace_end_byte..)
            .and_then(|suffix| suffix.chars().next())
            .map(|character| !character.is_whitespace())
            .unwrap_or(true);
        if needs_space {
            insert_text.push(' ');
        }
    }
    InputCompletion {
        label: item_label,
        description: match kind {
            InputCompletionKind::PathDir => "Directory".to_string(),
            InputCompletionKind::PathFile => "File".to_string(),
            _ => String::new(),
        },
        insert_text,
        replace_start: input[..replace_start_byte].chars().count(),
        replace_end: input[..replace_end_byte].chars().count(),
        kind,
        alias_of: None,
        command: None,
    }
}

fn char_to_byte(input: &str, char_index: usize) -> Option<usize> {
    if char_index == input.chars().count() {
        Some(input.len())
    } else {
        input.char_indices().nth(char_index).map(|(byte, _)| byte)
    }
}

fn mention_range_at(input: &str, cursor_byte: usize) -> Option<(usize, usize)> {
    nuo_wire::mention::mention_range_at(input, cursor_byte)
}

fn path_query_match(path: &str, query: &str) -> bool {
    if query.is_empty() {
        !path.trim_end_matches('/').contains('/')
    } else if let Some(prefix) = query.strip_suffix('/').filter(|_| query.contains('/')) {
        path.to_lowercase().starts_with(&prefix.to_lowercase())
    } else {
        path.to_lowercase().contains(&query.to_lowercase())
    }
}

fn is_explicit_path_prefix(query: &str) -> bool {
    let normalized = query.replace('\\', "/");
    normalized.starts_with("../")
        || normalized == ".."
        || normalized.starts_with("./")
        || normalized == "."
        || normalized.starts_with("~/")
        || normalized == "~"
        || normalized.starts_with('/')
        || Path::new(query).is_absolute()
}

fn resolve_explicit_dir(query: &str, cwd: &Path) -> Option<(PathBuf, String)> {
    let normalized = query.replace('\\', "/");
    let home = || dirs::home_dir().or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    let (expanded, names_directory) = if normalized == "~" {
        (home()?, true)
    } else if let Some(rest) = normalized.strip_prefix("~/") {
        (home()?.join(rest), normalized.ends_with('/'))
    } else {
        (
            cwd.join(Path::new(&normalized)),
            normalized.ends_with('/') || matches!(normalized.as_str(), "." | ".."),
        )
    };
    let (dir, name_prefix) = if names_directory {
        (expanded, String::new())
    } else {
        (
            expanded.parent()?.to_path_buf(),
            expanded.file_name()?.to_string_lossy().into_owned(),
        )
    };
    Some((dir.canonicalize().unwrap_or(dir), name_prefix))
}

fn scan_project_files(root: &Path) -> Vec<String> {
    // In-process traversal with ripgrep's `ignore` walker: identical
    // gitignore and hidden-file semantics on every machine, with no
    // dependency on an installed `rg` executable. `MAX_SCAN_RESULTS` bounds
    // the walk so huge trees cannot stall composer completion.
    const MAX_SCAN_RESULTS: usize = 2_000;
    let mut paths: Vec<String> = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .ignore(true)
        .sort_by_file_path(|left, right| left.cmp(right))
        .build();
    for entry in walker.flatten() {
        let Some(path) = entry
            .path()
            .strip_prefix(root)
            .ok()
            .and_then(|relative| relative.to_str().map(str::to_string))
        else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        if Path::new(&path)
            .components()
            .any(|component| component.as_os_str().to_str() == Some(".git"))
        {
            continue;
        }
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            paths.push(format!("{path}/"));
        } else {
            paths.push(path.clone());
            if let Some(index) = path.rfind('/') {
                paths.push(format!("{}/", &path[..index]));
            }
        }
        if paths.len() >= MAX_SCAN_RESULTS {
            break;
        }
    }
    sort_paths(&mut paths);
    paths.dedup();
    paths
}

fn sort_paths(paths: &mut [String]) {
    paths.sort_by(|left, right| {
        right
            .ends_with('/')
            .cmp(&left.ends_with('/'))
            .then_with(|| left.to_lowercase().cmp(&right.to_lowercase()))
    });
}

fn sort_path_completions(items: &mut [InputCompletion]) {
    items.sort_by(|left, right| {
        right
            .label
            .ends_with('/')
            .cmp(&left.label.ends_with('/'))
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
    });
}

/// Expand a usage signature (e.g. `"/trust [workspace|extensions|all]"` or
/// `"/debug trace on|off"`) into concrete runnable subcommand completion candidates.
/// Strips placeholder arguments like `<id>` and expands option sets like `a|b|c`
/// or `[a|b|c]`.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> CommandCatalog {
        crate::startup::command_catalog(&[("/project-check".into(), "Check project".into())])
    }

    #[tokio::test]
    async fn slash_matching_and_intent_are_backend_owned() {
        let engine = InputCompletionEngine::new(catalog(), PathBuf::from("."));
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(7, "/mod".into(), 4).await
        else {
            panic!("unexpected response")
        };
        assert!(items.iter().any(|item| item.label == "/models"));

        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(8, "/switch".into(), 7).await
        else {
            panic!("unexpected response")
        };
        assert_eq!(
            items.first().map(|item| item.label.as_str()),
            Some("/models")
        );
    }

    #[tokio::test]
    async fn slash_subcommand_options_expand_and_filter() {
        let engine = InputCompletionEngine::new(catalog(), PathBuf::from("."));

        // Retired command has no completion surface.
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(10, "/extensions ".into(), 12).await
        else {
            panic!("unexpected response")
        };
        assert!(items.is_empty());

        // /trust offers only the closed asset-domain grammar, each verb with
        // its own introduction (not the parent summary).
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(12, "/trust ".into(), 7).await
        else {
            panic!("unexpected response")
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "/trust all",
                "/trust instructions",
                "/trust ex-workspace",
                "/trust mcp",
                "/trust skills",
                "/trust hooks",
                "/trust status",
                "/trust revoke"
            ]
        );
        for item in &items {
            assert_ne!(
                item.description, "(via '') ",
                "subcommand rows must carry their own summary"
            );
        }
        let status_row = items.iter().find(|i| i.label == "/trust status").unwrap();
        assert!(
            status_row.description.contains("trust state"),
            "status row shows its own introduction, got: {}",
            status_row.description
        );
        assert_eq!(status_row.kind, InputCompletionKind::Slash);

        // Removed subcommands stay absent.
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(13, "/trust w".into(), 8).await
        else {
            panic!("unexpected response")
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.is_empty());

        // /debug trace offers on and off
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(14, "/debug trace ".into(), 13).await
        else {
            panic!("unexpected response")
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["/debug trace on", "/debug trace off"]);

        // /debug trace of filters to /debug trace off
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(15, "/debug trace of".into(), 15).await
        else {
            panic!("unexpected response")
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["/debug trace off"]);

        // /role expands subcommands
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(17, "/role ".into(), 6).await
        else {
            panic!("unexpected response")
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            &labels[..3],
            &["/role developer", "/role ops", "/role philosophist",]
        );
    }

    #[tokio::test]
    async fn slash_aliases_are_first_class_candidates() {
        let engine = InputCompletionEngine::new(catalog(), PathBuf::from("."));

        // Typing `/resum` surfaces the alias `/resume` under its own label
        // (the user's spelling is preserved in the menu), but accepting it
        // commits the canonical `/sessions`: `insert_text` is the target and
        // `alias_of` marks the row so frontends render it distinctly.
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(20, "/resum".into(), 6).await
        else {
            panic!("unexpected response")
        };
        let resume_row = items
            .iter()
            .find(|i| i.label == "/resume")
            .expect("alias /resume surfaces for prefix /resum");
        assert_eq!(resume_row.kind, InputCompletionKind::SlashAlias);
        assert_eq!(resume_row.insert_text, "/threads");
        assert_eq!(resume_row.alias_of.as_deref(), Some("/threads"));
        assert_eq!(
            resume_row.description, "Browse or resume past threads",
            "description is the target's plain summary — no inline (alias …) prose"
        );
        // The flyout doc is the target's spec.
        assert_eq!(
            resume_row.command.as_ref().map(|spec| spec.name.as_str()),
            Some("/threads")
        );

        // The canonical command row exists alongside, with plain summary and
        // no alias marker.
        let threads_row = items
            .iter()
            .find(|i| i.label == "/threads")
            .expect("canonical /threads also offered");
        assert_eq!(threads_row.kind, InputCompletionKind::Intent);
        assert_eq!(threads_row.alias_of, None);
        assert_eq!(threads_row.insert_text, "/threads");

        // Exact alias input behaves identically: the row stays the alias
        // label, the committed edit is the canonical target.
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(21, "/resume".into(), 7).await
        else {
            panic!("unexpected response")
        };
        let row = items
            .iter()
            .find(|i| i.label == "/resume")
            .expect("alias row persists at exact match");
        assert_eq!(row.kind, InputCompletionKind::SlashAlias);
        assert_eq!(row.insert_text, "/threads");
        assert_eq!(row.alias_of.as_deref(), Some("/threads"));
    }

    #[tokio::test]
    async fn slash_subcommands_have_own_introductions_and_stop_at_depth_two() {
        let engine = InputCompletionEngine::new(catalog(), PathBuf::from("."));

        // After a declared parent + space, verbs complete with their own copy.
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(30, "/permissions ".into(), 13).await
        else {
            panic!("unexpected response")
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["/permissions clear"]);
        for item in &items {
            assert_ne!(
                item.description, "Show or clear always-allowed tool rules",
                "verb rows must not parrot the parent summary"
            );
        }
        let clear = items
            .iter()
            .find(|i| i.label == "/permissions clear")
            .unwrap();
        assert!(clear.description.to_lowercase().contains("clear"));
        // Insert restores the typed parent exactly and appends the verb.
        assert_eq!(clear.insert_text, "/permissions clear");

        // Prefix filter on the verb token.
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(31, "/permissions c".into(), 11).await
        else {
            panic!("unexpected response")
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["/permissions"]);

        // Depth guard: three tokens deep is NOT a subcommand position
        // (`/debug trace ` already completed `trace`; the next slot belongs
        // to trace's own on|off usage fallback, not subcommand matching).
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(32, "/debug pre".into(), 10).await
        else {
            panic!("unexpected response")
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["/debug preview"]);

        // Commands without declarations keep the legacy usage-expansion
        // fallback (bracketed options out of usage strings).
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(33, "/debug t".into(), 8).await
        else {
            panic!("unexpected response")
        };
        // `/debug` HAS declarations now — this exercises the declared path:
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["/debug trace"]);
    }

    #[test]
    fn expand_usage_options_handles_bracketed_and_raw_pipes() {
        assert_eq!(
            expand_usage_options("/trust [all|mcp|skills|status|revoke]"),
            vec![
                "/trust all",
                "/trust mcp",
                "/trust skills",
                "/trust status",
                "/trust revoke"
            ]
        );
        assert_eq!(
            expand_usage_options("/debug trace [on|off]"),
            vec!["/debug trace on", "/debug trace off"]
        );
        assert_eq!(
            expand_usage_options("/debug trace on|off"),
            vec!["/debug trace on", "/debug trace off"]
        );
        assert_eq!(
            expand_usage_options("/repeat cancel <id>"),
            vec!["/repeat cancel"]
        );
        assert_eq!(
            expand_usage_options("/repeat <cron> <prompt>"),
            Vec::<String>::new()
        );
        assert_eq!(expand_usage_options("/init [path]"), Vec::<String>::new());
        assert_eq!(expand_usage_options("/models"), Vec::<String>::new());
    }

    #[tokio::test]
    async fn project_paths_are_resolved_by_the_server() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/main.rs"), "fn main() {}").unwrap();
        let engine = InputCompletionEngine::new(catalog(), temp.path().to_path_buf());
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(1, "look @file:main".into(), 15).await
        else {
            panic!("unexpected response")
        };
        assert!(
            items.iter().any(|item| item.label == "@file:src/main.rs"
                && item.insert_text == "@file:src/main.rs ")
        );
    }

    #[test]
    fn project_scan_honors_gitignore_in_process() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir_all(temp.path().join("target/debug")).unwrap();
        std::fs::write(temp.path().join("target/debug/artifact"), "junk").unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/main.rs"), "fn main() {}").unwrap();
        let paths = scan_project_files(temp.path());
        assert!(paths.iter().any(|path| path == "src/main.rs"));
        assert!(paths.iter().any(|path| path == "src/"));
        assert!(paths.iter().all(|path| !path.starts_with("target/")));
    }

    #[tokio::test]
    async fn skill_completions_trigger_inline_anywhere() {
        let temp = tempfile::tempdir().unwrap();
        let registry = nuo_harness::skills::SkillRegistry::empty();
        let skill: nuo_harness::skills::Skill = serde_json::from_value(serde_json::json!({
            "name": "skill-creator",
            "description": "Create and optimize nuo skills",
            "scope": "User",
            "source": "/skills/skill-creator/SKILL.md",
            "root": ".",
            "content": "",
            "version": null,
            "policy": { "allow_implicit_invocation": true }
        }))
        .unwrap();
        registry.replace(vec![skill]);

        let engine =
            InputCompletionEngine::new(catalog(), temp.path().to_path_buf()).with_skills(registry);

        // 1. Direct namespace trigger `@skill:`
        let input = "hello @skill:";
        let AgentResponse::ComposerCompletions { items, .. } = engine
            .complete(100, input.into(), input.chars().count())
            .await
        else {
            panic!("unexpected response")
        };
        assert!(
            items
                .iter()
                .any(|i| i.label == "@skill:skill-creator"
                    && i.insert_text == "@skill:skill-creator ")
        );

        // 2. Multiline cursor anywhere in content
        let input = "First line\nSecond line with @skill:creat and more text";
        let cursor_char_pos = "First line\nSecond line with @skill:creat".chars().count();
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(101, input.into(), cursor_char_pos).await
        else {
            panic!("unexpected response")
        };
        let match_item = items
            .iter()
            .find(|i| i.label == "@skill:skill-creator")
            .expect("should find skill-creator");
        assert_eq!(match_item.insert_text, "@skill:skill-creator");

        // 3. Namespace suggestion on `@skill`
        let input = "check @skill";
        let AgentResponse::ComposerCompletions { items, .. } = engine
            .complete(102, input.into(), input.chars().count())
            .await
        else {
            panic!("unexpected response")
        };
        assert!(
            items
                .iter()
                .any(|i| i.label == "@skill:" && i.insert_text == "@skill:")
        );

        // 4. Two-stage completion: bare `@` strictly offers namespaces, no file spam
        let input = "@";
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(103, input.into(), 1).await
        else {
            panic!("unexpected response")
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"@file:"));
        assert!(labels.contains(&"@skill:"));
        assert!(!labels.iter().any(|l| l.contains("Cargo.toml")));

        // 5. Prefix `@sk` narrows down to @skill: namespace item
        let input = "@sk";
        let AgentResponse::ComposerCompletions { items, .. } =
            engine.complete(104, input.into(), 3).await
        else {
            panic!("unexpected response")
        };
        assert_eq!(items[0].label, "@skill:");
        assert_eq!(items[0].kind, InputCompletionKind::PathDir);

        // 6. ADR-0290: a bare `@` query that is not a namespace prefix yields
        //    NOTHING. Stage-2 content (skills, files) must never leak through a
        //    bare `@query` — it is reachable only after `@skill:` / `@file:` has
        //    been committed. This is the reversal of ADR-0256's
        //    "fuzzy-at-mention" pass-through.
        for bare in ["@creat", "@skill-creator", "@main", "@Cargo"] {
            let AgentResponse::ComposerCompletions { items, .. } = engine
                .complete(105, bare.into(), bare.chars().count())
                .await
            else {
                panic!("unexpected response")
            };
            assert!(
                items.is_empty(),
                "bare {bare} must not leak Stage-2 content, got {:?}",
                items.iter().map(|i| &i.label).collect::<Vec<_>>()
            );
        }
    }
}
