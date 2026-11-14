use super::*;

#[test]
fn clone_without_children_drops_the_nested_transcript() {
    let inner = Message::new(Role::Assistant, "subagent note");
    let message = Message::tool_result(&ToolCall::new("c", "task", "{}"), "summary")
        .with_children(vec![inner])
        .with_subagent_meta(SubagentMeta::default());
    let projected = message.clone_without_children();
    assert!(projected.children.is_none());
    assert!(projected.subagent_meta.is_none());
    assert_eq!(projected.content, message.content);
    assert_eq!(projected.tool_call_id, message.tool_call_id);
}

#[test]
fn message_without_children_omits_field_in_json() {
    // Legacy compatibility: a normal Message must still serialise without
    // a `children` key so old consumers / tests that match the literal
    // JSON keep working.
    let m = Message::new(Role::User, "hi");
    let json = serde_json::to_string(&m).unwrap();
    assert!(
        !json.contains("children"),
        "json should omit children: {json}"
    );
}

#[test]
fn legacy_json_without_children_deserialises_to_none() {
    // Pre-Phase-3 snapshots must load unchanged.
    let json = r#"{"role":"User","content":"hi","hidden":false}"#;
    let m: Message = serde_json::from_str(json).unwrap();
    assert_eq!(m.content, "hi");
    assert!(m.children.is_none());
    assert!(m.sent_at_ms.is_none());
}

#[test]
fn children_round_trip_through_json() {
    // A tool result with a subagent transcript must survive a
    // serialise → deserialise round trip with the nested messages intact,
    // including their own nested children (sub-subagents).
    let call = ToolCall {
        id: "call_root".to_string(),
        name: "subagent".to_string(),
        arguments: "{}".to_string(),
    };
    let nested_call = ToolCall {
        id: "call_inner".to_string(),
        name: "search_text".to_string(),
        arguments: r#"{"pattern":"foo"}"#.to_string(),
    };
    let inner_child = Message::new(Role::Tool, "match at a.rs:1")
        .with_children(vec![Message::new(Role::Assistant, "deeply nested note")]);
    let subagent_transcript = vec![
        Message::new(Role::System, "subagent system"),
        Message::new(Role::User, "subagent task"),
        Message {
            role: Role::Assistant,
            content: String::new(),
            content_blob: None,
            display_content: None,
            reasoning_content: None,
            provider_meta: None,
            tool_calls: Some(vec![nested_call]),
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
        inner_child,
    ];
    let parent =
        Message::tool_result(&call, "[task result]:\nfound it").with_children(subagent_transcript);

    let json = serde_json::to_string_pretty(&parent).unwrap();
    let restored: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.role, Role::Tool);
    assert_eq!(restored.tool_call_id.as_deref(), Some("call_root"));
    let children = restored.children.expect("children round-trip");
    assert_eq!(children.len(), 4);
    // The grep call inside the subagent kept its tool_calls.
    assert!(children[2].tool_calls.is_some());
    // The inner Tool message kept its own nested children (sub-subagent).
    let inner = &children[3];
    assert_eq!(inner.role, Role::Tool);
    assert!(
        inner.children.is_some(),
        "sub-subagent children must survive"
    );
    assert_eq!(inner.children.as_ref().unwrap().len(), 1);
}

#[test]
fn with_children_empty_vec_is_none() {
    let call = ToolCall {
        id: "c".to_string(),
        name: "subagent".to_string(),
        arguments: "{}".to_string(),
    };
    let m = Message::tool_result(&call, "x").with_children(Vec::new());
    assert!(
        m.children.is_none(),
        "empty children should collapse to None"
    );
}

