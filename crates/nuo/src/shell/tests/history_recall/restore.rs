//! Session resume backfill: durable messages become transcript components.

use super::*;

#[test]
fn restored_history_hides_harness_messages() {
    assert!(transcript_message_from_core(Message::hidden(Role::User, "internal")).is_none());
    assert!(transcript_message_from_core(Message::new(Role::System, "system")).is_none());
}

#[test]
fn restored_history_uses_command_display_content() {
    let message = Message::new(Role::User, "Expanded internal prompt")
        .with_display_content("/review working-tree");
    let restored = transcript_message_from_core(message).unwrap();
    assert_eq!(restored.raw, "/review working-tree");
}

#[test]
fn restored_user_message_uses_exact_or_legacy_timestamp() {
    let exact = Message::new(Role::User, "hi").with_sent_at_ms(1_700_000_000_123);
    let restored = transcript_message_from_core(exact).unwrap();
    assert_eq!(restored.sent_at_ms, Some(1_700_000_000_123));

    let mut legacy = Message::new(Role::User, "hi");
    legacy.sent_at_ms = None;
    legacy.timestamp = Some(1_700_000_001);
    let restored = transcript_message_from_core(legacy).unwrap();
    assert_eq!(restored.sent_at_ms, Some(1_700_000_001_000));
}

#[test]
fn restored_assistant_tool_step_uses_message_timestamp_for_turn_header() {
    let mut assistant = Message::new(Role::Assistant, "");
    assistant.timestamp = Some(1_700_000_002);
    assistant.tool_calls = Some(vec![ToolCall {
        id: "call".to_string(),
        name: "read_text".to_string(),
        arguments: r#"{"path":"README.md"}"#.to_string(),
    }]);

    let restored = transcript_messages_from_core(vec![assistant], &config::TuiConfig::default());
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].sent_at_ms, Some(1_700_000_002_000));
    assert_eq!(restored[0].round, Some(1));
    assert_eq!(restored[0].turn, Some(1));
}

#[test]
fn restored_reasoning_component_carries_the_turn_send_time() {
    // A reasoning trace is a component of its assistant message's turn, so it
    // must carry that message's send time. When thinking is the turn's *first*
    // component it owns the band header — an unstamped trace silently dropped
    // the header's `HH:MM` on every resumed reasoning turn.
    let mut assistant = Message::new(Role::Assistant, "answer");
    assistant.timestamp = Some(1_700_000_002);
    assistant.reasoning_content = Some("inspect first".to_string());

    let restored = transcript_messages_from_core(vec![assistant], &config::TuiConfig::default());
    assert!(restored[0].is_reasoning());
    assert_eq!(
        restored[0].sent_at_ms,
        Some(1_700_000_002_000),
        "a restored reasoning trace must carry its turn's send time"
    );
}

#[test]
fn restored_assistant_components_share_their_round_and_turn() {
    let mut assistant = Message::new(Role::Assistant, "continue");
    assistant.reasoning_content = Some("inspect first".to_string());
    assistant.tool_calls = Some(vec![ToolCall {
        id: "call".to_string(),
        name: "read_text".to_string(),
        arguments: r#"{"path":"README.md"}"#.to_string(),
    }]);

    let restored = transcript_messages_from_core(vec![assistant], &config::TuiConfig::default());
    assert_eq!(restored.len(), 3);
    assert!(restored[0].is_reasoning());
    assert!(restored[1].is_tool_step());
    assert_eq!(restored[2].role, Role::Assistant);
    assert!(restored.iter().all(|message| message.round == Some(1)));
    assert!(restored.iter().all(|message| message.turn == Some(1)));
}

