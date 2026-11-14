use super::*;
use nuo_tool::ToolCall;

fn call(id: &str, name: &str, args: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: args.to_string(),
    }
}

/// Realistic Assistant message carrying a tool call — the way every Tool
/// result is produced in an OpenAI-protocol transcript. The pruner correlates
/// a result back to its call (name + arguments) via `tool_call_id`, so tests
/// that exercise labels / staleness / keep-alive need this originating call
/// to exist, not just the bare Tool result.
fn assistant_with_call(id: &str, name: &str, args: &str) -> Message {
    let mut message = Message::new(Role::Assistant, "");
    message.tool_calls = Some(vec![call(id, name, args)]);
    message
}

#[test]
fn weighted_session_estimate_matches_uncached_semantics() {
    // The pressure/prune gates must see EXACTLY the number the uncached
    // `estimate_tokens` produced (per-message framing + nested subagent
    // children), or their thresholds silently drift between call sites.
    // Build a session with children, tool calls, and CJK (multi-byte)
    // content, then compare both paths.
    let mut parent = Message::new(Role::Assistant, "父消息 some tool narrative");
    parent.tool_calls = Some(vec![call("c1", "read", "{\"path\":\"a.rs\"}")]);
    let mut child_user = Message::new(Role::User, "subagent prompt");
    child_user.children = Some(vec![Message::tool_result(
        &call("c2", "bash", "ls"),
        "child output 汉字混合 with English words",
    )]);
    parent.children = Some(vec![child_user]);
    let messages = vec![
        Message::new(Role::User, "hello world question"),
        parent,
        Message::new(Role::Assistant, "final answer"),
    ];

    let uncached = estimate_tokens(&messages);
    let weights = MessageTokenWeights::new();
    assert_eq!(estimate_tokens_weighted(&messages, &weights), uncached);
    // Second pass is a pure cache walk — same number, no re-tokenization.
    assert_eq!(estimate_tokens_weighted(&messages, &weights), uncached);
    // Empty window keeps the floor of 1 (parity with `estimate_tokens`).
    assert_eq!(
        estimate_tokens_weighted(&[], &weights),
        estimate_tokens(&[])
    );
}

#[test]
fn large_fresh_result_is_truncated_then_cleared_on_next_pass() {
    // ~64 BPE tokens per 256 'Y's: 5 000 chars ≈ 1 250 tokens, well past
    // the 512-token truncate tier; head/tail keeps 128 tokens each side.
    let big = "Y".repeat(5_000);
    let mut messages = vec![
        Message::new(Role::User, "q1"),
        Message::tool_result(&call("c1", "execute_command", "{}"), big),
        Message::new(Role::User, "q2"),
    ];

    // Pass 1: large + fresh -> truncated (head/tail kept), not cleared.
    let out1 = prune_tool_results(&mut messages, 0, 1).unwrap();
    assert_eq!(out1.cleared_count, 1);
    assert!(is_truncated(&messages[1].content));
    assert!(!is_cleared(&messages[1].content));
    assert!(messages[1].content.len() < 5_000);

    // Pass 2: already truncated -> escalated to a full clear. The reclaim
    // gate must see a real gain: the truncated form still holds ~260
    // tokens (128 head + 128 tail + marker), the clear drops it to ~20.
    let out2 = prune_tool_results(&mut messages, 0, 1).unwrap();
    assert_eq!(out2.cleared_count, 1);
    assert!(is_cleared(&messages[1].content));

    // Pass 3: nothing left to do -> None (converged / idempotent).
    assert!(prune_tool_results(&mut messages, 0, 50).is_none());
}