#[test]
fn to_wire_strips_children_and_sidecars_keeps_wire_fields() {
    // A Tool-role subagent result carries a heavy nested transcript + meta +
    // attribution + origin. `to_wire` must drop all of those (the provider
    // never sees them) while keeping role/content/tool_call_id. This is the
    // contract `/debug preview` relies on to dump the real request body
    // rather than the internal `Message` struct that also carries
    // durable-session sidecars.
    let call = ToolCall {
        id: "c".to_string(),
        name: "subagent".to_string(),
        arguments: "{}".to_string(),
    };
    let m = Message::tool_result(&call, "[subagent result]: summary")
        .with_children(vec![Message::new(
            Role::Assistant,
            "subagent internal turn",
        )])
        .with_subagent_meta(SubagentMeta::default())
        .with_attribution("kimi", "kimi-code")
        .with_origin(InjectionOrigin::new(InjectionKind::SubagentSteer))
        .with_sent_at_ms(1_700_000_000_123);

    let w = m.to_wire();
    assert_eq!(w.role, Role::Tool);
    assert_eq!(w.content, "[subagent result]: summary");
    assert_eq!(w.tool_call_id.as_deref(), Some("c"));
    assert!(w.children.is_none(), "children must be stripped");
    assert!(w.subagent_meta.is_none(), "subagent_meta must be stripped");
    assert!(
        w.provider.is_none() && w.model.is_none(),
        "attribution stripped"
    );
    assert!(w.origin.is_none(), "origin stripped");
    assert!(w.sent_at_ms.is_none(), "UI timestamp stripped");
    assert!(!w.hidden, "hidden reset to false");
    assert!(
        w.content_blob.is_none() && w.display_content.is_none(),
        "storage/UI sidecars stripped"
    );
}

#[test]
fn semantic_wire_eq_matches_to_wire_equality() {
    let m1 = Message::new(Role::User, "hello")
        .with_attribution("kimi", "kimi-code")
        .with_sent_at_ms(12345);
    let m2 = Message::new(Role::User, "hello")
        .with_attribution("openai", "gpt-4o")
        .with_sent_at_ms(67890);
    let m3 = Message::new(Role::User, "hello world");

    assert!(m1.semantic_wire_eq(&m2));
    assert_eq!(m1.to_wire() == m2.to_wire(), m1.semantic_wire_eq(&m2));
    assert!(!m1.semantic_wire_eq(&m3));
    assert_eq!(m1.to_wire() == m3.to_wire(), m1.semantic_wire_eq(&m3));
}

#[test]
fn default_message_omits_origin_key() {
    // A genuine message (user input / assistant / tool result) must
    // serialise WITHOUT an `origin` key, so the wire shape is unchanged
    // and legacy consumers / snapshot matchers keep working. Mirrors the
    // `children` compat contract.
    let m = Message::new(Role::User, "hi");
    let json = serde_json::to_string(&m).unwrap();
    assert!(
        !json.contains("origin"),
        "default message must omit origin: {json}"
    );
    assert!(
        !json.contains("injection"),
        "default message must omit injection fields: {json}"
    );
}

#[test]
fn legacy_json_without_origin_loads_as_none() {
    // A pre-C4 snapshot / event-log line has no `origin` key and must
    // deserialise to `origin: None`. This is the load-side of the
    // backward-compat contract.
    let json = r#"{"role":"User","content":"hi","hidden":false}"#;
    let m: Message = serde_json::from_str(json).unwrap();
    assert_eq!(m.content, "hi");
    assert!(m.origin.is_none());
}

#[test]
fn injection_origin_round_trips() {
    // A stamped origin must survive a serialise → deserialise round trip
    // with its kind, reason, and the nested HookEventKind intact. This is
    // the contract that makes the persisted transcript faithfully
    // reconstruct injection provenance.
    use super::HookEventKind;
    let msg = Message::injected(
        Role::User,
        "remember X",
        InjectionOrigin::new(InjectionKind::Hook(HookEventKind::PostToolUse))
            .with_reason("my_hook.sh"),
    );
    let json = serde_json::to_string(&msg).unwrap();
    // The origin object is present in the wire form.
    assert!(json.contains("\"origin\""), "origin must serialise: {json}");
    assert!(
        json.contains("\"reason\":\"my_hook.sh\""),
        "reason must serialise: {json}"
    );
    let restored: Message = serde_json::from_str(&json).unwrap();
    let origin = restored.origin.expect("origin round-trip");
    assert_eq!(origin.kind, InjectionKind::Hook(HookEventKind::PostToolUse));
    assert_eq!(origin.reason.as_deref(), Some("my_hook.sh"));
}