#[test]
fn restored_user_message_origin_inferred_from_shape() {
    use nuo_view::model::document::UserMessageOrigin;
    // A genuine chat prompt: no display_content, no leading `!`.
    let chat = transcript_message_from_core(Message::new(Role::User, "fix the bug")).unwrap();
    assert_eq!(chat.origin, UserMessageOrigin::Chat);

    // A slash command carries a `display_content` whose text is the literal
    // `/cmd` (its real content is the harness-expanded form) → Slash.
    let slash = Message::new(Role::User, "expanded pursue body")
        .with_display_content("/pursue ship the release");
    let slash = transcript_message_from_core(slash).unwrap();
    assert_eq!(slash.origin, UserMessageOrigin::Slash);

    // A prompt starting with `!` is a normal chat prompt
    let exclamation = transcript_message_from_core(Message::new(Role::User, "!ls -la")).unwrap();
    assert_eq!(exclamation.origin, UserMessageOrigin::Chat);

    // A genuine prompt that merely *starts* with `/` (no display_content) is
    // NOT misclassified as a slash command — e.g. "/etc is a path" stays Chat.
    let path_like =
        transcript_message_from_core(Message::new(Role::User, "/etc is a path")).unwrap();
    assert_eq!(path_like.origin, UserMessageOrigin::Chat);
}

#[test]
fn restored_user_insert_keeps_mid_round_origin_without_opening_a_turn() {
    use nuo_view::model::document::UserMessageOrigin;
    let first = Message::new(Role::Assistant, "first answer");
    let inserted = Message::new(Role::User, "one more constraint").with_origin(
        nuo_protocol::InjectionOrigin::new(nuo_protocol::InjectionKind::UserSteer),
    );
    let second = Message::new(Role::Assistant, "revised answer");

    let restored =
        transcript_messages_from_core(vec![first, inserted, second], &config::TuiConfig::default());
    assert_eq!(restored[1].origin, UserMessageOrigin::Steer);
    assert_eq!(restored[1].round, Some(1));
    assert_eq!(restored[1].turn, None);
    assert_eq!(restored[2].round, Some(1));
    assert_eq!(restored[2].turn, Some(2));
}

#[test]
fn restored_command_echo_origin_from_durable_provenance() {
    // ADR-0050: durable slash/shell echoes are persisted as
    // `Message::command_echo` — a visible user message carrying the
    // `CommandEcho` provenance. On resume the stored origin is consulted
    // FIRST (ahead of the shape heuristic), so an echo whose text lacks the
    // `display_content` / `!` shape signals is still classified as a
    // non-driving command, never as the round's driving prompt.
    use nuo_view::model::document::UserMessageOrigin;

    // A slash echo: content is the literal `/cmd`, no display_content. The
    // shape heuristic alone would misread this (no display_content → fall to
    // the `!` check → fail → Chat). The durable origin must win.
    let slash_echo = Message::command_echo("/pursue ship it");
    assert!(slash_echo.is_command_echo());
    let restored = transcript_message_from_core(slash_echo).unwrap();
    assert_eq!(
        restored.origin,
        UserMessageOrigin::Slash,
        "durable echo provenance must classify as Slash, not Chat"
    );

    // A shell echo: content is `!cmd`. Even though the `!` shape heuristic
    // would also catch it, the origin-first path must handle it too.
    let shell_echo = Message::command_echo("!ls -la");
    let restored_shell = transcript_message_from_core(shell_echo).unwrap();
    assert_eq!(restored_shell.origin, UserMessageOrigin::Slash);
}

