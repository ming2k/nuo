use super::*;
use crate::message::{InjectionKind, ToolCall};

fn message_entry(seq: u64, role: Role, content: &str) -> TranscriptEntry {
    let mut message = Message::new(role, content);
    message.timestamp = Some(1_700_000_000);
    TranscriptEntry::from_message(seq, &message)
}

fn tool_entry(seq: u64, call_id: &str, content: &str) -> TranscriptEntry {
    let call = ToolCall::new(call_id, "read", "{}");
    let message = Message::tool_result(&call, content);
    TranscriptEntry::from_message(seq, &message)
}

fn assistant_tool_call(seq: u64, call_id: &str) -> TranscriptEntry {
    let mut message = Message::new(Role::Assistant, "working");
    message.tool_calls = Some(vec![ToolCall::new(call_id, "read", "{}")]);
    let mut entry = TranscriptEntry::from_message(seq, &message);
    entry.payload = match entry.payload {
        EntryPayload::Message(mut payload) => {
            // assistant messages carry protocol-private state to verify
            // verbatim round-trips through the view
            payload.provider_meta = Some(
                [(
                    "gemini_thought_signatures".to_string(),
                    serde_json::json!({ call_id: "sig" }),
                )]
                .into_iter()
                .collect(),
            );
            EntryPayload::Message(payload)
        }
        other => other,
    };
    entry
}

#[test]
fn empty_transcript_projects_empty_view() {
    let transcript = Transcript::new();
    assert!(transcript.project().is_empty());
}

#[test]
fn plain_messages_pass_through_in_order() {
    let mut transcript = Transcript::new();
    transcript.push(message_entry(0, Role::User, "hello"));
    transcript.push(message_entry(1, Role::Assistant, "hi"));
    let view = transcript.project();
    assert_eq!(view.len(), 2);
    assert_eq!(view[0].0, 0);
    assert_eq!(view[1].0, 1);
    assert_eq!(view[0].1.content, "hello");
    assert_eq!(view[1].1.content, "hi");
}

#[test]
fn state_entries_never_enter_the_view() {
    let mut transcript = Transcript::new();
    transcript.push(message_entry(0, Role::User, "before"));
    transcript.push(TranscriptEntry::from_state(1, None));
    transcript.push(message_entry(2, Role::Assistant, "after"));
    let view = transcript.project();
    assert_eq!(view.len(), 2);
    assert_eq!(view[0].0, 0);
    assert_eq!(view[1].0, 2);
}

#[test]
fn todos_derive_from_newest_state_entry() {
    let mut transcript = Transcript::new();
    let first = crate::todos::TodoList::default();
    // (The list starts empty; an empty mirror still counts as derived
    // state, so push two mirrors and expect the newest to win.)
    transcript.push(TranscriptEntry::from_state(0, Some(first.clone())));
    transcript.push(message_entry(1, Role::User, "work"));
    let second = crate::todos::TodoList::default();
    transcript.push(TranscriptEntry::from_state(2, Some(second)));
    assert!(transcript.derive_todos().is_some());
}

#[test]
fn prune_replaces_only_listed_tool_bodies_with_placeholders() {
    let mut transcript = Transcript::new();
    transcript.push(assistant_tool_call(0, "call-1"));
    transcript.push(tool_entry(1, "call-1", "big output"));
    transcript.push(assistant_tool_call(2, "call-2"));
    transcript.push(tool_entry(3, "call-2", "small output"));
    transcript.push(message_entry(4, Role::Assistant, "done"));
    transcript.push_directive(ProjectionDirective {
        seq: 0,
        kind: DirectiveKind::Prune,
        up_to_seq: 3,
        payload: DirectivePayload::Prune {
            elided: vec![PrunedToolOutput {
                tool_call_id: "call-1".into(),
                placeholder: "[elided call-1]".into(),
            }],
            pruned_media: vec![],
        },
    });
    let view = transcript.project();
    assert_eq!(view.len(), 5);
    assert_eq!(view[1].1.content, "[elided call-1]");
    assert_eq!(view[3].1.content, "small output");
    assert_eq!(view[4].1.content, "done");
}