#[test]
fn every_injection_kind_serialises_distinctly() {
    // The closed classifier must serialise to distinct wire forms so a
    // persisted transcript can discriminate injection sources without
    // ambiguity. Regression guard: adding a variant without a distinct
    // serde tag would silently collapse provenance. We compare the full
    // serialised `kind` (not just a prefix) because `Hook(HookEventKind)`
    // serialises as a map `{"hook":"session_start"}` while unit variants
    // serialise as a bare string.
    use super::HookEventKind;
    let cases: Vec<InjectionKind> = vec![
        InjectionKind::Hook(HookEventKind::SessionStart),
        InjectionKind::Hook(HookEventKind::PostToolUse),
        InjectionKind::Hook(HookEventKind::Stop),
        InjectionKind::Hook(HookEventKind::Turn),
        InjectionKind::Hook(HookEventKind::TurnStart),
        InjectionKind::InterAgent,
        InjectionKind::SubagentSteer,
        InjectionKind::SubagentTask,
        InjectionKind::SessionReviewInput,
        InjectionKind::ImplicitSkill,
        InjectionKind::SystemPrompt,
        InjectionKind::CompactionCheckpoint,
        InjectionKind::HiddenRoundInput,
        InjectionKind::LoopReviewNudge,
        InjectionKind::CommandEcho,
        InjectionKind::ToolImage,
        InjectionKind::SystemReminder,
        InjectionKind::UntrustedDirective,
    ];
    let mut forms = Vec::new();
    for kind in cases {
        // Round-trip each kind in isolation.
        let json = serde_json::to_string(&kind).unwrap();
        let restored: InjectionKind = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, kind, "kind {kind:?} must round-trip");
        forms.push(json);
    }
    let mut sorted = forms.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        forms.len(),
        "injection kinds must serialise to distinct wire forms: {forms:?}"
    );
}

#[test]
fn hidden_round_input_writes_canonical_name_and_reads_legacy_name() {
    let legacy: InjectionKind = serde_json::from_str("\"hidden_turn_input\"").unwrap();
    assert_eq!(legacy, InjectionKind::HiddenRoundInput);
    assert_eq!(
        serde_json::to_string(&legacy).unwrap(),
        "\"hidden_round_input\""
    );
}

#[test]
fn injected_constructor_stamps_origin_and_hidden() {
    // `Message::injected` must set BOTH hidden=true (display contract) and
    // origin=Some (provenance contract). The two are orthogonal: hidden
    // governs visibility, origin governs "why is this here".
    let m = Message::injected(
        Role::User,
        "nudge",
        InjectionOrigin::new(InjectionKind::LoopReviewNudge),
    );
    assert!(m.hidden, "injected message must be hidden");
    assert!(m.origin.is_some(), "injected message must carry origin");
    assert_eq!(
        m.origin.as_ref().unwrap().kind,
        InjectionKind::LoopReviewNudge
    );
}

#[test]
fn command_echo_is_visible_and_non_driving() {
    // A command echo is VISIBLE (hidden=false, so it shows on resume/export)
    // yet non-driving (is_command_echo=true, so it is projected out before
    // the wire). It must not be conflated with an injected/hidden message,
    // whose `hidden` axis means the opposite half (ADR-0050).
    let m = Message::command_echo("/session list");
    assert!(!m.hidden, "command echo must be visible on resume/export");
    assert_eq!(m.role, Role::User);
    assert!(m.is_command_echo());
    // Round-trips with provenance intact (the durable record must survive).
    let restored: Message = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert!(
        restored.is_command_echo(),
        "echo origin survives round-trip"
    );
    // A plain user message is not an echo.
    assert!(!Message::new(Role::User, "hello").is_command_echo());
    // An injected (hidden) message is not an echo even though it has origin.
    assert!(
        !Message::injected(
            Role::User,
            "x",
            InjectionOrigin::new(InjectionKind::HiddenRoundInput)
        )
        .is_command_echo()
    );
}