/// A restored slash/shell echo folds into the command ledger
/// projection — the invocation renders once, on the `⌘` command component,
/// never twice (a `▌ cmd` user bubble *and* a command row). The echo is
/// dropped from the dialogue before merge; the ledger row keeps the record.
#[test]
fn restored_slash_echoes_fold_into_command_components() {
    use nuo_protocol::Role;
    use nuo_view::model::document::UserMessageOrigin;

    let restored = transcript_messages_from_core(
        vec![
            Message::new(Role::User, "hello"), // a real prompt: survives
            Message::new(Role::Assistant, "hi"),
            // A durable command echo (the legacy live path persisted these).
            Message::command_echo("/pursue ship it"),
            // A display-content slash shape (legacy sessions pre-ADR-0050).
            {
                let mut m = Message::new(Role::User, "expanded prompt text");
                m.display_content = Some("/delegate on".to_string());
                m
            },
            Message::new(Role::User, "and me too"), // another real prompt
        ],
        &crate::shell::config::TuiConfig::default(),
    );

    let raws: Vec<&str> = restored.iter().map(|m| m.raw.as_str()).collect();
    assert_eq!(
        raws,
        vec!["hello", "hi", "and me too"],
        "command echoes must not render as user bubbles; the ledger row owns the invocation"
    );
    // The projection builder still classifies them correctly when asked
    // directly (used by Activity-modal prompt gating) — the fold happens at
    // the list level, not by breaking classification.
    let echo = transcript_message_from_core(Message::command_echo("/pursue ship it")).unwrap();
    assert_eq!(echo.origin, UserMessageOrigin::Slash);
}

#[test]
fn restored_assistant_carries_provider_and_model_attribution() {
    // A persisted assistant message stamped by the harness keeps its
    // provider/model so a resumed session that mixed models stays
    // traceable in the transcript.
    let message = Message::new(Role::Assistant, "Hello from kimi")
        .with_attribution("kimi-code", "kimi-k2.7-code");
    let restored = transcript_message_from_core(message).unwrap();
    assert_eq!(restored.provider.as_deref(), Some("kimi-code"));
    assert_eq!(restored.model.as_deref(), Some("kimi-k2.7-code"));
    assert_eq!(
        restored.attribution_label(),
        Some(("kimi-code".to_string(), "kimi-k2.7-code".to_string()))
    );
    // No persisted effort → no depth chip on restore.
    assert_eq!(restored.effort, None);

    // The persisted reasoning depth round-trips with the attribution.
    let mut message = Message::new(Role::Assistant, "deep thought");
    message.effort = Some("high".to_string());
    let restored = transcript_message_from_core(message).unwrap();
    assert_eq!(restored.effort.as_deref(), Some("high"));
    assert_eq!(restored.attribution_label().map(|(_, m)| m), None::<String>);

    // A plain user message carries no attribution.
    let user = transcript_message_from_core(Message::new(Role::User, "hi")).unwrap();
    assert!(user.attribution_label().is_none());

    // A provider without an id still surfaces the model alone.
    let model_only = Message::new(Role::Assistant, "x").with_attribution("", "gpt-4o");
    let restored = transcript_message_from_core(model_only).unwrap();
    assert_eq!(
        restored.attribution_label(),
        Some((String::new(), "gpt-4o".to_string()))
    );
}

#[test]
fn restored_reasoning_is_not_shown_as_running() {
    let message = Message {
        role: Role::Assistant,
        content: String::new(),
        content_blob: None,
        display_content: None,
        reasoning_content: Some("step-by-step reasoning".to_string()),
        provider_meta: None,
        tool_calls: None,
        tool_call_id: None,
        images: None,
        provider: None,
        model: None,
        effort: None,
        hidden: false,
        children: None,
        subagent_meta: None,
        origin: None,
        timestamp: None,
        sent_at_ms: None,
    };

    let restored = transcript_messages_from_core(vec![message], &config::TuiConfig::default());
    assert_eq!(restored.len(), 1);
    let thinking = &restored[0];
    assert!(thinking.is_reasoning());
    assert_eq!(thinking.turn, Some(1));
    // A finished reasoning block must not be rendered with a live spinner.
    assert!(
        thinking.reasoning_summary().unwrap().contains("0ms"),
        "restored thinking should have a finished duration, got {:?}",
        thinking.reasoning_summary()
    );
}

