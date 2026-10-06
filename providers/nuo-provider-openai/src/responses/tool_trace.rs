//! Typed projection of the provider-neutral tool trace into Responses input.
//!
//! The durable transcript keeps provider-returned ids unchanged for audit and
//! replay. This module gives one request a wire-valid view: calls and outputs
//! are paired by occurrence, orphaned halves are removed, and locally replayed
//! calls receive request-unique ids without changing the transcript.

use nuo_model_codec::ToolCall;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProjectionError {
    MissingCallId(&'static str),
    UnknownCallId(String),
}

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCallId(kind) => {
                write!(
                    formatter,
                    "Responses tool trace contains {kind} without a call_id."
                )
            }
            Self::UnknownCallId(call_id) => write!(
                formatter,
                "Responses tool output references unknown call_id `{call_id}`."
            ),
        }
    }
}

impl std::error::Error for ProjectionError {}

/// One typed Responses input item before final JSON serialization.
#[derive(Debug)]
pub(super) struct InputItem {
    kind: InputItemKind,
}

#[derive(Debug)]
enum InputItemKind {
    Plain(Value),
    FunctionCall {
        original_call_id: String,
        wire_call_id: String,
        payload: FunctionCallPayload,
    },
    FunctionCallOutput {
        original_call_id: String,
        wire_call_id: String,
        payload: FunctionCallOutputPayload,
    },
}

#[derive(Debug)]
enum FunctionCallPayload {
    Neutral { name: String, arguments: String },
    ProviderOwned(Value),
}

#[derive(Debug)]
enum FunctionCallOutputPayload {
    Neutral(String),
    ProviderOwned(Value),
}

impl InputItem {
    pub(super) fn plain(value: Value) -> Self {
        Self {
            kind: InputItemKind::Plain(value),
        }
    }

