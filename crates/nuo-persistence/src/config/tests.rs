use super::*;

#[test]
fn agent_table_round_trips_through_toml() {
    // The `[agent]` table must round-trip: partial TOML keeps defaults,
    // full TOML preserves explicit overrides.
    let toml_canonical = r#"
            [agent]
            hard_stop_turns = 40
        "#;
    let cfg: Config = toml::from_str(toml_canonical).unwrap();
    assert_eq!(cfg.agent.hard_stop_turns, 40);

    // Missing `[agent]` table → defaults match the documented values.
    let cfg: Config = toml::from_str("").unwrap();
    assert_eq!(cfg.agent.hard_stop_turns, 0);

    // Round-trip through save+load format (serialize then parse).
    let mut cfg = Config::default();
    cfg.agent.hard_stop_turns = 99;
    let serialised = toml::to_string(&cfg).unwrap();
    let parsed: Config = toml::from_str(&serialised).unwrap();
    assert_eq!(parsed.agent.hard_stop_turns, 99);
}

#[test]
fn compaction_round_count_writes_canonical_key_and_drops_legacy_key() {
    // The pre-ADR-0047 key is not aliased. It parses as
    // an unknown key (warned and ignored) and the field stays at its
    // default — the stale value must not carry through.
    let legacy: Config = toml::from_str("compaction_preserve_turns = 9").unwrap();
    assert_eq!(
        legacy.context.preferred_recent_rounds,
        Config::default().context.preferred_recent_rounds
    );

    let serialized = toml::to_string(&legacy).unwrap();
    assert!(serialized.contains("preferred_recent_rounds ="));
    assert!(!serialized.contains("compaction_preserve_turns ="));
    assert!(!serialized.contains("[compaction]"));
}

#[test]
fn legacy_compaction_keys_are_refused_under_inv_policy_01() {
    let err = toml::from_str::<Config>("[compaction]\nutilization = 0.85\n").unwrap_err();
    assert!(err.to_string().contains("retired [INV-POLICY-01]"));

    let err2 = toml::from_str::<Config>("compaction_preserve_rounds = 6\n").unwrap_err();
    assert!(err2.to_string().contains("retired [INV-POLICY-01]"));
}

#[test]
fn trajectory_guard_table_writes_canonical_key() {
    let canonical: Config =
        toml::from_str("[agent.trajectory_guard]\nenabled = false\nwindow = 24\n").unwrap();
    assert!(!canonical.agent.trajectory_guard.enabled);
    assert_eq!(canonical.agent.trajectory_guard.window, 24);

    let serialized = toml::to_string(&canonical).unwrap();
    assert!(
        serialized.contains("[agent.trajectory_guard]"),
        "got: {serialized}"
    );
}

#[test]
fn restart_budget_parses_canonically() {
    let canonical: Config = toml::from_str(
        "connection_retry_max_attempts = 12\n\
             connection_stream_restart_max_attempts = 1\n",
    )
    .unwrap();
    assert_eq!(canonical.connection_retry_max_attempts, 12);
    assert_eq!(canonical.connection_stream_restart_max_attempts, 1);

    let serialized = toml::to_string(&canonical).unwrap();
    assert!(
        serialized.contains("connection_stream_restart_max_attempts = 1"),
        "got: {serialized}"
    );
}

#[test]
fn tool_variants_table_parses_and_resolves_per_model() {
    // The table name mirrors the Config field name (`tool_variants`), as
    // serde maps struct fields to TOML keys verbatim. The model id is
    // quoted because it contains dots/hyphens. Each entry maps a capability
    // name to the variant id chosen for that model.
    let toml_src = r#"
            [tool_variants."kimi-k2.7-code"]
            read_text = "terse"
            execute_command = "workspace"

            [tool_variants."glm-5.2"]
            read_text = "verbose"
        "#;
    let cfg: Config = toml::from_str(toml_src).unwrap();

    // Known model → its map; unlisted capability within a known model → absent.
    let kimi = cfg.tool_variants.for_model("kimi-k2.7-code");
    assert_eq!(kimi.get("read_text").map(String::as_str), Some("terse"));
    assert_eq!(
        kimi.get("execute_command").map(String::as_str),
        Some("workspace")
    );
    assert!(kimi.get("grep").is_none());

    // A different model gets its own independent map.
    let glm = cfg.tool_variants.for_model("glm-5.2");
    assert_eq!(glm.get("read_text").map(String::as_str), Some("verbose"));
    assert!(glm.get("execute_command").is_none());

    // Unknown model → empty (but borrowable without an Option).
    assert!(cfg.tool_variants.for_model("does-not-exist").is_empty());

    // Absent table entirely → empty config, every lookup is empty.
    let cfg: Config = toml::from_str("").unwrap();
    assert!(cfg.tool_variants.for_model("kimi-k2.7-code").is_empty());
}