#[test]
fn short_result_is_cleared_directly_with_informative_placeholder() {
    // Below TRUNCATE_MIN_CHARS, so it skips the truncate tier and is cleared
    // outright — but still larger than the placeholder, so clearing reclaims
    // real space (a result shorter than its placeholder is correctly left
    // alone: clearing it would *grow* the window).
    let body = format!(
        "{}\n{}\n{}",
        "a".repeat(100),
        "b".repeat(100),
        "c".repeat(100)
    );
    let mut messages = vec![
        assistant_with_call("c1", "read", r#"{"path":"src/config.rs"}"#),
        Message::tool_result(&call("c1", "read", r#"{"path":"src/config.rs"}"#), body),
    ];

    let out = prune_tool_results(&mut messages, 0, 1).unwrap();
    assert_eq!(out.cleared_count, 1);
    // Informative: carries tool label, line count, and char count.
    assert!(messages[1].content.starts_with(CLEARED_TOOL_PREFIX));
    assert!(messages[1].content.contains("read src/config.rs"));
    assert!(messages[1].content.contains("3 lines"));
}

#[test]
fn stale_read_superseded_by_later_edit_is_cleared_first() {
    let body = "X".repeat(3_000);
    let mut messages = vec![
        // Round 1: read config.rs ...
        assistant_with_call("c1", "read", r#"{"path":"config.rs"}"#),
        Message::tool_result(&call("c1", "read", r#"{"path":"config.rs"}"#), body.clone()),
        // ... superseded by a later edit of the same file (round 2).
        assistant_with_call("c2", "edit", r#"{"path":"config.rs"}"#),
        Message::tool_result(
            &call("c2", "edit", r#"{"path":"config.rs"}"#),
            "ok".to_string(),
        ),
    ];

    let out = prune_tool_results(&mut messages, 0, 1).unwrap();
    assert_eq!(out.cleared_count, 1);
    // The stale read (index 1) is cleared outright (not merely truncated),
    // because a later op touched the same file.
    assert!(is_cleared(&messages[1].content));
    assert!(messages[1].content.contains("read config.rs"));
}

#[test]
fn paging_different_ranges_of_one_file_do_not_evict_each_other() {
    // The regression this fix targets: under prune pressure, reading two
    // *different* pages of one large file used to mark the earlier page
    // stale (path-only staleness), so the model lost it and re-read — an
    // oscillation. Different ranges are complementary, so neither is stale.
    let page1 = "A".repeat(3_000);
    let page2 = "B".repeat(3_000);
    let mut messages = vec![
        assistant_with_call("c1", "read", r#"{"path":"big.rs","offset":1,"limit":800}"#),
        Message::tool_result(
            &call("c1", "read", r#"{"path":"big.rs","offset":1,"limit":800}"#),
            page1,
        ),
        assistant_with_call(
            "c2",
            "read",
            r#"{"path":"big.rs","offset":900,"limit":800}"#,
        ),
        Message::tool_result(
            &call(
                "c2",
                "read",
                r#"{"path":"big.rs","offset":900,"limit":800}"#,
            ),
            page2,
        ),
    ];

    // Zero recency protection so nothing is spared by recency: the only thing
    // keeping page 1 alive is that page 2 does not supersede it. Both pages
    // are large and fresh, so the worst that happens is a gentle truncate —
    // never a full clear of a still-live page.
    let out = prune_tool_results(&mut messages, 0, 1).unwrap();
    assert!(
        !is_cleared(&messages[1].content),
        "page 1 must not be cleared by a read of a different page"
    );
    // Both are merely truncated (head/tail kept), not evicted.
    assert!(is_truncated(&messages[1].content) || messages[1].content.len() >= 3_000);
    assert!(out.cleared_count >= 1);
}

#[test]
fn full_reread_covering_an_earlier_page_clears_it() {
    // A later read whose range fully covers an earlier read *does* supersede
    // it (a genuine re-read), so dedup still works for overlapping reads.
    let page = "A".repeat(3_000);
    let whole = "W".repeat(3_000);
    let mut messages = vec![
        assistant_with_call("c1", "read", r#"{"path":"big.rs","offset":10,"limit":50}"#),
        Message::tool_result(
            &call("c1", "read", r#"{"path":"big.rs","offset":10,"limit":50}"#),
            page,
        ),
        // Open-ended read from line 1 covers [10,60) -> earlier page is stale.
        assistant_with_call("c2", "read", r#"{"path":"big.rs"}"#),
        Message::tool_result(&call("c2", "read", r#"{"path":"big.rs"}"#), whole),
    ];

    let out = prune_tool_results(&mut messages, 0, 1).unwrap();
    assert!(
        is_cleared(&messages[1].content),
        "the earlier page is fully re-covered, so it is stale and cleared"
    );
    assert!(out.cleared_count >= 1);
}

#[test]
fn keep_alive_spares_a_result_mentioned_in_recent_messages() {
    let body = "Z".repeat(3_000);
    let mut messages = vec![
        assistant_with_call("c1", "read", r#"{"path":"important.rs"}"#),
        Message::tool_result(&call("c1", "read", r#"{"path":"important.rs"}"#), body),
        // A later user message references the file by name -> keep alive.
        Message::new(Role::User, "now fix the bug in important.rs please"),
    ];

    // Even with zero recency protection, the mentioned result is spared.
    assert!(prune_tool_results(&mut messages, 0, 1).is_none());
}

#[test]
fn result_shorter_than_its_placeholder_is_left_alone() {
    // A result tinier than the informative placeholder that would replace it
    // yields negative reclaim — clearing it would *grow* the window. Such a
    // candidate is skipped entirely (no real gain), so it is left verbatim.
    let tiny = "ok".to_string();
    let mut messages = vec![Message::tool_result(
        &call("c1", "execute_command", "{}"),
        tiny,
    )];
    assert!(prune_tool_results(&mut messages, 0, 1).is_none());
    assert_eq!(messages[0].content, "ok");
}

#[test]
fn recency_protection_and_min_reclaim_gate() {
    let big = "Z".repeat(3_000);
    let mut messages = vec![Message::tool_result(
        &call("c", "execute_command", "{}"),
        big,
    )];

    // Fully protected by a large recency budget -> None, untouched.
    assert!(prune_tool_results(&mut messages, 10_000, 1).is_none());
    // Reclaim floor larger than anything available -> None, untouched.
    assert!(prune_tool_results(&mut messages, 0, 1_000_000).is_none());
    assert!(!is_cleared(&messages[0].content));
    assert!(!is_truncated(&messages[0].content));
}

#[test]
fn policy_resolves_thresholds_relative_to_window() {
    let policy = CompactionPolicy::default();
    let budget = policy.resolve(200_000);
    assert_eq!(budget.window_tokens, 200_000);
    assert_eq!(budget.prune_threshold_tokens, 130_000); // 65%
    assert_eq!(budget.compaction_threshold_tokens, 170_000); // 85%
    assert_eq!(budget.target_tokens, 50_000); // 25%
    // Pruning trips before full compaction, and compaction leaves a target
    // well below its trigger — the escalation ladder the harness relies on.
    assert!(budget.prune_threshold_tokens < budget.compaction_threshold_tokens);
    assert!(budget.target_tokens < budget.prune_threshold_tokens);
}

#[test]
fn policy_falls_back_for_unknown_window() {
    let policy = CompactionPolicy::default();
    let budget = policy.resolve(0);
    assert_eq!(budget.window_tokens, 32_000);
    assert_eq!(budget.compaction_threshold_tokens, 27_200); // 85% of 32_000
}

#[test]
fn policy_round_trips_through_serde_with_defaults() {
    // A config with no compaction table keeps the documented defaults.
    let policy: CompactionPolicy = toml::from_str("").unwrap();
    assert_eq!(policy, CompactionPolicy::default());

    // Explicit overrides survive a round-trip.
    let toml = r#"
        utilization = 0.9
        target_utilization = 0.2
        prune_utilization = 0.7
        fallback_window_tokens = 64_000
    "#;
    let policy: CompactionPolicy = toml::from_str(toml).unwrap();
    let budget = policy.resolve(100_000);
    assert_eq!(budget.prune_threshold_tokens, 70_000);
    assert_eq!(budget.compaction_threshold_tokens, 90_000);
}

#[test]
fn removed_working_set_keys_are_ignored_for_compatibility() {
    let policy: CompactionPolicy =
        toml::from_str("max_active_tokens = 96000\nprompt_reserve_tokens = 8000\n").unwrap();

    assert_eq!(policy, CompactionPolicy::default());
    let serialized = toml::to_string(&policy).unwrap();
    assert!(!serialized.contains("max_active_tokens"));
    assert!(!serialized.contains("prompt_reserve_tokens"));
}

#[test]
fn default_policy_uses_the_full_large_model_window() {
    let budget = CompactionPolicy::default().resolve(1_000_000);
    assert_eq!(budget.window_tokens, 1_000_000);
    assert_eq!(budget.prune_threshold_tokens, 650_000);
    assert_eq!(budget.compaction_threshold_tokens, 850_000);
    assert_eq!(budget.target_tokens, 250_000);
}

#[test]
fn semantic_json_ignores_transport_syntax_but_keeps_domain_content() {
    let empty = serde_json::json!({"type": "object", "properties": {}});
    let meaningful = serde_json::json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "File to read"}
        },
        "required": ["path"]
    });
    assert!(
        estimate_semantic_json_tokens(&empty) > 0,
        "schema type is semantic"
    );
    assert!(
        estimate_semantic_json_tokens(&meaningful) > estimate_semantic_json_tokens(&empty),
        "property names and descriptions must remain model-visible"
    );
    assert_eq!(estimate_semantic_json_tokens(&serde_json::json!({})), 0);
}

#[test]
fn message_estimate_treats_empty_content_and_empty_arguments_as_framing_only() {
    let mut message = Message::new(Role::Assistant, "");
    message.tool_calls = Some(vec![nuo_tool::ToolCall {
        id: "call_1".into(),
        name: String::new(),
        arguments: "{}".into(),
    }]);
    // No content, no name, empty JSON object: only the per-message (4)
    // and per-tool-call (2) framing overhead remains.
    assert_eq!(estimate_message_tokens(&message), 6);
}

#[test]
fn message_estimate_with_no_content_and_no_calls_is_zero() {
    let message = Message::new(Role::Assistant, "");
    assert_eq!(estimate_message_tokens(&message), 0);
}

#[test]
fn estimate_tokens_excludes_children_recursion_is_consistent() {
    // A message with tool calls: the name (read) + JSON args +
    // content all get counted via the char-class path.
    let mut m = Message::new(Role::Tool, "读取结果：你好");
    m.tool_calls = None;
    let est = crate::estimate_tokens(&[m]);
    // CJK content alone is 6 glyphs; plus the ASCII prefix ~3 tokens.
    assert!(est >= 6, "got {est}");
}

#[test]
fn prior_build_command_is_superseded_by_later_build_command() {
    let call1 = call(
        "c1",
        "execute_command",
        "{\"command\":\"ninja -C build test\"}",
    );
    let call2 = call(
        "c2",
        "execute_command",
        "{\"command\":\"ninja -C build test\"}",
    );
    let log1 = "FAILED: test_embed\nAssertionError: failed\n".repeat(15);
    let log2 = "PASSED: all 134 tests passed\n".repeat(15);
    let mut messages = vec![
        assistant_with_call(
            "c1",
            "execute_command",
            "{\"command\":\"ninja -C build test\"}",
        ),
        Message::tool_result(&call1, log1),
        assistant_with_call(
            "c2",
            "execute_command",
            "{\"command\":\"ninja -C build test\"}",
        ),
        Message::tool_result(&call2, log2),
    ];

    // Zero protect_recent_tokens: call1 is stale because call2 is also a build/test command
    let out = prune_tool_results(&mut messages, 0, 1).unwrap();
    assert_eq!(out.cleared_count, 2); // Both are eligible, earlier is stale and fully cleared
    assert!(messages[1].content.starts_with(CLEARED_TOOL_PREFIX));
}

#[test]
fn prior_build_command_is_invalidated_by_code_mutation() {
    let call1 = call(
        "c1",
        "execute_command",
        "{\"command\":\"ninja -C build test\"}",
    );
    let call2 = call(
        "c2",
        "edit_text",
        "{\"path\":\"src/main.rs\",\"old_string\":\"foo\",\"new_string\":\"bar\"}",
    );
    let log1 = "error: undefined reference to foo in embed.c\n".repeat(15);
    let mut messages = vec![
        assistant_with_call(
            "c1",
            "execute_command",
            "{\"command\":\"ninja -C build test\"}",
        ),
        Message::tool_result(&call1, log1),
        assistant_with_call(
            "c2",
            "edit_text",
            "{\"path\":\"src/main.rs\",\"old_string\":\"foo\",\"new_string\":\"bar\"}",
        ),
        Message::tool_result(&call2, "edited successfully"),
    ];

    let out = prune_tool_results(&mut messages, 0, 1).unwrap();
    assert!(out.cleared_count >= 1);
    assert!(messages[1].content.starts_with(CLEARED_TOOL_PREFIX));
}

#[test]
fn companion_tool_image_is_evicted_on_prune() {
    let call1 = call("c1", "read", "{\"path\":\"screenshot.png\"}");
    let tool_msg = Message::tool_result(&call1, "[image: image/png]");
    let companion = Message::new(Role::User, "Image from screenshot")
        .with_images(vec![nuo_tool::ImagePart {
            mime: "image/png".into(),
            data: "very_large_base64_data".into(),
        }])
        .with_origin(nuo_tool::InjectionOrigin::new(
            nuo_tool::InjectionKind::ToolImage,
        ));
    let mut messages = vec![
        assistant_with_call("c1", "read", "{\"path\":\"screenshot.png\"}"),
        tool_msg,
        companion,
    ];

    let out = prune_tool_results(&mut messages, 0, 1).unwrap();
    assert!(out.cleared_count >= 1);
    assert!(messages[2].images.is_none());
    assert!(messages[2].content.starts_with("[cleared image payload"));
}

#[test]
fn user_uploaded_image_is_evicted_with_artifact_invoice() {
    let user_msg = Message::new(Role::User, "User prompt with visual").with_images(vec![
        nuo_tool::ImagePart {
            mime: "image/png".into(),
            data: "very_large_base64_data".into(),
        },
    ]);
    let assistant_msg = Message::new(Role::Assistant, "I will analyze this.");
    let mut messages = vec![user_msg, assistant_msg];

    let out = prune_tool_results(&mut messages, 0, 1).unwrap();
    assert!(out.cleared_count >= 1);
    assert!(messages[0].images.is_none());
    assert!(messages[0].content.starts_with("User prompt with visual"));
    assert!(
        messages[0]
            .content
            .contains("[Image Offloaded: image/png elided to relieve context pressure]")
    );
    // The Spool invoice carries the CAS Artifact Path (ADR-0093: zero imperative guidance).
    assert!(messages[0].content.contains("Artifact Path"));
    assert!(messages[0].content.contains(".png"));
    assert!(!messages[0].content.contains("Guidance:"));
}

fn tool_with_spool(id: &str, name: &str, body: &str) -> Message {
    Message::tool_result(
        &call(id, name, "{}"),
        format!("[{name} result]:\n{body}\n- Spool File: \"/tmp/nuo/{id}.log\""),
    )
}

#[test]
fn historical_tool_outputs_are_banded_from_the_tail() {
    let newest = tool_with_spool("newest", "read", &"n".repeat(400));
    let warm = tool_with_spool("warm", "execute_command", &"w\n".repeat(2_000));
    let cold = tool_with_spool("cold", "find_files", &"c\n".repeat(500));
    let mut messages = vec![cold, warm, newest];

    let weights = MessageTokenWeights::new();
    let out = project_historical_tool_outputs(&mut messages, 10, 200, &weights);

    // The newest result stays verbatim.
    assert!(messages[2].content.contains(&"n".repeat(400)));
    assert!(!messages[2].content.contains(RECENT_COMPACTED_MARKER));
    // The warm band is frozen, and keeps its re-fetch pointer.
    assert!(messages[1].content.contains(RECENT_COMPACTED_MARKER));
    assert!(
        messages[1]
            .content
            .contains("- Spool File: \"/tmp/nuo/warm.log\"")
    );
    // The cold band is cleared, and keeps its re-fetch pointer.
    assert!(messages[0].content.starts_with(CLEARED_TOOL_PREFIX));
    assert!(
        messages[0]
            .content
            .contains("- Spool File: \"/tmp/nuo/cold.log\"")
    );
    assert_eq!(
        out,
        ToolOutputProjection {
            frozen: 1,
            cleared: 1
        }
    );

    // Re-projecting the same window is a no-op: the shape is final.
    let again = project_historical_tool_outputs(&mut messages, 10, 200, &weights);
    assert!(again.is_empty());
    assert!(messages[1].content.contains(RECENT_COMPACTED_MARKER));
    assert!(messages[0].content.starts_with(CLEARED_TOOL_PREFIX));
}

#[test]
fn freezing_a_large_result_is_idempotent_and_keeps_the_pointer() {
    let body = (0..80)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let original = format!("[read result]:\n{body}\n- Spool File: \"/tmp/a.log\"");
    let frozen = freeze_tool_output(&original);
    assert!(frozen.contains(RECENT_COMPACTED_MARKER));
    assert!(frozen.contains("- Spool File: \"/tmp/a.log\""));
    assert!(frozen.lines().count() < original.lines().count());
    assert_eq!(freeze_tool_output(&frozen), frozen);
}

#[test]
fn a_short_result_is_not_frozen() {
    let original = "[read result]:\nok\n- Spool File: \"/tmp/a.log\"";
    assert_eq!(freeze_tool_output(original), original);
}
