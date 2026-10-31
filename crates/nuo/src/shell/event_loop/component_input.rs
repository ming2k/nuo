//! Scene-resolved component input handlers (M2).
//!
//! The event loop asks the committed scene for a keyboard path, then offers
//! the event to the components on that path. These handlers contain the few
//! component-local mutations that cannot be represented by the shared input
//! action vocabulary.
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::shell::composer_grid::Vertical;
use crate::shell::input::readline::{
    cursor_line_end, cursor_line_start, next_grapheme_boundary, next_word_end,
    normalized_cursor_byte, prev_word_start, previous_grapheme_boundary,
};
use crate::shell::input::{self};
use crate::shell::ui;
use crate::shell::{App, ProviderDeleteChoice, SelectionEdge};
use nuo_ui_primitives::components::dropdown::DropdownEventOutcome;
use nuo_view::model::selection::{SelectionState, floor_grapheme_boundary, inclusive_grapheme_end};

/// Offer an event to the handlers on the scene-resolved keyboard path.
///
/// # Pointer input never travels this path
///
/// This cascade is a *keyboard* route (`UiRuntime::keyboard_path_for`). A
/// pointer event's authority is the presented scene's geometry, not the
/// keyboard foreground — so a mounted `InputPolicy::Modal` component must not
/// be offered a mouse event at all. Routing clicks through it made every click
/// in the frame inert while a settings dropdown or the provider-delete confirm
/// was mounted (`handle_config_dropdown` / `handle_delete_overlay` answer
/// `None`-actions for any non-key event), which silently ate the click meant to
/// dismiss the popup *and* any click aimed at the tab bar underneath it — the
/// "click twice and nothing happens" defect.
pub(crate) fn route(
    app: &mut App,
    event: &Event,
    keyboard_path: &[ui::UiKey],
) -> Option<input::InputAction> {
    if matches!(event, Event::Mouse(_)) {
        return None;
    }
    for key in keyboard_path {
        match key {
            ui::UiKey::ConfigDropdown => return handle_config_dropdown(app, event),
            ui::UiKey::ProviderDelete => return handle_delete_overlay(app, event),
            ui::UiKey::Composer => {
                if let Some(action) = handle_input_selection(app, event) {
                    return Some(action);
                }
            }
            _ => {}
        }
    }
    None
}