#[test]
fn tool_variants_round_trip_through_serialise() {
    let mut cfg = Config::default();
    let mut sel = nuo_protocol::VariantSelection::new();
    sel.insert("read_text".to_string(), "terse".to_string());
    sel.insert("bash".to_string(), "strict".to_string());
    cfg.tool_variants
        .0
        .insert("kimi-k2.7-code".to_string(), ModelToolVariants(sel));
    let serialised = toml::to_string(&cfg).unwrap();
    let parsed: Config = toml::from_str(&serialised).unwrap();
    let resolved = parsed.tool_variants.for_model("kimi-k2.7-code");
    assert_eq!(resolved.get("read_text").map(String::as_str), Some("terse"));
    assert_eq!(resolved.get("bash").map(String::as_str), Some("strict"));
}

#[test]
fn user_instructions_parse_and_validate() {
    let toml_src = r#"
            [[instructions.user]]
            model = "*deepseek*"
            text = "Speak naturally."

            [[instructions.user]]
            provider = "commandcode-goat"
            connection = "cc-fox"
            role = "reviewer"
            model = "deepseek/deepseek-v4-flash-fast"
            text = "Review tightly."
        "#;
    let cfg: Config = toml::from_str(toml_src).unwrap();
    assert_eq!(cfg.instructions.user.len(), 2);
    let scoped = cfg.instructions.scoped();
    assert_eq!(scoped.len(), 2);
    assert_eq!(scoped[0].source, nuo_protocol::InstructionSource::User);
    assert_eq!(scoped[0].ordinal, 0);
    assert_eq!(scoped[0].selector.model.as_deref(), Some("*deepseek*"));
    assert_eq!(scoped[1].ordinal, 1);
    assert_eq!(scoped[1].selector.role.as_deref(), Some("reviewer"));
    assert_eq!(scoped[1].selector.connection.as_deref(), Some("cc-fox"));

    let serialized = toml::to_string(&cfg).unwrap();
    assert!(serialized.contains("[[instructions.user]]"), "{serialized}");
    let reparsed: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(reparsed.instructions, cfg.instructions);
}

#[test]
fn user_instruction_validation_rejects_empty_text_and_bad_patterns() {
    let missing_text =
        toml::from_str::<Config>("[[instructions.user]]\nmodel = '*x*'\n").unwrap_err();
    assert!(missing_text.to_string().contains("text"), "{missing_text}");

    let bad_pattern =
        toml::from_str::<Config>("[[instructions.user]]\nmodel = '['\ntext = 'x'\n").unwrap_err();
    assert!(
        bad_pattern.to_string().contains("invalid `model` pattern"),
        "{bad_pattern}"
    );

    let empty_text = toml::from_str::<Config>("[[instructions.user]]\ntext = '   '\n").unwrap_err();
    assert!(empty_text.to_string().contains("empty"), "{empty_text}");
}

#[test]
fn retired_model_guidance_is_rejected_with_migration() {
    let error = toml::from_str::<Config>("[model_guidance]\n\"*x*\" = \"y\"\n").unwrap_err();
    assert!(error.to_string().contains("retired"), "{error}");
    assert!(
        error.to_string().contains("[[instructions.user]]"),
        "{error}"
    );
}

