//! Composer and suggestion/history action handling.
use crate::shell::App;
use crate::shell::input::InputAction;

pub(super) fn handle_composer_action(app: &mut App, action: InputAction) {
    match action {
        InputAction::InsertChar(c) => {
            // Already handled by route_event mutating app.composer.input
            let _ = c;
            app.composer.suggestion_index = None;
            // The user is editing again, so live completions are
            // once again useful — clear the Enter-commit dismissal.
            app.composer.completion_dismissed = false;
            // Only the composer plane accepts printable input (route_event
            // gates insertion on `edits_input_field`), so reaching here already
            // implies composer ownership; state it so the invariant holds by
            // construction rather than by trusting the router's gate.
            app.transcript.focus.enter_composer();
            // Reconcile attachments: if the user typed inside a chip
            // (breaking its syntax) the backing staged entry must be
            // dropped, and surviving chips relabeled.
            app.reconcile_attachments();
        }
        InputAction::Backspace => {
            app.composer.suggestion_index = None;
            app.composer.completion_dismissed = false;
            // Same as InsertChar: an edit is a composer-plane act, so the
            // plane is stated explicitly rather than inferred.
            app.transcript.focus.enter_composer();
            // Reconcile attachments: a chip-aware backspace has
            // already spliced the chip out of `app.composer.input`; this
            // drops the orphaned entry from `pending_images` /
            // `pending_text_pastes` and relabels survivors.
            app.reconcile_attachments();
        }
        InputAction::DeleteForward => {
            // Forward delete runs the same post-edit passes as Backspace: the
            // text mutation already happened in `route_event`; this arm
            // only keeps the completion latch, focus ownership, and staged
            // attachments consistent with the new buffer (a chip-aware
            // forward delete may have orphaned a staged entry).
            app.composer.suggestion_index = None;
            app.composer.completion_dismissed = false;
            // Editing the input box is a composer-plane act, mirroring
            // Backspace.
            app.transcript.focus.enter_composer();
            app.reconcile_attachments();
        }
        InputAction::SuggestNext => {
            let count = app.completions().len();
            if count > 0 {
                let next = match app.composer.suggestion_index {
                    Some(i) => (i + 1) % count,
                    None => 0,
                };
                app.composer.suggestion_index = Some(next);
            }
        }
        InputAction::SuggestPrev => {
            let count = app.completions().len();
            if count > 0 {
                let prev = match app.composer.suggestion_index {
                    Some(i) => {
                        if i == 0 {
                            count - 1
                        } else {
                            i - 1
                        }
                    }
                    None => count - 1,
                };
                app.composer.suggestion_index = Some(prev);
            }
        }
        InputAction::AcceptSuggestion(idx_str) => {
            if let Ok(idx) = idx_str.parse::<usize>() {
                app.accept_completion(idx);
            }
            // Legacy accept-without-closing arm (no longer bound to a key at
            // the top level — Tab now commits like Enter). Kept for callers
            // that want a live splice; the popup stays open only for
            // directory descents, which accept_completion decides by kind.
        }
        InputAction::CommitSuggestion(idx_str) => {
            if let Ok(idx) = idx_str.parse::<usize>() {
                app.accept_completion(idx);
            }
        }
        InputAction::ReopenCompletion => {
            // The other half of the Esc/Tab toggle: bring a dismissed
            // completion menu back without accepting anything. The next
            // anchor pass (post-dispatch, same iteration) seeds the
            // highlight onto the first candidate, so the reopened menu
            // lands already selected with its details flyout showing.
            app.composer.completion_dismissed = false;
        }
        InputAction::CloseCompletion => {
            // Esc dismisses the popup without accepting anything.
            // Same latch as Enter-commit so the popup stays hidden
            // until the next edit clears `completion_dismissed` — or
            // until Tab re-opens it (ReopenCompletion).
            app.composer.suggestion_index = None;
            app.composer.completion_dismissed = true;
        }
        InputAction::HistoryPrev => {
            // Inline ↑ walks the **current session's** history only
            // (newest-first), not the whole cross-session log — Ctrl+R
            // is the global search surface. We recompute the session
            // slice each press so newly-recorded entries appear
            // without a restart; `history_index` is a position into
            // that slice. `App::history_prev` advances toward older
            // entries and stashes the in-progress draft on the first
            // press (so ↓ can restore it).
            let session_rows = app.current_session_history();
            app.history_prev(&session_rows);
        }
        InputAction::HistoryNext => {
            // Inline ↓ walks the current session's history forward
            // (toward the newest), mirroring HistoryPrev. Walking past
            // the newest entry restores the stashed draft.
            let session_rows = app.current_session_history();
            app.history_next(&session_rows);
        }
        _ => {}
    }
}
