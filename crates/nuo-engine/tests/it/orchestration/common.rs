//! Shared snapshots for orchestration integration assertions.

use super::*;

/// Serialize the provider-visible history while excluding local diagnostic
/// metadata that is regenerated for every request projection.
///
/// These tests verify checkpoint semantics. A projected system message's
/// wall-clock timestamp is intentionally not part of that protocol contract
/// and may advance while an instrumented/slow platform executes the round.
pub(crate) fn provider_history_snapshot(messages: &[Message]) -> String {
    let mut messages =
        serde_json::to_value(messages).expect("messages should serialize to a JSON value");
    for message in messages
        .as_array_mut()
        .expect("serialized messages should be an array")
    {
        message
            .as_object_mut()
            .expect("serialized message should be an object")
            .remove("timestamp");
    }
    serde_json::to_string(&messages).expect("messages should serialize")
}

#[test]
pub(super) fn provider_history_snapshot_ignores_only_diagnostic_timestamps() {
    let original = Message::new(Role::System, "stable prompt");
    let mut later_projection = original.clone();
    later_projection.timestamp = original.timestamp.map(|timestamp| timestamp + 60);
    assert_eq!(
        provider_history_snapshot(std::slice::from_ref(&original)),
        provider_history_snapshot(std::slice::from_ref(&later_projection))
    );

    later_projection.content = "changed prompt".to_string();
    assert_ne!(
        provider_history_snapshot(std::slice::from_ref(&original)),
        provider_history_snapshot(std::slice::from_ref(&later_projection)),
        "semantic provider history must remain part of the checkpoint assertion"
    );
}
