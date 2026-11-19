//! Dialog keyboard input: navigation, sub-layers, and actions.

use super::actions::{dialog_body_bounds, handle_dialog_click, handle_dialog_scroll, scroll_delta};
use super::entities::*;
use super::navigation::{rotate, rotate_selectable};
use super::*;

/// Handle a local navigation/scroll action for one dialog entity
/// (`DialogView::handle_input`). `ModalUp`/`ModalDown` are the generic ↑/↓
/// arrows; `SessionSelect` is the list-navigation family; `Scroll*` and `Wheel`
/// actions drive autonomous viewport and sub-view scrolling (`[INV-SURFACE-06]`,
/// `[INV-SURFACE-08]`). Returns `Unhandled` for actions the entity does not
/// own, so the event loop keeps dispatching semantic commands.
#[allow(clippy::expect_used)]
pub(super) fn input_dialog(
    view: &mut dyn DialogView,
    action: &InputAction,
    app: &mut crate::shell::App,
    viewed_session_id: &str,
) -> DialogOutcome {
    use InputAction as A;

    // Viewport scrolling and spatial wheel delegation (ADR-0047):
    if let Some(delta) = scroll_delta(action, app) {
        return handle_dialog_scroll(view, delta);
    }
    if let InputAction::DialogKeysScroll { delta } = action
        && view.keys_open()
    {
        if *delta < 0 {
            *view.keys_scroll_mut() = view.keys_scroll_mut().saturating_sub((-delta) as usize);
        } else {
            *view.keys_scroll_mut() = view.keys_scroll_mut().saturating_add(*delta as usize);
        }
        return DialogOutcome::Consumed;
    }

    // Spatial click-to-select delegation (ADR-0047 [INV-SURFACE-07]):
    if let InputAction::SelectionStart { x, y } = action {
        if view.keys_open() {
            return DialogOutcome::Consumed;
        }
        let modal_bounds = app.transcript.ui.bounds(crate::shell::ui::UiKey::Overlay(
            crate::shell::surfaces::OverlaySurface::Dialog(view.kind()),
        ));
        if let Some(modal_rect) = modal_bounds {
            let (body_top, body_bottom) = dialog_body_bounds(view.kind(), modal_rect);
            if *y >= body_top
                && *y < body_bottom
                && *x >= modal_rect.x
                && *x < modal_rect.x + modal_rect.width
            {
                let visual_row = (*y - body_top) as usize;
                return handle_dialog_click(view, visual_row, app, viewed_session_id);
            }
            if view.kind() == DialogKind::Help
                && *y >= modal_rect.y + modal_rect.height.saturating_sub(2)
                && *y < modal_rect.y + modal_rect.height
                && *x >= modal_rect.x
                && *x < modal_rect.x + modal_rect.width
            {
                return DialogOutcome::Dismiss;
            }
        }
        return DialogOutcome::Consumed;
    }

    // `(forward, is_modal_arrow)`.
    let (forward, arrow) = match action {
        A::ModalUp => (false, true),
        A::ModalDown => (true, true),
        A::SessionSelect { forward } => (*forward, false),
        _ => return DialogOutcome::Unhandled,
    };

    match view.kind() {
        DialogKind::Tools => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ToolsDialog>()
                .expect("kind");
            if e.info_detail {
                if forward {
                    e.info_scroll = e.info_scroll.saturating_add(1);
                } else {
                    e.info_scroll = e.info_scroll.saturating_sub(1);
                }
                return DialogOutcome::Consumed;
            }
            if arrow {
                return DialogOutcome::Unhandled;
            }
            let count = app.session_tools_len();
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            e.index = rotate(e.index, count, forward);
            e.follow = true;
            DialogOutcome::Consumed
        }
        DialogKind::Mcp | DialogKind::Skills => {
            if arrow {
                return DialogOutcome::Unhandled;
            }
            let count = match view.kind() {
                DialogKind::Mcp => app
                    .session
                    .session_context
                    .as_ref()
                    .map_or(0, |s| s.mcp.len()),
                _ => app
                    .session
                    .session_context
                    .as_ref()
                    .map_or(0, |s| s.skills.len()),
            };
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            match view.kind() {
                DialogKind::Mcp => {
                    let e = view.as_any_mut().downcast_mut::<McpDialog>().expect("kind");
                    e.index = rotate(e.index, count, forward);
                    e.follow = true;
                }
                _ => {
                    let e = view
                        .as_any_mut()
                        .downcast_mut::<SkillsDialog>()
                        .expect("kind");
                    e.index = rotate(e.index, count, forward);
                    e.follow = true;
                }
            }
            DialogOutcome::Consumed
        }
        DialogKind::Permissions => {
            let count = app
                .session
                .session_context
                .as_ref()
                .map_or(0, |s| s.permissions.len());
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            let e = view
                .as_any_mut()
                .downcast_mut::<PermissionsDialog>()
                .expect("kind");
            e.index = rotate(e.index, count, forward);
            DialogOutcome::Consumed
        }
        DialogKind::Connections => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ConnectionsDialog>()
                .expect("kind");
            if arrow && e.info_detail {
                if forward {
                    e.info_scroll = e.info_scroll.saturating_add(1);
                } else {
                    e.info_scroll = e.info_scroll.saturating_sub(1);
                }
                return DialogOutcome::Consumed;
            }
            let count = app.picker_row_count();
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            e.index = rotate(e.index, count, forward);
            e.follow = true;
            DialogOutcome::Consumed
        }
        DialogKind::Models => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ModelsDialog>()
                .expect("kind");
            if arrow && e.info_detail {
                if forward {
                    e.info_scroll = e.info_scroll.saturating_add(1);
                } else {
                    e.info_scroll = e.info_scroll.saturating_sub(1);
                }
                return DialogOutcome::Consumed;
            }
            let models = app.models_flat_filtered();
            let count = models.len();
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            e.index = rotate_selectable(e.index, count, forward, |idx| {
                models.get(idx).is_some_and(|m| m.usable)
            });
            e.follow = true;
            DialogOutcome::Consumed
        }
        DialogKind::HistorySearch => {
            let count = app.history_rows().len();
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            let e = view
                .as_any_mut()
                .downcast_mut::<HistorySearchDialog>()
                .expect("kind");
            let cur = e.index.min(count.saturating_sub(1));
            e.index = rotate(cur, count, forward);
            e.follow = true;
            DialogOutcome::Consumed
        }
        DialogKind::Threads => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ThreadsDialog>()
                .expect("kind");
            if arrow && e.info_detail {
                if forward {
                    e.info_scroll = e.info_scroll.saturating_add(1);
                } else {
                    e.info_scroll = e.info_scroll.saturating_sub(1);
                }
                return DialogOutcome::Consumed;
            }
            let count = crate::shell::overlays::session::project_session_rows(
                &app.session.sessions_overview,
                Some(&e.expanded),
            )
            .len();
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            e.index = rotate(e.index, count, forward);
            e.follow = true;
            DialogOutcome::Consumed
        }
        DialogKind::SessionTree => {
            let count = crate::shell::overlays::tree::flatten_tree(&app.session.session_tree).len();
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            let e = view
                .as_any_mut()
                .downcast_mut::<SessionTreeDialog>()
                .expect("kind");
            e.index = rotate(e.index, count, forward);
            e.follow = true;
            DialogOutcome::Consumed
        }
        DialogKind::ThreadStats => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ThreadStatsDialog>()
                .expect("kind");
            if forward {
                e.scroll = e.scroll.saturating_add(1);
            } else {
                e.scroll = e.scroll.saturating_sub(1);
            }
            DialogOutcome::Consumed
        }
        DialogKind::ContextPlan => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ContextPlanDialog>()
                .expect("kind");
            // The cursor walks the plan's **blocks**, not its rows: the request
            // accounting, the band headers, an expanded body, the notices and the
            // drift verdict are all part of the document but none of them is a
            // selection target, and a cursor that could land on a heading would
            // let `Enter` expand nothing.
            let count = app
                .telemetry
                .context_plan
                .as_ref()
                .map(|plan| plan.blocks.len())
                .unwrap_or(0);
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            let next = rotate(e.index, count, forward);
            if next != e.index {
                e.index = next;
                e.follow = true;
                // A body belongs to the row it was opened on. Carrying it across
                // a cursor move would expand the new row without the reader
                // asking, which reads as a jump rather than a selection.
                e.body_open = false;
                e.scroll = 0;
            }
            DialogOutcome::Consumed
        }
        DialogKind::SessionTrace => session_trace_step(view, app, viewed_session_id, forward),
        DialogKind::UsageStats => {
            let e = view
                .as_any_mut()
                .downcast_mut::<UsageStatsDialog>()
                .expect("kind");
            if forward {
                e.scroll = e.scroll.saturating_add(1);
            } else {
                e.scroll = e.scroll.saturating_sub(1);
            }
            DialogOutcome::Consumed
        }
        DialogKind::Quotas => {
            let e = view
                .as_any_mut()
                .downcast_mut::<QuotasDialog>()
                .expect("kind");
            if arrow {
                if forward {
                    e.scroll = e.scroll.saturating_add(1);
                } else {
                    e.scroll = e.scroll.saturating_sub(1);
                }
                e.follow = false;
            } else {
                let count = app
                    .telemetry
                    .provider_quotas
                    .as_ref()
                    .map(|q| q.entries.len())
                    .unwrap_or(0)
                    .max(1);
                e.index = rotate(e.index, count, forward);
                e.follow = true;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Queue => {
            let e = view
                .as_any_mut()
                .downcast_mut::<QueueDialog>()
                .expect("kind");
            if arrow {
                if forward {
                    e.scroll = e.scroll.saturating_add(1);
                } else {
                    e.scroll = e.scroll.saturating_sub(1);
                }
                e.follow = false;
            } else {
                // Address the same rows the modal lists (see
                // `App::outbox_projection`): a count that included
                // `Dispatching` rows would let the highlight move onto an item
                // the surface does not show.
                let count = app.pending_count(viewed_session_id);
                if count == 0 {
                    return DialogOutcome::Unhandled;
                }
                e.index = rotate(e.index, count, forward);
                e.follow = true;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Palette => {
            let e = view
                .as_any_mut()
                .downcast_mut::<PaletteDialog>()
                .expect("kind");
            let count = crate::shell::palette::project(app, &e.query.text)
                .entries
                .len();
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            e.selected = rotate(e.selected, count, forward);
            DialogOutcome::Consumed
        }
        DialogKind::Help => {
            let e = view
                .as_any_mut()
                .downcast_mut::<HelpDialog>()
                .expect("kind");
            if forward {
                e.scroll = e.scroll.saturating_add(1);
            } else {
                e.scroll = e.scroll.saturating_sub(1);
            }
            DialogOutcome::Consumed
        }
        DialogKind::PortShare => DialogOutcome::Consumed,
        DialogKind::Notices => {
            let e = view
                .as_any_mut()
                .downcast_mut::<NoticesDialog>()
                .expect("kind");
            let count = app.notice_count();
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            e.index = rotate(e.index, count, forward);
            e.follow = true;
            DialogOutcome::Consumed
        }
    }
}

/// One arrow step through the session-trace drill-down (L1 rounds → L2 turns →
/// L3 attempt scroll).
///
/// Extracted so the dialog's own navigation rules stay readable: the trace is
/// the one inspector whose cursor means three different things by level, and
/// inlining it beside every other dialog's single `scroll += 1` hid that.
///
/// `expect` is the same contract as the dispatch arm that calls it: `kind`
/// routing and the entity type are one fact, so the downcast cannot fail.
#[allow(clippy::expect_used)]
fn session_trace_step(
    view: &mut dyn DialogView,
    app: &mut crate::shell::App,
    viewed_session_id: &str,
    forward: bool,
) -> DialogOutcome {
    let e = view
        .as_any_mut()
        .downcast_mut::<SessionTraceDialog>()
        .expect("kind");
    if e.turn.is_some() {
        if forward {
            e.scroll = e.scroll.saturating_add(1);
        } else {
            e.scroll = e.scroll.saturating_sub(1);
        }
    } else if e.detail {
        let report = app.token_source_report(viewed_session_id);
        let round_index = e.index.min(
            report
                .as_ref()
                .map(|r| crate::shell::overlays::telemetry_round_count(r).saturating_sub(1))
                .unwrap_or(0),
        );
        let count = report
            .as_ref()
            .map(|r| crate::shell::overlays::telemetry_attempt_count(r, round_index))
            .unwrap_or(0)
            .max(1);
        e.turn_cursor = rotate(e.turn_cursor, count, forward);
    } else {
        let count = app
            .token_source_report(viewed_session_id)
            .map(|r| crate::shell::overlays::telemetry_round_count(&r))
            .unwrap_or(0)
            .max(1);
        e.index = rotate(e.index, count, forward);
    }
    DialogOutcome::Consumed
}
