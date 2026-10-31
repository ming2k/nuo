//! Transcript stream mutations, patch application, and selection extraction.

use nuo_view::model::document::TranscriptMessage;
use nuo_view::model::selection::{CellDragInfo, SelectionState, get_selected_text};

/// Resolve a mutable reference to a message by semantic index.
pub(crate) fn resolve_focused_mut<'a>(
    messages: &'a mut [TranscriptMessage],
    focus_stack: &[crate::shell::app::ZoomFrame],
    mi: usize,
) -> Option<&'a mut TranscriptMessage> {
    let Some(current) = focus_stack.last() else {
        return messages.get_mut(mi);
    };
    let task_idx = messages.iter().position(|message| {
        message.is_subagent_task() && message.tool_step_call_id() == Some(current.call_id.as_str())
    })?;
    messages[task_idx].subagent_children_mut()?.get_mut(mi)
}

/// Iterate mutable messages in the currently focused view for tests.
#[cfg(test)]
pub(crate) fn focused_messages_mut<'a>(
    messages: &'a mut [TranscriptMessage],
    focus_stack: &[crate::shell::app::ZoomFrame],
) -> Box<dyn Iterator<Item = &'a mut TranscriptMessage> + 'a> {
    match focus_stack.last() {
        None => Box::new(messages.iter_mut()),
        Some(current) => {
            let task_idx = messages.iter().position(|message| {
                message.is_subagent_task()
                    && message.tool_step_call_id() == Some(current.call_id.as_str())
            });
            match task_idx {
                Some(idx) => match messages[idx].subagent_children_mut() {
                    Some(children) => Box::new(children.iter_mut()),
                    None => Box::new(std::iter::empty()),
                },
                None => Box::new(std::iter::empty()),
            }
        }
    }
}

/// Extract selected text from either transcript messages or the live input box.
pub(crate) fn extract_selection_text(
    sel: &SelectionState,
    messages: &[nuo_view::model::document::TranscriptMessage],
    input: &str,
    layout_map: &nuo_view::model::layout::LayoutMap,
    cell_info: Option<&CellDragInfo>,
) -> Option<String> {
    if let SelectionState::InputRange {
        anchor_byte,
        head_byte,
    } = sel
    {
        let s = (*anchor_byte).min(*head_byte);
        let e = (*anchor_byte).max(*head_byte);
        if s < e && e <= input.len() {
            return Some(input[s..e].to_string());
        }
        return None;
    }
    if let Some((start, end)) = sel.active_normalized_range() {
        if start.message_idx == crate::shell::render::INPUT_MSG_IDX {
            let s = start.byte_offset;
            let e = end.byte_offset;
            if s <= e && e <= input.len() {
                let start_idx = input
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|&i| i <= s)
                    .last()
                    .unwrap_or(0);
                let end_idx = input
                    .char_indices()
                    .map(|(i, _)| i)
                    .find(|&i| i >= e)
                    .unwrap_or(input.len());
                return Some(input[start_idx..end_idx].to_string());
            }
            return None;
        }
        if start.message_idx == nuo_view::model::layout::MODAL_DOC_MSG_IDX {
            return layout_map.extract_text_for_range(sel);
        }
    } else if let SelectionState::Block { message_idx, .. } = sel
        && *message_idx == crate::shell::render::INPUT_MSG_IDX
    {
        return Some(input.to_string());
    } else if let SelectionState::Block { message_idx, .. } = sel
        && *message_idx == nuo_view::model::layout::MODAL_DOC_MSG_IDX
    {
        return layout_map.extract_text_for_range(sel);
    }

    let grid = |mi, bi| layout_map.table_grid(mi, bi);
    get_selected_text(sel, messages, &grid, cell_info)
}

/// Format the current loop status into human-readable text.
pub(crate) fn display_status(
    loop_status: nuo_protocol::LoopStatus,
    phase: Option<&nuo_view::phase::Phase>,
) -> String {
    match (loop_status, phase) {
        (nuo_protocol::LoopStatus::Idle, None) => "idle".to_string(),
        (nuo_protocol::LoopStatus::Running, None) => "preparing".to_string(),
        (nuo_protocol::LoopStatus::Idle, Some(phase))
        | (nuo_protocol::LoopStatus::Running, Some(phase)) => phase.label().into_owned(),
    }
}