/// A sandboxed config root: a fresh throwaway directory with the
/// process-wide paths override installed against it.
///
/// Dropping it clears the override and removes the directory.
/// [`paths::TestDirsGuard`] also serialises against every other
/// override-touching test through the crate-wide lock, so no per-module
/// lock is needed. Access the directory through [`Self::path`].
pub(crate) struct ConfigSandbox {
    path: std::path::PathBuf,
    _paths: paths::TestDirsGuard,
}

impl ConfigSandbox {
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for ConfigSandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(crate) fn sandbox_config_dir() -> ConfigSandbox {
    let tmp = std::env::temp_dir().join(format!("nuo-creds-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    let dirs = paths::Dirs {
        config_dir: tmp.clone(),
        data_dir: tmp.join("data"),
        state_dir: tmp.join("state"),
        cache_dir: tmp.join("cache"),
        runtime_dir: None,
    };
    ConfigSandbox {
        path: tmp,
        _paths: paths::TestDirsGuard::install(dirs),
    }
}

#[test]
fn credentials_round_trip_through_toml() {
    let _sandbox = sandbox_config_dir();
    let mut creds = Credentials::default();
    creds.set_api_key("deepseek", Some("sk-ds".into()));
    creds.set_api_key("relay", Some("relay-secret".into()));
    // Empty / whitespace keys never materialise an entry.
    creds.set_api_key("keyless", Some("   ".into()));
    creds.save().unwrap();

    let on_disk = std::fs::read_to_string(paths::get().credentials_file()).unwrap();
    assert!(on_disk.contains("sk-ds"));
    assert!(on_disk.contains("relay-secret"));
    assert!(!on_disk.contains("keyless"), "empty key must not persist");

    let mut reloaded = Credentials::load();
    assert_eq!(
        reloaded
            .api_key("deepseek")
            .map(SecretString::expose_secret),
        Some("sk-ds")
    );
    assert_eq!(
        reloaded.api_key("relay").map(SecretString::expose_secret),
        Some("relay-secret")
    );
    assert!(reloaded.api_key("keyless").is_none());
    assert!(reloaded.api_key("missing").is_none());

    reloaded.remove_api_key("deepseek");
    reloaded.save().unwrap();
    assert!(Credentials::load().api_key("deepseek").is_none());
}

#[test]
fn credentials_ignore_legacy_sections_and_read_connections() {
    // The pre-refactor credentials layout (`[builtins.<id>]` /
    // `[user.<id>]`) is superseded by the canonical `[connections.<id>]`.
    // The retired `[providers]` spelling is no longer an alias
    // (ADR-0031 `[INV-CONF-01]`).
    let sandbox = sandbox_config_dir();
    std::fs::write(
        sandbox.path().join("credentials.toml"),
        r#"[builtins]
openai = "old-builtin"
[user.my-relay]
api_key = "old-user"
[connections]
deepseek = "new-key"
"#,
    )
    .unwrap();
    let creds = Credentials::load();
    assert!(
        creds.api_key("openai").is_none(),
        "builtins section is gone"
    );
    assert!(creds.api_key("my-relay").is_none(), "user section is gone");
    assert_eq!(
        creds.api_key("deepseek").map(SecretString::expose_secret),
        Some("new-key")
    );
}

#[test]
fn catalog_cache_and_remote_round_trip() {
    let _sandbox = sandbox_config_dir();
    let mut cache = RemoteCatalogCache::default();
    cache.connection_models.insert(
        "deepseek".to_string(),
        vec!["deepseek-v4-flash".to_string()],
    );
    cache.fitted_models.insert("kimi".to_string(), {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "kimi-for-coding".to_string(),
            FittedModelInfo {
                context_window: 262_144,
                reasoning: true,
                vision: Some(true),
                efforts: vec!["max".to_string()],
            },
        );
        m
    });
    cache.model_lists.insert(
        "deepseek".to_string(),
        ModelListCacheState {
            etag: Some("\"models-v1\"".to_string()),
            client_version: "0.1.0".to_string(),
            source_identity: "models-dev:deepseek".to_string(),
            refreshed_at_ms: 1234,
            refresh_failed: false,
        },
    );
    cache.save().unwrap();

    let mut reloaded = RemoteCatalogCache::load();
    assert_eq!(
        reloaded.fitted_models["kimi"]["kimi-for-coding"].context_window,
        262_144
    );
    assert!(reloaded.remote_metadata_for("deepseek", "nope").is_none());
    assert_eq!(
        reloaded.model_lists["deepseek"].etag.as_deref(),
        Some("\"models-v1\"")
    );

    reloaded.remove_connection("deepseek");
    assert!(reloaded.connection_models.is_empty());
    assert!(reloaded.model_lists.is_empty());
    reloaded.save().unwrap();
    assert!(RemoteCatalogCache::load().connection_models.is_empty());
}

#[test]
fn config_save_writes_domain_files_only() {
    let sandbox = sandbox_config_dir();
    std::fs::write(
        sandbox.path().join("config.toml"),
        "default_connection = \"deepseek\"\n[[providers]]\nid = \"deepseek\"\n",
    )
    .unwrap();
    let loaded = Config::load();
    assert_eq!(loaded.default_connection, "deepseek");
    assert!(
        !sandbox.path().join("config.toml").exists(),
        "the legacy monolith is retired on promotion"
    );
    let mut cfg = loaded;
    cfg.default_connection = "glm-cn".to_string();
    cfg.save().unwrap();
    let client = std::fs::read_to_string(Config::client_config_file_path()).unwrap();
    assert!(client.contains("default_connection = \"glm-cn\""));
    assert!(!client.contains("providers"));
    assert!(!sandbox.path().join("config.toml").exists());
}

#[test]
fn save_does_not_resurrect_user_deleted_workspace_roots() {
    // Disk is the newer truth for user-owned tables: a long-lived snapshot
    // must never write a hand-deleted workspace root back.
    let _sandbox = sandbox_config_dir();
    std::fs::write(
        Config::agent_config_file_path(),
        "[workspace]\nadditional_roots = [\"../x\", \"../y\"]\n",
    )
    .unwrap();
    let snapshot = Config::load();
    assert_eq!(
        snapshot.workspace.additional_roots,
        vec!["../x", "../y"],
        "precondition: snapshot saw the roots"
    );
    // The user deletes them from agent.toml while the session is live.
    std::fs::write(Config::agent_config_file_path(), "").unwrap();
    snapshot.save().unwrap();
    let after = Config::load();
    assert!(
        after.workspace.additional_roots.is_empty(),
        "user-deleted additional_roots must stay deleted, got {:?}",
        after.workspace.additional_roots
    );
}

#[test]
fn save_preserves_user_edited_unrelated_tables() {
    let _sandbox = sandbox_config_dir();
    std::fs::write(
        Config::client_config_file_path(),
        "[connection]\ndefault_model = \"m1\"\n",
    )
    .unwrap();
    let snapshot = Config::load();
    std::fs::write(
        Config::agent_config_file_path(),
        "[agent]\nhard_stop_turns = 42\n",
    )
    .unwrap();
    snapshot.save_preserving_connection_selection().unwrap();
    let after = Config::load();
    assert_eq!(after.agent.hard_stop_turns, 42, "user edit must survive");
    assert_eq!(after.default_model.as_deref(), Some("m1"));
}

#[test]
fn save_still_flushes_runtime_changed_selection_and_leaves_roots_to_disk() {
    let _sandbox = sandbox_config_dir();
    std::fs::write(
        Config::client_config_file_path(),
        "[connection]\ndefault_connection = \"old\"\n",
    )
    .unwrap();
    std::fs::write(
        Config::agent_config_file_path(),
        "[workspace]\nadditional_roots = [\"../user-root\"]\n",
    )
    .unwrap();
    let mut snapshot = Config::load();
    snapshot.default_connection = "new".to_string();
    snapshot.default_model = Some("m2".to_string());
    snapshot.merge_project_additional_roots(vec!["../project-root".to_string()]);
    assert_eq!(
        snapshot.workspace.additional_roots,
        vec!["../user-root", "../project-root"]
    );
    snapshot.save().unwrap();
    let after = Config::load();
    assert_eq!(after.default_connection, "new");
    assert_eq!(after.default_model.as_deref(), Some("m2"));
    assert_eq!(
        after.workspace.additional_roots,
        vec!["../user-root"],
        "project-merged roots are runtime view state, never persisted"
    );
}

#[test]
fn save_preserving_connection_selection_keeps_disk_default() {
    let _sandbox = sandbox_config_dir();
    std::fs::write(
        Config::client_config_file_path(),
        "[connection]\ndefault_connection = \"disk-default\"\ndefault_model = \"disk-model\"\n",
    )
    .unwrap();
    let mut snapshot = Config::load();
    snapshot.default_connection = "session-pin".to_string();
    snapshot.default_model = Some("session-model".to_string());
    snapshot.save_preserving_connection_selection().unwrap();
    let after = Config::load();
    assert_eq!(after.default_connection, "disk-default");
    assert_eq!(after.default_model.as_deref(), Some("disk-model"));
}

#[test]
fn route_settings_is_empty_tracks_the_tuning_map() {
    // An entry that carries nothing must report empty so callers prune it.
    assert!(RouteSettings::default().is_empty());

    // A depth choice makes the entry non-empty. There is no separate
    // `effort` field to check: the open tuning map is the single carrier
    // (ADR-0126), so "is this route configured" is exactly "is the map
    // non-empty, or does it carry overrides/cache intent".
    let mut with_effort = RouteSettings::default();
    with_effort
        .tunings
        .set_effort(Some(nuo_protocol::Effort::High));
    assert!(!with_effort.is_empty());
    assert_eq!(
        with_effort.tunings.effort(),
        Some(nuo_protocol::Effort::High)
    );

    // The same holds for the thinking switch.
    let mut with_thinking = RouteSettings::default();
    with_thinking.tunings.set_thinking(Some(false));
    assert!(!with_thinking.is_empty());
    assert_eq!(with_thinking.tunings.thinking(), Some(false));

    // Clearing a knob restores emptiness — no residue anywhere.
    with_effort.tunings.set_effort(None);
    assert!(with_effort.is_empty());

    // A knob the framework has never heard of also counts: the map is open,
    // and an unrecognised provider knob must not be pruned as "nothing".
    let mut unknown = RouteSettings::default();
    unknown.tunings.set(
        "edit_format",
        nuo_protocol::TunableSetting::from("apply_patch"),
    );
    assert!(!unknown.is_empty());
    assert_eq!(
        unknown.tunings.get("edit_format").and_then(|v| v.as_rung()),
        Some("apply_patch")
    );
}

// project-scope MCP merge

struct ScratchProject(tempfile::TempDir);

impl std::ops::Deref for ScratchProject {
    type Target = std::path::Path;

