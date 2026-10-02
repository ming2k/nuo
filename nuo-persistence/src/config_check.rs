//! Whole-file `config.toml` validation.
//!
//! The schema's compatibility policy is "unknown keys are ignored" (so a
//! rename never breaks parsing), which buys resilience at the cost of signal:
//! a typo'd key — or a key a newer release renamed — produced *no* output
//! anywhere and silently fell back to a default. `muta config check`
//! restores the signal without changing the policy: it re-parses the file
//! as a raw table and reports (a) hard syntax/type errors that made a load
//! fall back to defaults, (b) keys that parse but match nothing in the
//! schema, and (c) known historical spellings that now do nothing, so the
//! user can modernize before the next save drops them.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// One finding from a validation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFinding {
    /// Where: a dotted key path, or `<file>` for file-level errors.
    pub key: String,
    pub message: String,
    /// True when the key matched a known historical spelling that today's
    /// schema ignores (ADR-0120 renames and removed subsystems).
    pub is_legacy: bool,
}

/// Validate `config.toml` at `path` (default: the resolved config file).
///
/// Purely diagnostic — it never mutates anything and returns findings even
/// for a file the app would load fine. An empty result means the file is
/// fully understood by the current schema.
pub fn check_config_file(path: Option<PathBuf>) -> Vec<ConfigFinding> {
    let path = path.unwrap_or_else(crate::config::Config::config_file_path);
    let Ok(content) = fs::read_to_string(&path) else {
        return vec![ConfigFinding {
            key: "<file>".to_string(),
            message: format!("no config file at {}", path.display()),
            is_legacy: false,
        }];
    };
    let parsed: toml::Table = match toml::from_str(&content) {
        Ok(table) => table,
        Err(error) => {
            return vec![ConfigFinding {
                key: "<file>".to_string(),
                message: format!(
                    "unparseable TOML — the app fell back to defaults at load: {error}"
                ),
                is_legacy: false,
            }];
        }
    };
    // A file that parses as TOML but not as `Config` means a type error
    // (e.g. a string where a number is expected) — the loudest possible
    // signal, because loading discarded the user's whole setup.
    let mut findings = Vec::new();
    let effective = crate::web_migration::migrate_config_source(&content);
    if let Err(error) = toml::from_str::<crate::config::Config>(&effective) {
        findings.push(ConfigFinding {
            key: "<file>".to_string(),
            message: format!(
                "parses as TOML but not as the config schema — \
                 the app fell back to defaults at load: {error}"
            ),
            is_legacy: false,
        });
    }
    // When parsing succeeds, the semantic shape is trustworthy for the
    // unknown-key walk below.
    findings.extend(unknown_keys(&parsed, CONFIG_KEYS, ""));
    findings
}

/// Keys the current schema knows, as a nested map (empty map = a leaf).
/// Kept for the drift test and any future `muta config` completions: the
/// test asserts every section the schema serializes appears here.
pub fn schema_key_tree() -> BTreeMap<String, BTreeMap<String, String>> {
    let mut root = BTreeMap::new();
    root.insert("default_connection".to_string(), BTreeMap::new());
    root.insert("default_model".to_string(), BTreeMap::new());
    root.insert("mcp".to_string(), BTreeMap::new());
    root.insert("context".to_string(), BTreeMap::new());
    root.insert("connection_retry_max_attempts".to_string(), BTreeMap::new());
    root.insert("connection_retry_base_ms".to_string(), BTreeMap::new());
    root.insert("connection_retry_max_ms".to_string(), BTreeMap::new());
    root.insert("favorites".to_string(), BTreeMap::new());
    root.insert("hidden_models".to_string(), BTreeMap::new());
    root.insert("skills".to_string(), BTreeMap::new());
    root.insert("permissions".to_string(), BTreeMap::new());
    root.insert("workspace".to_string(), BTreeMap::new());
    root.insert("bash_policy".to_string(), BTreeMap::new());
    root.insert("web".to_string(), BTreeMap::new());
    root.insert("agent".to_string(), BTreeMap::new());
    root.insert("hooks".to_string(), BTreeMap::new());
    root.insert("tool_variants".to_string(), BTreeMap::new());
    root.insert("daemon".to_string(), BTreeMap::new());
    root
}

