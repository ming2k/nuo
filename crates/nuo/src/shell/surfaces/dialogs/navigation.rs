//! Shared row navigation helpers.

use super::*;

pub(crate) fn rotate_selectable<F>(
    cur: usize,
    count: usize,
    forward: bool,
    mut is_selectable: F,
) -> usize
where
    F: FnMut(usize) -> bool,
{
    if count == 0 {
        return 0;
    }
    let cur = cur.min(count - 1);
    let mut next = if forward {
        (cur + 1) % count
    } else if cur == 0 {
        count - 1
    } else {
        cur - 1
    };

    for _ in 0..count {
        if is_selectable(next) {
            return next;
        }
        next = if forward {
            (next + 1) % count
        } else if next == 0 {
            count - 1
        } else {
            next - 1
        };
    }

    cur.min(count.saturating_sub(1))
}

pub(super) fn rotate(cur: usize, count: usize, forward: bool) -> usize {
    rotate_selectable(cur, count, forward, |_| true)
}

pub(super) fn switcher_app_ctx(app: &crate::shell::App) -> crate::shell::keymap::AppContext {
    crate::shell::keymap::AppContext {
        has_overlay: true,
        active_dialog: app
            .surfaces
            .underlying_dialog()
            .or_else(|| app.active_dialog()),
        is_responding: app.viewed_chrome().responding,
        has_selection: !matches!(app.selection, SelectionState::None),
        has_running_task: app.viewed_chrome().responding,
        has_notices: !!app.has_notices(),
        has_focused_entry: app.focus.entry().is_some(),
        queue_count: app.outbox_total(),
        has_session: app.has_session(),
        scene: app.current_scene(),
    }
}