    pub(super) fn function_call(call: &ToolCall) -> Self {
        Self {
            kind: InputItemKind::FunctionCall {
                original_call_id: call.id.clone(),
                wire_call_id: call.id.clone(),
                payload: FunctionCallPayload::Neutral {
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
            },
        }
    }

    pub(super) fn function_call_output(call_id: String, output: String) -> Self {
        Self {
            kind: InputItemKind::FunctionCallOutput {
                original_call_id: call_id.clone(),
                wire_call_id: call_id,
                payload: FunctionCallOutputPayload::Neutral(output),
            },
        }
    }

    /// Preserve an opaque provider item while lifting tool-trace items into the
    /// typed projection. Unknown items remain structurally unchanged.
    pub(super) fn provider_owned(value: Value) -> Self {
        let item_type = value.get("type").and_then(Value::as_str);
        let call_id = value
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match item_type {
            Some("function_call") => Self {
                kind: InputItemKind::FunctionCall {
                    original_call_id: call_id.clone(),
                    wire_call_id: call_id,
                    payload: FunctionCallPayload::ProviderOwned(value),
                },
            },
            Some("function_call_output") => Self {
                kind: InputItemKind::FunctionCallOutput {
                    original_call_id: call_id.clone(),
                    wire_call_id: call_id,
                    payload: FunctionCallOutputPayload::ProviderOwned(value),
                },
            },
            _ => Self::plain(value),
        }
    }

    fn call_id(&self) -> Option<&str> {
        match &self.kind {
            InputItemKind::FunctionCall {
                original_call_id, ..
            }
            | InputItemKind::FunctionCallOutput {
                original_call_id, ..
            } => Some(original_call_id),
            InputItemKind::Plain(_) => None,
        }
    }

    fn set_wire_call_id(&mut self, call_id: String) {
        match &mut self.kind {
            InputItemKind::FunctionCall { wire_call_id, .. }
            | InputItemKind::FunctionCallOutput { wire_call_id, .. } => *wire_call_id = call_id,
            InputItemKind::Plain(_) => {}
        }
    }

    pub(super) fn into_wire(self) -> Value {
        match self.kind {
            InputItemKind::Plain(value) => value,
            InputItemKind::FunctionCall {
                wire_call_id,
                payload,
                ..
            } => match payload {
                FunctionCallPayload::Neutral { name, arguments } => json!({
                    "type": "function_call",
                    "call_id": wire_call_id,
                    "name": name,
                    "arguments": arguments,
                }),
                FunctionCallPayload::ProviderOwned(mut value) => {
                    value["call_id"] = json!(wire_call_id);
                    value
                }
            },
            InputItemKind::FunctionCallOutput {
                wire_call_id,
                payload,
                ..
            } => match payload {
                FunctionCallOutputPayload::Neutral(output) => json!({
                    "type": "function_call_output",
                    "call_id": wire_call_id,
                    "output": output,
                }),
                FunctionCallOutputPayload::ProviderOwned(mut value) => {
                    value["call_id"] = json!(wire_call_id);
                    value
                }
            },
        }
    }
}

/// Produce a complete, request-valid tool trace.
///
/// Local outputs pair with the nearest preceding local call of the same
/// provider id. Outputs whose calls live in `previous_response_id` are retained
/// unchanged. Every other unpaired call or output is omitted. Surviving local
/// calls are then assigned deterministic ids unique across both the local input
/// and the remote continuation context.
pub(super) fn project(
    mut items: Vec<InputItem>,
    remote_call_ids: impl IntoIterator<Item = String>,
) -> Result<Vec<InputItem>, ProjectionError> {
    let remote_call_ids: HashSet<String> = remote_call_ids
        .into_iter()
        .filter(|call_id| !call_id.is_empty())
        .collect();
    let mut unmatched_remote_calls = remote_call_ids.clone();
    let mut pending_local_calls: HashMap<String, Vec<usize>> = HashMap::new();
    let mut local_output_to_call: HashMap<usize, usize> = HashMap::new();
    let mut remote_outputs = HashSet::new();
    let mut matched_local_calls = HashSet::new();

    for (index, item) in items.iter().enumerate() {
        match &item.kind {
            InputItemKind::FunctionCall {
                original_call_id, ..
            } => {
                if original_call_id.is_empty() {
                    return Err(ProjectionError::MissingCallId("a function call"));
                }
                pending_local_calls
                    .entry(original_call_id.clone())
                    .or_default()
                    .push(index);
            }
            InputItemKind::FunctionCallOutput {
                original_call_id, ..
            } => {
                if original_call_id.is_empty() {
                    return Err(ProjectionError::MissingCallId("a function output"));
                }
                if let Some(call_index) = pending_local_calls
                    .get_mut(original_call_id)
                    .and_then(|indices| indices.pop())
                {
                    matched_local_calls.insert(call_index);
                    local_output_to_call.insert(index, call_index);
                } else if unmatched_remote_calls.remove(original_call_id) {
                    remote_outputs.insert(index);
                } else {
                    return Err(ProjectionError::UnknownCallId(original_call_id.clone()));
                }
            }
            _ => {}
        }
    }

    let mut reserved_ids = remote_call_ids.clone();
    reserved_ids.extend(
        matched_local_calls
            .iter()
            .filter_map(|index| items[*index].call_id().map(str::to_string)),
    );
    let mut used_ids = remote_call_ids;
    let mut effective_ids: HashMap<usize, String> = HashMap::new();
    let mut synthetic_ordinal = 1_u64;
    for (index, item) in items.iter_mut().enumerate() {
        if !matched_local_calls.contains(&index) {
            continue;
        }
        let Some(original) = item.call_id().map(str::to_string) else {
            continue;
        };
        let effective = if used_ids.insert(original.clone()) {
            original
        } else {
            loop {
                let candidate = format!("call_nuo_{synthetic_ordinal}");
                synthetic_ordinal += 1;
                if !reserved_ids.contains(&candidate) && used_ids.insert(candidate.clone()) {
                    break candidate;
                }
            }
        };
        item.set_wire_call_id(effective.clone());
        effective_ids.insert(index, effective);
    }
    for (output_index, call_index) in &local_output_to_call {
        if let Some(effective) = effective_ids.get(call_index) {
            items[*output_index].set_wire_call_id(effective.clone());
        }
    }

    Ok(items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| match &item.kind {
            InputItemKind::FunctionCall { .. } if !matched_local_calls.contains(&index) => None,
            InputItemKind::FunctionCallOutput { .. }
                if !local_output_to_call.contains_key(&index)
                    && !remote_outputs.contains(&index) =>
            {
                None
            }
            _ => Some(item),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str) -> InputItem {
        InputItem::function_call(&ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
        })
    }

    fn output(id: &str, value: &str) -> InputItem {
        InputItem::function_call_output(id.to_string(), value.to_string())
    }

    fn wire(items: Vec<InputItem>, remote: &[&str]) -> Vec<Value> {
        project(items, remote.iter().map(|id| (*id).to_string()))
            .unwrap()
            .into_iter()
            .map(InputItem::into_wire)
            .collect()
    }

    #[test]
    fn duplicate_local_ids_are_remapped_with_their_outputs() {
        let items = wire(
            vec![
                call("call_244115", "first"),
                output("call_244115", "one"),
                call("call_244115", "second"),
                output("call_244115", "two"),
            ],
            &[],
        );
        assert_eq!(items[0]["call_id"], "call_244115");
        assert_eq!(items[1]["call_id"], "call_244115");
        assert_eq!(items[2]["call_id"], "call_nuo_1");
        assert_eq!(items[3]["call_id"], "call_nuo_1");
    }

    #[test]
    fn synthetic_ids_skip_provider_owned_reservations() {
        let items = wire(
            vec![
                call("call_nuo_1", "reserved"),
                output("call_nuo_1", "reserved"),
                call("duplicate", "first"),
                output("duplicate", "first"),
                call("duplicate", "second"),
                output("duplicate", "second"),
            ],
            &[],
        );
        assert_eq!(items[4]["call_id"], "call_nuo_2");
        assert_eq!(items[5]["call_id"], "call_nuo_2");
    }

    #[test]
    fn nearest_answered_duplicate_survives() {
        let items = wire(
            vec![
                call("same", "interrupted"),
                call("same", "answered"),
                output("same", "ok"),
            ],
            &[],
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["name"], "answered");
        assert_eq!(items[1]["output"], "ok");
    }

    #[test]
    fn orphan_outputs_fail_closed() {
        let error = project(
            vec![
                InputItem::plain(json!({"type": "message"})),
                call("unanswered", "tool"),
                output("orphan", "ignored"),
            ],
            Vec::<String>::new(),
        )
        .unwrap_err();
        assert_eq!(error, ProjectionError::UnknownCallId("orphan".to_string()));
    }

    #[test]
    fn remote_continuation_outputs_keep_the_remote_id() {
        let items = wire(vec![output("remote_call", "done")], &["remote_call"]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["call_id"], "remote_call");
    }

    #[test]
    fn local_ids_cannot_collide_with_the_remote_context() {
        let items = wire(
            vec![
                output("remote_call", "remote result"),
                call("remote_call", "local"),
                output("remote_call", "local result"),
            ],
            &["remote_call"],
        );
        assert_eq!(items[0]["call_id"], "remote_call");
        assert_eq!(items[1]["call_id"], "call_nuo_1");
        assert_eq!(items[2]["call_id"], "call_nuo_1");
    }

    #[test]
    fn provider_owned_items_preserve_every_field_except_a_remapped_call_id() {
        let items = wire(
            vec![
                InputItem::provider_owned(json!({
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "duplicate",
                    "name": "first",
                    "arguments": "{}",
                    "status": "completed"
                })),
                output("duplicate", "first"),
                InputItem::provider_owned(json!({
                    "id": "fc_2",
                    "type": "function_call",
                    "call_id": "duplicate",
                    "name": "second",
                    "arguments": "{}",
                    "status": "completed"
                })),
                output("duplicate", "second"),
            ],
            &[],
        );
        assert_eq!(items[2]["id"], "fc_2");
        assert_eq!(items[2]["status"], "completed");
        assert_eq!(items[2]["call_id"], "call_nuo_1");
        assert_eq!(items[3]["call_id"], "call_nuo_1");
    }
}