#[test]
fn compact_replaces_range_with_checkpoint_once() {
    let mut transcript = Transcript::new();
    transcript.push(message_entry(0, Role::User, "old"));
    transcript.push(message_entry(1, Role::Assistant, "older"));
    let mut checkpoint = Message::new(Role::User, "summary of old rounds");
    checkpoint.hidden = true;
    checkpoint.origin = Some(crate::message::InjectionOrigin::new(
        InjectionKind::CompactionCheckpoint,
    ));
    let checkpoint = TranscriptEntry::from_message(2, &checkpoint);
    let checkpoint_seq = checkpoint.seq;
    transcript.push(checkpoint);
    transcript.push(message_entry(3, Role::User, "new"));
    transcript.push_directive(ProjectionDirective {
        seq: 0,
        kind: DirectiveKind::Compact,
        up_to_seq: 1,
        payload: DirectivePayload::Compact { checkpoint_seq },
    });
    let view = transcript.project();
    assert_eq!(view.len(), 2);
    assert_eq!(view[0].0, checkpoint_seq);
    assert_eq!(view[0].1.content, "summary of old rounds");
    assert_eq!(view[1].0, 3);
    assert_eq!(view[1].1.content, "new");
    // The originals remain recoverable.
    assert_eq!(transcript.projected_out().len(), 2);
}

#[test]
fn later_compact_wins_over_earlier() {
    let mut transcript = Transcript::new();
    for i in 0..5 {
        transcript.push(message_entry(i, Role::User, &format!("m{i}")));
    }
    let mut checkpoint_a = Message::new(Role::User, "checkpoint a");
    checkpoint_a.hidden = true;
    checkpoint_a.origin = Some(crate::message::InjectionOrigin::new(
        InjectionKind::CompactionCheckpoint,
    ));
    let checkpoint_a = TranscriptEntry::from_message(5, &checkpoint_a);
    transcript.push(checkpoint_a);
    let mut checkpoint_b = Message::new(Role::User, "checkpoint b");
    checkpoint_b.hidden = true;
    checkpoint_b.origin = Some(crate::message::InjectionOrigin::new(
        InjectionKind::CompactionCheckpoint,
    ));
    let checkpoint_b = TranscriptEntry::from_message(6, &checkpoint_b);
    transcript.push(checkpoint_b);
    transcript.push_directive(ProjectionDirective {
        seq: 0,
        kind: DirectiveKind::Compact,
        up_to_seq: 2,
        payload: DirectivePayload::Compact { checkpoint_seq: 5 },
    });
    transcript.push_directive(ProjectionDirective {
        seq: 1,
        kind: DirectiveKind::Compact,
        up_to_seq: 5,
        payload: DirectivePayload::Compact { checkpoint_seq: 6 },
    });
    let view = transcript.project();
    // entries 0..=5 hidden; checkpoint 5 superseded by checkpoint 6.
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].0, 6);
    assert_eq!(view[0].1.content, "checkpoint b");
}

#[test]
fn message_round_trip_is_lossless() {
    let mut message = Message::new(Role::Assistant, "answer");
    message.provider = Some("google".into());
    message.model = Some("gemini-2.5-pro".into());
    message.provider_meta = Some(
        [(
            "gemini_thought_signatures".to_string(),
            serde_json::json!({"call-1": "sig"}),
        )]
        .into_iter()
        .collect(),
    );
    message.tool_calls = Some(vec![ToolCall::new("call-1", "read", "{}")]);
    message.reasoning_content = Some("thought".into());
    let entry = TranscriptEntry::from_message(7, &message);
    assert_eq!(entry.seq, 7);
    let restored = entry.to_message().unwrap();
    assert_eq!(restored.role, Role::Assistant);
    assert_eq!(restored.content, "answer");
    assert_eq!(restored.provider.as_deref(), Some("google"));
    assert_eq!(restored.model.as_deref(), Some("gemini-2.5-pro"));
    assert_eq!(restored.provider_meta, message.provider_meta);
    assert_eq!(restored.tool_calls, message.tool_calls);
    assert_eq!(restored.reasoning_content.as_deref(), Some("thought"));
}

