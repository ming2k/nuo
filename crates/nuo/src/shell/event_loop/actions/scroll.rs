//! Scrolling and wheel event handlers.
use super::tabs::activate_tab_at_index;
use crate::shell::App;
use crate::shell::event_loop::runtime::UiRuntime;
use crate::shell::input;

pub(crate) fn scroll_tick(app: &mut App, down: bool) {
    if let Some((scroll, follow)) = app.modal_scroll_field() {
        if let Some(f) = follow {
            *f = false;
        }
        *scroll = if down {
            scroll.saturating_add(1)
        } else {
            scroll.saturating_sub(1)
        };
    } else if down {
        app.transcript.pin_summary_line = None;
        app.transcript.scroll = app
            .transcript
            .scroll
            .saturating_add(4)
            .min(app.transcript.max_scroll);
        if app.transcript.scroll >= app.transcript.max_scroll {
            app.transcript.follow_bottom = true;
        }
    } else {
        // While a permission sheet is open the transcript stays scrollable,
        // so the wheel / page keys drive the thread behind it, not the
        // sheet's own body.
        app.transcript.follow_bottom = false;
        app.transcript.pin_summary_line = None;
        app.transcript.scroll = app.transcript.scroll.saturating_sub(4);
    }
}

pub(crate) fn scroll_transcript_page(app: &mut App, down: bool) {
    let step = app.transcript.view_height.saturating_sub(1).max(1);
    app.transcript.pin_summary_line = None;
    if down {
        app.transcript.scroll = app
            .transcript
            .scroll
            .saturating_add(step)
            .min(app.transcript.max_scroll);
        if app.transcript.scroll >= app.transcript.max_scroll {
            app.transcript.follow_bottom = true;
        }
    } else {
        app.transcript.follow_bottom = false;
        app.transcript.scroll = app.transcript.scroll.saturating_sub(step);
    }
}

pub(crate) fn scroll_transcript_to_edge(app: &mut App, bottom: bool) {
    app.transcript.pin_summary_line = None;
    if bottom {
        app.transcript.scroll = app.transcript.max_scroll;
        app.transcript.follow_bottom = true;
    } else {
        app.transcript.scroll = 0;
        app.transcript.follow_bottom = false;
    }
}

pub(crate) fn handle_wheel(
    app: &mut App,
    up: bool,
    x: u16,
    y: u16,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) {
    use crate::shell::ui::UiKey;
    match app.transcript.ui.scene().hit_test(x, y).copied() {
        Some(
            UiKey::TabBar
            | UiKey::TabBarItem(_)
            | UiKey::TabBarPrev
            | UiKey::TabBarNext
            | UiKey::TabBarOverview,
        ) => {
            let total = app.surfaces.surfaces.tabs().len().max(1);
            let target_idx = if up {
                if app.surfaces.surfaces.active_tab_index() == 0 {
                    total - 1
                } else {
                    app.surfaces.surfaces.active_tab_index() - 1
                }
            } else {
                (app.surfaces.surfaces.active_tab_index() + 1) % total
            };
            activate_tab_at_index(app, target_idx, runtime, viewed_session_id);
        }
        Some(UiKey::Overlay(overlay))
            if app.transcript.ui.contains(UiKey::Overlay(overlay), x, y) =>
        {
            if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
                let action = if up {
                    input::InputAction::ScrollUp
                } else {
                    input::InputAction::ScrollDown
                };
                let outcome = ent.handle_input(&action, app, viewed_session_id);
                app.surfaces.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return;
                }
            }
            scroll_tick(app, !up);
        }
        Some(UiKey::SettingsCategory(_)) => {
            app.surfaces.config_scroll = if up {
                app.surfaces.config_scroll.saturating_sub(1)
            } else {
                app.surfaces.config_scroll.saturating_add(1)
            };
        }
        Some(
            UiKey::OauthUrl | UiKey::OauthCode | UiKey::SettingsOption(_) | UiKey::SettingsRow(_),
        ) => scroll_tick(app, !up),
        Some(UiKey::ProviderDelete | UiKey::PreAttach) => {}
        Some(
            UiKey::Sheet(crate::shell::sheet::SheetKind::Permission) | UiKey::PermissionAction(_),
        ) if app.human.permission_show_details => {
            app.human.permission_scroll = if up {
                app.human.permission_scroll.saturating_sub(1)
            } else {
                app.human
                    .permission_scroll
                    .saturating_add(1)
                    .min(app.human.permission_max_scroll)
            };
        }
        Some(UiKey::Sheet(crate::shell::sheet::SheetKind::Question) | UiKey::QuestionOption(_)) => {
            app.human.question_modal_follow = false;
            app.human.question_scroll = if up {
                app.human.question_scroll.saturating_sub(1)
            } else {
                app.human.question_scroll.saturating_add(1)
            };
        }
        Some(UiKey::Completion | UiKey::CompletionItem(_)) => {
            let count = app.completions().len();
            if count > 0 {
                app.composer.suggestion_index = Some(if up {
                    match app.composer.suggestion_index {
                        Some(0) | None => count.saturating_sub(1),
                        Some(i) => i.saturating_sub(1),
                    }
                } else {
                    match app.composer.suggestion_index {
                        Some(i) if i + 1 < count => i + 1,
                        _ => 0,
                    }
                });
            }
        }
        Some(UiKey::Composer) if app.step_input_scroll(up, 4).is_none() => {
            scroll_tick(app, !up);
        }
        Some(
            UiKey::Transcript
            | UiKey::Sticky
            | UiKey::Queue
            | UiKey::Activity
            | UiKey::ModelBar
            | UiKey::Context
            | UiKey::Performance
            | UiKey::Connection
            | UiKey::NoticesBadge,
        ) => {
            scroll_tick(app, !up);
        }
        _ => {}
    }
}