#[test]
fn restored_native_tool_calls_are_visible() {
    let message = Message {
        role: Role::Assistant,
        content: String::new(),
        content_blob: None,
        display_content: None,
        reasoning_content: None,
        provider_meta: None,
        tool_calls: Some(vec![ToolCall {
            id: "call".to_string(),
            name: "read_text".to_string(),
            arguments: "{\"path\":\"README.md\"}".to_string(),
        }]),
        tool_call_id: None,
        images: None,
        provider: None,
        model: None,
        effort: None,
        hidden: false,
        children: None,
        subagent_meta: None,
        origin: None,
        timestamp: None,
        sent_at_ms: None,
    };

    let restored = transcript_message_from_core(message).unwrap();
    assert!(restored.raw.contains("read_text"));
}

#[test]
fn restored_tool_results_merge_into_steps_in_fifo_order() {
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: String::new(),
            content_blob: None,
            display_content: None,
            reasoning_content: None,
            provider_meta: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "one".to_string(),
                    name: "read_text".to_string(),
                    arguments: r#"{"path":"one"}"#.to_string(),
                },
                ToolCall {
                    id: "two".to_string(),
                    name: "read_text".to_string(),
                    arguments: r#"{"path":"two"}"#.to_string(),
                },
            ]),
            tool_call_id: None,
            images: None,
            provider: None,
            model: None,
            effort: None,
            hidden: false,
            children: None,
            subagent_meta: None,
            origin: None,
            timestamp: None,
            sent_at_ms: None,
        },
        Message::tool_result(
            &ToolCall {
                id: "one".to_string(),
                name: "read_text".to_string(),
                arguments: String::new(),
            },
            "[read_text result]:\nfirst",
        ),
        Message::tool_result(
            &ToolCall {
                id: "two".to_string(),
                name: "read_text".to_string(),
                arguments: String::new(),
            },
            "[read_text result]:\nsecond",
        ),
    ];

    let mut restored = transcript_messages_from_core(messages, &config::TuiConfig::default());
    assert_eq!(restored.len(), 2);
    restored[0].set_tool_step_expanded(true);
    restored[1].set_tool_step_expanded(true);
    assert!(restored[0].raw.contains("first"));
    assert!(!restored[0].raw.contains("second"));
    assert!(restored[1].raw.contains("second"));
}

#[test]
fn restored_tool_steps_carry_the_call_id_and_pair_by_identity() {
    // ADR-0166 `[INV-TOOL-ID-01]`: a restored step's id is the call id, never
    // the tool name, so two same-named calls stay distinct. Pairing is by the
    // tool message's `tool_call_id`, so it survives a reordered result stream.
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: String::new(),
            content_blob: None,
            display_content: None,
            reasoning_content: None,
            provider_meta: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "call_a".to_string(),
                    name: "read_text".to_string(),
                    arguments: r#"{"path":"a"}"#.to_string(),
                },
                ToolCall {
                    id: "call_b".to_string(),
                    name: "read_text".to_string(),
                    arguments: r#"{"path":"b"}"#.to_string(),
                },
            ]),
            tool_call_id: None,
            images: None,
            provider: None,
            model: None,
            effort: None,
            hidden: false,
            children: None,
            subagent_meta: None,
            origin: None,
            timestamp: None,
            sent_at_ms: None,
        },
        // Results deliberately arrive out of call order.
        Message::tool_result(
            &ToolCall {
                id: "call_b".to_string(),
                name: "read_text".to_string(),
                arguments: String::new(),
            },
            "[read_text result]:\nsecond",
        ),
        Message::tool_result(
            &ToolCall {
                id: "call_a".to_string(),
                name: "read_text".to_string(),
                arguments: String::new(),
            },
            "[read_text result]:\nfirst",
        ),
    ];

    let mut restored = transcript_messages_from_core(messages, &config::TuiConfig::default());
    assert_eq!(restored.len(), 2);
    assert_eq!(restored[0].tool_step_call_id(), Some("call_a"));
    assert_eq!(restored[1].tool_step_call_id(), Some("call_b"));
    for message in &mut restored {
        message.set_tool_step_expanded(true);
    }
    assert!(restored[0].raw.contains("first"));
    assert!(!restored[0].raw.contains("second"));
    assert!(restored[1].raw.contains("second"));
}