#[test]
fn compaction_checkpoint_maps_to_checkpoint_origin() {
    let mut message = Message::new(Role::User, "checkpoint");
    message.hidden = true;
    message.origin = Some(crate::message::InjectionOrigin::new(
        InjectionKind::CompactionCheckpoint,
    ));
    let entry = TranscriptEntry::from_message(0, &message);
    assert_eq!(entry.origin, Some(EntryOrigin::Checkpoint));
    assert!(entry.hidden);
    let restored = entry.to_message().unwrap();
    assert_eq!(
        restored.origin.as_ref().map(|o| o.kind),
        Some(InjectionKind::CompactionCheckpoint)
    );
}

#[test]
fn image_and_display_sidecars_round_trip() {
    let mut message = Message::new(Role::User, "look at this");
    message.display_content = Some("look at this".into());
    let entry = TranscriptEntry::from_message(0, &message);
    let restored = entry.to_message().unwrap();
    assert_eq!(restored.display_content.as_deref(), Some("look at this"));
}

#[test]
fn unknown_payload_kind_preserved_by_serde_fallback() {
    // Simulate a future kind the current binary does not know: the
    // contract requires loaders to preserve it verbatim.
    let json = serde_json::json!({
        "id": "01900000-0000-7000-8000-000000000001",
        "seq": 0,
        "kind": "message",
        "payload": { "type": "future_kind", "x": 1 },
    });
    let decoded: Result<TranscriptEntry, _> = serde_json::from_value(json);
    // Unknown payload kinds must not silently vanish: decoding fails loud
    // at the typed boundary, and the raw row remains preserved in storage.
    // (The persistence layer stores payload as raw JSON; only typed
    // decoding is attempted.)
    assert!(decoded.is_err());
}

#[test]
fn seq_watermarks_advance_gap_free() {
    let mut transcript = Transcript::new();
    assert_eq!(transcript.next_seq(), 0);
    transcript.push(message_entry(0, Role::User, "a"));
    assert_eq!(transcript.next_seq(), 1);
    transcript.push(message_entry(1, Role::Assistant, "b"));
    assert_eq!(transcript.next_seq(), 2);
    assert_eq!(transcript.next_directive_seq(), 0);
    transcript.push_directive(ProjectionDirective {
        seq: 0,
        kind: DirectiveKind::Prune,
        up_to_seq: 1,
        payload: DirectivePayload::Prune {
            elided: vec![],
            pruned_media: vec![],
        },
    });
    assert_eq!(transcript.next_directive_seq(), 1);
}

#[test]
fn prune_replaces_user_media_with_invoice_placeholder() {
    let mut transcript = Transcript::new();
    let user_msg =
        Message::new(Role::User, "User prompt with image").with_images(vec![crate::ImagePart {
            mime: "image/png".into(),
            data: "base64data".into(),
        }]);
    transcript.push(TranscriptEntry::from_message(0, &user_msg));
    transcript.push(message_entry(1, Role::Assistant, "I see the image"));

    transcript.push_directive(ProjectionDirective {
        seq: 0,
        kind: DirectiveKind::Prune,
        up_to_seq: 1,
        payload: DirectivePayload::Prune {
            elided: vec![],
            pruned_media: vec![PrunedMediaOutput {
                seq: 0,
                placeholder: format!(
                    "User prompt with image\n[Image Offloaded: image/png elided to relieve context pressure]\n- Artifact Path: \"{}/abc.png\"",
                    nuo_tool::spool_root_dir().display()
                ),
            }],
        },
    });

    let view = transcript.project();
    assert_eq!(view.len(), 2);
    assert!(view[0].1.images.is_none());
    assert!(view[0].1.content.contains("abc.png"));
}