/// Top-level schema keys (flat view of the section tree), exposed for the
/// tests and any future `muta config` completions.
pub const CONFIG_KEYS: &[&str] = &[
    "default_connection",
    "default_model",
    "mcp",
    "context",
    "connection_retry_max_attempts",
    "connection_retry_base_ms",
    "connection_retry_max_ms",
    "favorites",
    "hidden_models",
    "skills",
    "permissions",
    "workspace",
    "bash_policy",
    "web",
    "agent",
    "hooks",
    "tool_variants",
    "daemon",
];

/// Historical top-level/section keys today's schema deliberately ignores.
/// Surfaced separately from unknown keys so `check` can say *why* the key is
/// dead and what replaced it.
const LEGACY_KEYS: &[(&str, &str)] = &[
    ("default_provider", "renamed to `default_connection`"),
    (
        "websearch",
        "renamed to `[web]`; accepted as migration input and emitted canonically on save",
    ),
    (
        "provider_retry_max_attempts",
        "renamed to `connection_retry_max_attempts`",
    ),
    (
        "provider_retry_base_ms",
        "renamed to `connection_retry_base_ms`",
    ),
    (
        "provider_retry_max_ms",
        "renamed to `connection_retry_max_ms`",
    ),
    (
        "compaction_preserve_turns",
        "renamed to `compaction.preserve_rounds` (ADR-0047); the old key is \
         ignored and dropped on next save (ADR-0120)",
    ),
    (
        "compaction_preserve_rounds",
        "moved into `[compaction]` table as `preserve_rounds`",
    ),
    (
        "compaction_summarize",
        "moved into `[compaction]` table as `summarize`",
    ),
    (
        "compaction_prune",
        "moved into `[compaction]` table as `prune`",
    ),
    (
        "compaction_prune_protect_tokens",
        "moved into `[compaction]` table as `prune_protect_tokens`",
    ),
    (
        "compaction.max_active_tokens",
        "removed; the pressure ladder derives thresholds from the context window",
    ),
    (
        "compaction.prompt_reserve_tokens",
        "removed; superseded by the pressure ladder",
    ),
    (
        "tui",
        "decoupled into the dedicated TUI client configuration `$XDG_CONFIG_HOME/mutx/config.toml` (ADR-0136)",
    ),
    (
        "input_history",
        "decoupled into the dedicated TUI client configuration `$XDG_CONFIG_HOME/mutx/config.toml` (ADR-0136)",
    ),
    ("providers", "moved to `connections.toml`"),
    (
        "model_reasoning",
        "moved into the model `e` editor's route settings",
    ),
    ("agent.review", "removed with the session-review subsystem"),
    (
        "builtins",
        "credentials layout replaced by `credentials.toml [connections.<id>]`",
    ),
    (
        "websearch.exa_api_key",
        "secrets moved to provider-scoped `credentials.toml [web.search]`",
    ),
    (
        "websearch.parallel_api_key",
        "secrets moved to provider-scoped `credentials.toml [web.search]`",
    ),
    (
        "websearch.tavily_api_key",
        "secrets moved to provider-scoped `credentials.toml [web.search]`",
    ),
    (
        "websearch.bocha_api_key",
        "secrets moved to provider-scoped `credentials.toml [web.search]`",
    ),
    (
        "websearch.jina_api_key",
        "secrets moved to provider-scoped `credentials.toml [web.reader]`",
    ),
    (
        "websearch.fallback",
        "removed; web search has exactly one selected provider and no fallback route",
    ),
];