/// Probe a raw input event against the composer's text selection and navigation.
fn handle_input_selection(app: &mut App, event: &Event) -> Option<input::InputAction> {
    let Event::Key(key) = event else {
        return None;
    };
    if !matches!(key.kind, KeyEventKind::Press) {
        return None;
    }
    if app.caret_owner() != crate::shell::CaretOwner::Composer {
        return None;
    }

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let word_chord = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);

    if shift {
        let is_nav = matches!(
            key.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
        );
        if is_nav {
            let anchor_byte = match app.selection {
                SelectionState::InputRange { anchor_byte, .. } => anchor_byte,
                SelectionState::Range { anchor, .. }
                    if anchor.message_idx == crate::shell::render::INPUT_MSG_IDX =>
                {
                    anchor.byte_offset
                }
                _ => normalized_cursor_byte(&app.input, app.cursor_position),
            };

            let new_pos = match key.code {
                KeyCode::Left => {
                    if word_chord {
                        prev_word_start(&app.input, app.cursor_position)
                    } else {
                        previous_grapheme_boundary(&app.input, app.cursor_position)
                    }
                }
                KeyCode::Right => {
                    if word_chord {
                        next_word_end(&app.input, app.cursor_position)
                    } else {
                        next_grapheme_boundary(&app.input, app.cursor_position)
                    }
                }
                // Shift+↑/↓ extend the selection one *visual row*, the same
                // motion plain ↑/↓ performs (shared grid, shared sticky
                // column), so extending and then releasing the shift never
                // lands the caret in a different column than the plain arrows
                // would have.
                KeyCode::Up => {
                    let grid = app.composer_grid();
                    grid.shift_row(app.cursor_position, Vertical::Up, &mut app.caret_motion)
                        .unwrap_or(0)
                }
                KeyCode::Down => {
                    let grid = app.composer_grid();
                    grid.shift_row(app.cursor_position, Vertical::Down, &mut app.caret_motion)
                        .unwrap_or(app.input.len())
                }
                KeyCode::Home => {
                    let mut target = app.cursor_position;
                    cursor_line_start(&app.input, &mut target);
                    target
                }
                KeyCode::End => {
                    let mut target = app.cursor_position;
                    cursor_line_end(&app.input, &mut target);
                    target
                }
                _ => app.cursor_position,
            };

            app.set_cursor(new_pos);
            let head_byte = normalized_cursor_byte(&app.input, new_pos);
            if anchor_byte == head_byte {
                app.selection = SelectionState::None;
            } else {
                app.selection = SelectionState::InputRange {
                    anchor_byte,
                    head_byte,
                };
            }
            return Some(input::InputAction::None);
        }
    }

    if !app.has_input_selection() {
        return None;
    }

    let step_from_head = |app: &mut App, forward: bool, word: bool| {
        app.adopt_caret_from_input_selection(SelectionEdge::Head);
        let target = if word {
            if forward {
                next_word_end(&app.input, app.cursor_position)
            } else {
                prev_word_start(&app.input, app.cursor_position)
            }
        } else if forward {
            next_grapheme_boundary(&app.input, app.cursor_position)
        } else {
            previous_grapheme_boundary(&app.input, app.cursor_position)
        };
        app.set_cursor(target);
    };

    match (key.code, word_chord) {
        (KeyCode::Left, false) => {
            step_from_head(app, false, false);
            Some(input::InputAction::None)
        }
        (KeyCode::Right, false) => {
            step_from_head(app, true, false);
            Some(input::InputAction::None)
        }
        (KeyCode::Left, true) => {
            step_from_head(app, false, true);
            Some(input::InputAction::None)
        }
        (KeyCode::Right, true) => {
            step_from_head(app, true, true);
            Some(input::InputAction::None)
        }
        (KeyCode::Up | KeyCode::Down, _) => {
            app.adopt_caret_from_input_selection(SelectionEdge::Head);
            Some(input::InputAction::None)
        }
        (KeyCode::Home, _) => {
            if let SelectionState::InputRange {
                anchor_byte,
                head_byte,
            } = app.selection
            {
                let lo = anchor_byte.min(head_byte).min(app.input.len());
                app.selection = SelectionState::None;
                app.drag.cancel();
                app.set_cursor(lo);
            } else if let Some((start, _)) = app.selection.active_normalized_range() {
                let byte =
                    floor_grapheme_boundary(&app.input, start.byte_offset).min(app.input.len());
                app.selection = SelectionState::None;
                app.drag.cancel();
                app.set_cursor(byte);
            } else {
                app.adopt_caret_from_input_selection(SelectionEdge::Tail);
            }
            Some(input::InputAction::None)
        }
        (KeyCode::End, _) => {
            if let SelectionState::InputRange {
                anchor_byte,
                head_byte,
            } = app.selection
            {
                let hi = anchor_byte.max(head_byte).min(app.input.len());
                app.selection = SelectionState::None;
                app.drag.cancel();
                app.set_cursor(hi);
            } else if let Some((_, end)) = app.selection.active_normalized_range() {
                let byte = inclusive_grapheme_end(&app.input, end.byte_offset).min(app.input.len());
                app.selection = SelectionState::None;
                app.drag.cancel();
                app.set_cursor(byte);
            } else {
                app.adopt_caret_from_input_selection(SelectionEdge::Head);
            }
            Some(input::InputAction::None)
        }
        (KeyCode::Esc, _) => {
            app.selection = SelectionState::None;
            app.drag.cancel();
            Some(input::InputAction::None)
        }
        (KeyCode::Backspace | KeyCode::Delete, _) => {
            app.delete_input_selection();
            Some(input::InputAction::Backspace)
        }
        (KeyCode::Char('w') | KeyCode::Char('u') | KeyCode::Char('k'), _)
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.delete_input_selection();
            Some(input::InputAction::Backspace)
        }
        (KeyCode::Char('d'), _) if key.modifiers.contains(KeyModifiers::ALT) => {
            app.delete_input_selection();
            Some(input::InputAction::Backspace)
        }
        _ => None,
    }
}