    fn deref(&self) -> &Self::Target {
        self.0.path()
    }
}

fn scratch_project_root() -> ScratchProject {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".nuo")).unwrap();
    ScratchProject(dir)
}

#[test]
fn resolve_workspace_additional_roots_empty_when_table_absent() {
    let root = scratch_project_root();
    assert!(
        Config::default()
            .resolve_workspace_additional_roots(&root)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn resolve_workspace_additional_roots_resolves_relative_and_absolute_entries() {
    let root = scratch_project_root();
    let sibling =
        std::env::temp_dir().join(format!("nuo-additional-root-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&sibling).unwrap();
    let mut config = Config::default();
    config.workspace.additional_roots = vec![
        format!("../{}", sibling.file_name().unwrap().to_string_lossy()),
        sibling.canonicalize().unwrap().display().to_string(),
    ];
    let roots = config.resolve_workspace_additional_roots(&root).unwrap();
    // Both spellings resolve to the same canonical sibling directory.
    assert_eq!(roots, vec![sibling.canonicalize().unwrap()]);
}

#[test]
fn resolve_workspace_additional_roots_skips_missing_and_nested_entries() {
    let root = scratch_project_root();
    let sibling =
        std::env::temp_dir().join(format!("nuo-additional-root-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::create_dir_all(root.join("nested")).unwrap();

    let mut config = Config::default();
    config.workspace.additional_roots = vec![
        "../does-not-exist-anywhere".to_string(),
        "nested".to_string(),
        root.canonicalize().unwrap().display().to_string(),
        sibling.display().to_string(),
    ];
    let detailed = config
        .resolve_workspace_additional_roots_detailed(&root)
        .unwrap();
    assert_eq!(detailed.admitted, vec![sibling.canonicalize().unwrap()]);
    assert_eq!(detailed.skipped.len(), 3);
    assert!(detailed.skipped[0].1.contains("does not exist"));
    assert!(detailed.skipped[1].1.contains("inside workspace"));
    assert!(detailed.skipped[2].1.contains("workspace root itself"));
}

#[test]
fn project_config_cannot_widen_workspace_roots() {
    let root = scratch_project_root();
    let sibling =
        std::env::temp_dir().join(format!("nuo-additional-root-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(
        root.join(".nuo/config.toml"),
        format!(
            r#"
                    [workspace]
                    additional_roots = ["{}"]
                "#,
            sibling.canonicalize().unwrap().display()
        ),
    )
    .unwrap();
    let roots = Config::default()
        .resolve_workspace_additional_roots(&root)
        .unwrap();
    assert!(roots.is_empty());
}

#[test]
fn load_project_mcp_reads_nuo_config_table() {
    let root = scratch_project_root();
    std::fs::write(
        root.join(".nuo/config.toml"),
        r#"
                [mcp.project-db]
                command = ["./bin/db-mcp"]
                enabled = true
            "#,
    )
    .unwrap();

    let mcp = Config::load_project_mcp(&root);
    assert_eq!(mcp.len(), 1);
    assert_eq!(mcp["project-db"].command, vec!["./bin/db-mcp".to_string()]);
    assert!(mcp["project-db"].enabled);
    assert_eq!(
        mcp["project-db"].sandbox_root.as_deref(),
        Some(root.canonicalize().unwrap().as_path())
    );
}

#[test]
fn load_project_mcp_reads_json_and_json_overrides_toml() {
    let root = scratch_project_root();
    std::fs::write(
        root.join(".nuo/config.toml"),
        "[mcp.shared]\ncommand = [\"toml-server\"]\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".nuo/mcp.json"),
        r#"{
                "mcpServers": {
                    "shared": {
                        "command": "json-server",
                        "args": ["--stdio"],
                        "env": {"MODE": "project"},
                        "read_only": true
                    }
                }
            }"#,
    )
    .unwrap();

    let mcp = Config::load_project_mcp(&root);
    assert_eq!(
        mcp["shared"].command,
        vec!["json-server".to_string(), "--stdio".to_string()]
    );
    assert_eq!(mcp["shared"].environment["MODE"], "project");
    assert!(mcp["shared"].enabled);
    assert!(mcp["shared"].read_only);
    assert_eq!(
        mcp["shared"].sandbox_root.as_deref(),
        Some(root.canonicalize().unwrap().as_path())
    );
}

#[test]
fn load_project_mcp_is_empty_when_file_absent() {
    let root = scratch_project_root();
    // No config.toml written.
    let mcp = Config::load_project_mcp(&root);
    assert!(mcp.is_empty());
}

#[test]
fn load_project_mcp_ignores_unrelated_keys_and_bad_toml() {
    let root = scratch_project_root();
    // A project file may carry non-mcp tables; only [mcp.*] is projected,
    // and an invalid [mcp.*] makes the whole table drop (warn + empty).
    std::fs::write(
        root.join(".nuo/config.toml"),
        r#"
                [agent]
                hard_stop_turns = 7

                [mcp.ok]
                command = ["x"]
            "#,
    )
    .unwrap();
    let mcp = Config::load_project_mcp(&root);
    assert_eq!(mcp.len(), 1, "agent ignored, mcp.ok projected");

    // A structurally invalid TOML → empty (never panics).
    let root2 = scratch_project_root();
    std::fs::write(root2.join(".nuo/config.toml"), "this is = = not toml").unwrap();
    assert!(Config::load_project_mcp(&root2).is_empty());
}

#[test]
fn merge_project_mcp_overrides_same_name_adds_new() {
    let mut global = Config::default();
    global.mcp.insert(
        "shared".to_string(),
        McpServerConfig {
            command: vec!["global-cmd".into()],
            enabled: true,
            ..McpServerConfig::default()
        },
    );
    global
        .mcp
        .insert("only-global".to_string(), McpServerConfig::default());

    let mut project = HashMap::new();
    // Same name → wholesale override (new command).
    project.insert(
        "shared".to_string(),
        McpServerConfig {
            command: vec!["project-cmd".into()],
            enabled: false,
            ..McpServerConfig::default()
        },
    );
    // New name → added.
    project.insert("only-project".to_string(), McpServerConfig::default());

    global.merge_project_mcp(project);

    // Override took effect.
    assert_eq!(
        global.mcp["shared"].command,
        vec!["project-cmd".to_string()]
    );
    assert!(!global.mcp["shared"].enabled);
    // Both pre-existing and added survive.
    assert!(global.mcp.contains_key("only-global"));
    assert!(global.mcp.contains_key("only-project"));
    assert_eq!(global.mcp.len(), 3);
}

#[test]
fn load_project_hooks_reads_hooks_array() {
    let root = scratch_project_root();
    std::fs::write(
        root.join(".nuo/config.toml"),
        r#"
                [[hooks]]
                event   = "PostToolUse"
                matcher = "Write|Edit"
                command = ".nuo/hooks/lint.sh"

                [[hooks]]
                event   = "Stop"
                command = ".nuo/hooks/notify.sh"
            "#,
    )
    .unwrap();

    let hooks = Config::load_project_hooks(&root);
    let canonical = root.0.path().canonicalize().unwrap();
    assert_eq!(hooks.len(), 2);
    assert_eq!(hooks[0].event, HookEventKind::PostToolUse);
    assert_eq!(hooks[0].command, ".nuo/hooks/lint.sh");
    assert_eq!(hooks[1].event, HookEventKind::Stop);
    assert!(
        hooks
            .iter()
            .all(|hook| hook.sandbox_root.as_deref() == Some(canonical.as_path()))
    );
}

#[test]
fn load_project_hooks_is_empty_when_file_absent() {
    let root = scratch_project_root();
    // No config.toml written.
    assert!(Config::load_project_hooks(&root).is_empty());
}

#[test]
fn load_project_hooks_ignores_unrelated_keys_and_bad_toml() {
    let root = scratch_project_root();
    // A project file may carry non-hooks tables; only [[hooks]] projects.
    std::fs::write(
        root.join(".nuo/config.toml"),
        r#"
                [mcp.something]
                command = ["x"]

                [[hooks]]
                event = "Stop"
                command = "echo done"
            "#,
    )
    .unwrap();
    let hooks = Config::load_project_hooks(&root);
    assert_eq!(hooks.len(), 1, "mcp ignored, one hook projected");

    // Structurally invalid TOML → empty (never panics).
    let root2 = scratch_project_root();
    std::fs::write(root2.join(".nuo/config.toml"), "this is = = not toml").unwrap();
    assert!(Config::load_project_hooks(&root2).is_empty());
}

#[test]
fn merge_project_hooks_appends_to_global() {
    let mut global = Config::default();
    global.hooks.push(HookSpec {
        event: HookEventKind::Stop,
        matcher: None,
        command: "global-notify.sh".to_string(),
        sandbox_root: None,
    });
    let project_hooks = vec![HookSpec {
        event: HookEventKind::PostToolUse,
        matcher: Some("Write".to_string()),
        command: ".nuo/hooks/lint.sh".to_string(),
        sandbox_root: Some(std::path::PathBuf::from("/project")),
    }];
    global.merge_project_hooks(project_hooks);
    assert_eq!(global.hooks.len(), 2, "global + project appended");
    // Global hook ordering preserved; project hooks come after.
    assert_eq!(global.hooks[0].command, "global-notify.sh");
    assert_eq!(global.hooks[1].command, ".nuo/hooks/lint.sh");
}

#[test]
fn load_project_additional_roots_reads_workspace_table() {
    let root = scratch_project_root();
    std::fs::write(
        root.join(".nuo/config.toml"),
        r#"
                [workspace]
                additional_roots = ["../optics", "~/shared/design"]
            "#,
    )
    .unwrap();

    let roots = Config::load_project_additional_roots(&root);
    assert_eq!(roots, vec!["../optics", "~/shared/design"]);
}

#[test]
fn load_project_additional_roots_empty_when_absent() {
    let root = scratch_project_root();
    assert!(Config::load_project_additional_roots(&root).is_empty());
}

#[test]
fn merge_project_additional_roots_deduplicates_and_appends() {
    let mut global = Config::default();
    global.workspace.additional_roots = vec!["../optics".to_string()];
    global.merge_project_additional_roots(vec!["../optics".to_string(), "../backend".to_string()]);
    assert_eq!(
        global.workspace.additional_roots,
        vec!["../optics", "../backend"]
    );
}

fn catalog_cache_with(connection: &str) -> RemoteCatalogCache {
    let mut cache = RemoteCatalogCache::default();
    cache
        .connection_models
        .insert(connection.into(), vec!["qfmodel".into()]);
    cache.fitted_models.insert(
        connection.into(),
        [(
            "qfmodel".to_string(),
            FittedModelInfo {
                context_window: 200_000,
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
    );
    cache.remote_metadata.insert(
        connection.into(),
        [(
            "qfmodel".to_string(),
            RemoteModelMetadata {
                context_window: Some(200_000),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
    );
    cache.model_lists.insert(
        connection.into(),
        ModelListCacheState {
            etag: Some("W/\"catalog-etag\"".into()),
            source_identity: "sha256:deadbeef".into(),
            client_version: "1.1.58".into(),
            refreshed_at_ms: 1_700_000_000_000,
            refresh_failed: false,
        },
    );
    cache
}

/// A rename re-keys all four maps, so the renamed connection keeps its
/// cached models, fitted metadata, advertised metadata, and — critically —
/// its ETag validator. Nothing is left under the dead name.
#[test]
fn rename_connection_carries_every_map_including_the_validator() {
    let mut cache = catalog_cache_with("old");
    cache.rename_connection("old", "new");

    assert!(
        !cache.connection_models.contains_key("old"),
        "no entry may be stranded under the old name"
    );
    assert_eq!(cache.connection_models["new"], ["qfmodel".to_string()]);
    assert_eq!(
        cache.fitted_models["new"]["qfmodel"].context_window,
        200_000
    );
    assert_eq!(
        cache
            .remote_metadata_for("new", "qfmodel")
            .and_then(|m| m.context_window),
        Some(200_000)
    );
    assert_eq!(
        cache.model_lists["new"].etag.as_deref(),
        Some("W/\"catalog-etag\""),
        "the validator must survive so the next sync revalidates, not refetches"
    );
    assert_eq!(cache.model_lists["new"].source_identity, "sha256:deadbeef");
}

/// A case-only rename is the same connection and must not move anything:
/// the key is case-insensitive for identity but the stored key
/// is exact, so a case flip that re-keyed would drop the entry.
#[test]
fn case_only_rename_leaves_the_entry_in_place() {
    let mut cache = catalog_cache_with("qod");
    cache.rename_connection("qod", "QOD");
    assert!(cache.model_lists.contains_key("qod"), "unchanged");
    assert!(!cache.model_lists.contains_key("QOD"));
}

/// Renaming a connection with no cached entry is a no-op that must not
/// create one — an empty entry would make `route_models` treat the
/// connection as "catalog answered with nothing" rather than "never
/// fetched", which changes the fallback behaviour.
#[test]
fn rename_connection_without_an_entry_creates_nothing() {
    let mut cache = RemoteCatalogCache::default();
    cache.rename_connection("ghost", "renamed");
    assert!(cache.connection_models.is_empty());
    assert!(cache.model_lists.is_empty());
    assert!(cache.remote_metadata.is_empty());
    assert!(cache.fitted_models.is_empty());
}
