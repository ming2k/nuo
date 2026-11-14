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
            "/trust revoke",
            "/trust deny"
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

    // /unattended offers on and off
    let AgentResponse::ComposerCompletions { items, .. } =
        engine.complete(14, "/unattended ".into(), 12).await
    else {
        panic!("unexpected response")
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["/unattended on", "/unattended off"]);

    // /unattended of filters to /unattended off
    let AgentResponse::ComposerCompletions { items, .. } =
        engine.complete(15, "/unattended of".into(), 14).await
    else {
        panic!("unexpected response")
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["/unattended off"]);

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

    // Subcommand matching and filtering for declared subcommands
    let AgentResponse::ComposerCompletions { items, .. } =
        engine.complete(32, "/unattended o".into(), 13).await
    else {
        panic!("unexpected response")
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["/unattended on", "/unattended off"]);
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
        expand_usage_options("/trace [on|off]"),
        vec!["/trace on", "/trace off"]
    );
    assert_eq!(
        expand_usage_options("/trace on|off"),
        vec!["/trace on", "/trace off"]
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
        items
            .iter()
            .any(|item| item.label == "@file:src/main.rs"
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
    let registry = nuo_harness::cognition::skills::SkillRegistry::empty();
    let skill: nuo_harness::cognition::skills::Skill = serde_json::from_value(serde_json::json!({
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
            .any(|i| i.label == "@skill:skill-creator" && i.insert_text == "@skill:skill-creator ")
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

    // 6.: a bare `@` query that is not a namespace prefix yields
    //    NOTHING. Stage-2 content (skills, files) must never leak through a
    //    bare `@query` — it is reachable only after `@skill:` / `@file:` has
    //    been committed. This is the reversal of
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