fn handle_config_dropdown(app: &mut App, event: &Event) -> Option<input::InputAction> {
    app.config_dropdown.as_ref()?;

    let Event::Key(k) = event else {
        return Some(input::InputAction::None);
    };

    if !matches!(k.kind, KeyEventKind::Press) {
        return Some(input::InputAction::None);
    }

    let (mut dropdown, anchor) = app.config_dropdown.take()?;
    let outcome = dropdown.handle_key(crate::shell::input::keystroke::stroke_from_event(*k));
    match outcome {
        DropdownEventOutcome::Ignored => {
            app.config_dropdown = Some((dropdown, anchor));
            None
        }
        DropdownEventOutcome::Handled => {
            app.config_dropdown = Some((dropdown, anchor));
            Some(input::InputAction::None)
        }
        DropdownEventOutcome::Cancelled => {
            app.config_dropdown = None;
            Some(input::InputAction::None)
        }
        DropdownEventOutcome::Confirmed(payload) => {
            let ctx = dropdown.context.as_deref().unwrap_or("");
            let revision = app.websearch_config.as_ref().map(|config| config.revision);
            match ctx {
                "websearch_provider" => {
                    if let (Some(revision), Ok(provider)) = (revision, payload.parse()) {
                        app.send_intent(nuo_protocol::AgentRequest::UpdateWebSearchConfig(
                            Box::new(nuo_protocol::WebSearchConfigUpdate {
                                expected_revision: revision,
                                provider: Some(provider),
                                ..Default::default()
                            }),
                        ));
                    }
                }
                "websearch_reader" => {
                    if let (Some(revision), Ok(reader)) = (revision, payload.parse()) {
                        app.send_intent(nuo_protocol::AgentRequest::UpdateWebSearchConfig(
                            Box::new(nuo_protocol::WebSearchConfigUpdate {
                                expected_revision: revision,
                                reader: Some(reader),
                                ..Default::default()
                            }),
                        ));
                    }
                }
                _ => {}
            }
            app.config_dropdown = None;
            Some(input::InputAction::None)
        }
    }
}

fn handle_delete_overlay(app: &mut App, event: &Event) -> Option<input::InputAction> {
    app.pending_provider_delete.as_ref()?;

    let Event::Key(k) = event else {
        return Some(input::InputAction::None);
    };

    if !matches!(k.kind, KeyEventKind::Press) {
        return Some(input::InputAction::None);
    }

    match (k.modifiers, k.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            Some(input::InputAction::DeleteProviderCancel)
        }
        (KeyModifiers::NONE, KeyCode::Esc) => Some(input::InputAction::DeleteProviderCancel),
        (KeyModifiers::NONE, KeyCode::Enter) => {
            if app.provider_delete_focus == ProviderDeleteChoice::Delete {
                Some(input::InputAction::DeleteProviderConfirm)
            } else {
                Some(input::InputAction::DeleteProviderCancel)
            }
        }
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Left)
        | (KeyModifiers::CONTROL, KeyCode::Char('b'))
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('h')) => {
            app.provider_delete_focus = ProviderDeleteChoice::Cancel;
            Some(input::InputAction::None)
        }
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Right)
        | (KeyModifiers::CONTROL, KeyCode::Char('f'))
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('l')) => {
            app.provider_delete_focus = ProviderDeleteChoice::Delete;
            Some(input::InputAction::None)
        }
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Tab)
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Down)
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Up) => {
            app.provider_delete_focus = match app.provider_delete_focus {
                ProviderDeleteChoice::Cancel => ProviderDeleteChoice::Delete,
                ProviderDeleteChoice::Delete => ProviderDeleteChoice::Cancel,
            };
            Some(input::InputAction::None)
        }
        _ => Some(input::InputAction::None),
    }
}