/// Walk a parsed table against the known key set, reporting unknown leaves
/// and known-legacy keys. Table-valued unknown keys report once at the table
/// level (a whole unknown section), not per leaf — a misspelled section name
/// is one mistake, not ten.
fn unknown_keys(table: &toml::Table, known: &[&str], prefix: &str) -> Vec<ConfigFinding> {
    let mut out = Vec::new();
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if let Some((_, note)) = LEGACY_KEYS.iter().find(|(k, _)| *k == path) {
            out.push(ConfigFinding {
                key: path,
                message: format!("legacy key ignored by the current schema — {note}"),
                is_legacy: true,
            });
            continue;
        }
        if !known.contains(&key.as_str()) {
            out.push(ConfigFinding {
                key: path.clone(),
                message: if value.is_table() {
                    "unknown section (a typo here silently falls back to defaults)".to_string()
                } else {
                    "unknown key (ignored; check the spelling against `muta config list`)"
                        .to_string()
                },
                is_legacy: false,
            });
            continue;
        }
        // Known table sections: recurse when a nested key set exists. The
        // section schemas are validated by the `Config` parse above; the
        // recursion here only covers top-level → section membership.
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a config file into a temp dir that stays alive until the
    /// returned guard drops (the checker reads from disk, so the dir must
    /// outlive the write helper).
    fn write_config(content: &str) -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, content).unwrap();
        (path, dir)
    }

    #[test]
    fn schema_key_tree_covers_every_config_field() {
        // The tree is the checker's source of truth; it must list exactly
        // the serialized `Config` fields. A field added without updating the
        // tree would make `check` report false "unknown key" findings.
        // Optional fields (skip_serializing_if) may be absent from a default
        // serialization, so they are exempt from the emits-check but still
        // verified to parse.
        let optional = ["default_model"];
        let serialized = toml::to_string(&crate::config::Config::default()).unwrap();
        for key in schema_key_tree().keys() {
            assert!(
                serialized.contains(&format!("{key} ="))
                    || serialized.contains(&format!("[{key}]"))
                    || serialized.contains(&format!("[[{key}]]"))
                    || optional.contains(&key.as_str()),
                "checker knows key `{key}` but the schema does not serialize it"
            );
        }
        // And conversely: every table the schema emits must be known.
        for line in serialized.lines() {
            if let Some(table) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let name = table.trim_start_matches('[').trim_end_matches(']');
                if !name.is_empty()
                    && !name.contains('.')
                    && !name.contains("[[")
                    && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                {
                    assert!(
                        CONFIG_KEYS.contains(&name),
                        "schema emits section `{name}` the checker does not know"
                    );
                }
            }
        }
    }

    #[test]
    fn clean_file_produces_no_findings() {
        let (path, _dir) = write_config("default_connection = \"anthropic\"\n");
        assert!(check_config_file(Some(path)).is_empty());
    }

    #[test]
    fn typo_and_legacy_keys_are_reported_separately() {
        let (path, _dir) = write_config(
            "compaction_preserve_turns = 9\n\n[web]\nreader = \"builtin\"\n\n[copaction]\n",
        );
        let findings = check_config_file(Some(path));
        assert_eq!(findings.len(), 2, "got: {findings:?}");
        let legacy = findings.iter().find(|f| f.is_legacy).unwrap();
        assert_eq!(legacy.key, "compaction_preserve_turns");
        assert!(legacy.message.contains("preserve_rounds"));
        let typo = findings.iter().find(|f| !f.is_legacy).unwrap();
        assert_eq!(typo.key, "copaction");
        assert!(typo.message.contains("unknown section"));
    }

    #[test]
    fn unparseable_toml_is_a_finding() {
        let (path, _dir) = write_config("this is not = = toml\n");
        let findings = check_config_file(Some(path));
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("unparseable"));
    }

    #[test]
    fn type_mismatch_is_a_finding() {
        // `preferred_recent_rounds` is a u32; a string is a type error
        // that makes a load fall back to *defaults* — the exact silent
        // failure mode `check` exists to surface.
        let (path, _dir) = write_config("[context]\npreferred_recent_rounds = \"six\"\n");
        let findings = check_config_file(Some(path));
        assert!(
            findings.iter().any(|f| f.message.contains("schema")),
            "got: {findings:?}"
        );
    }

    #[test]
    fn legacy_websearch_alias_is_understood_and_reported() {
        let (path, _dir) = write_config("[websearch]\nreader = \"builtin\"\n");
        let findings = check_config_file(Some(path));
        assert_eq!(findings.len(), 1, "got: {findings:?}");
        assert_eq!(findings[0].key, "websearch");
        assert!(findings[0].is_legacy);
        assert!(findings[0].message.contains("[web]"));
    }

    #[test]
    fn missing_file_is_a_finding() {
        let dir = tempfile::tempdir().unwrap();
        let findings = check_config_file(Some(dir.path().join("nope.toml")));
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("no config file"));
    }
}
