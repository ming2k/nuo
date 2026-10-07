//! The main TUI event/render loop and its modular subsystems.

pub(crate) mod actions;
pub(crate) mod apply;
pub(crate) mod component_input;
pub(crate) mod input_reader;
pub(crate) mod mutations;
pub(crate) mod render;
pub(crate) mod runtime;
pub(crate) mod sync;
pub(crate) mod transcript;

#[allow(unused_imports)]
pub(crate) use actions::{effective_reasoning_effort, modal_page_step};
pub(crate) use mutations::{AppMutation, Buffer, CompletionSignal, TranscriptEdit};
#[cfg(test)]
pub(crate) use render::render_frame;
pub(crate) use runtime::{SideViewSignal, UiRuntime, now_epoch_ms};
#[cfg(test)]
pub(crate) use transcript::focused_messages_mut;
#[allow(unused_imports)]
pub(crate) use transcript::{display_status, resolve_focused_mut};

#[cfg(test)]
pub(crate) use actions::host_test_shims;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use actions::{
    InterruptTarget, handle_close_modal, handle_ctrl_c, handle_esc_interrupt, handle_modal_down,
    handle_modal_up, handle_send_slash, open_active_connection_detail,
};

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use nuotc::Terminal;
use tokio::sync::mpsc;

use crate::App;
use crate::clipboard;
use crate::clipboard_ops;
use crate::input::{self};
use crate::model::document::TranscriptMessage;

use input_reader::InputReader;
use sync::{
    consume_completion_signal, consume_navigation_signals, sync_request_surfaces,
    sync_transcripts_and_session, tick_toast_timers,
};

/// Whether an event expresses an editing/caret-navigation intent in the live
/// composer even when it is a no-op at the current boundary (for example,
/// `End` while the logical caret is already at the end). Such an intent must
/// leave a wheel-browsed viewport and reveal the caret again.
fn event_rearms_composer_follow(event: &Event) -> bool {
    match event {
        Event::Paste(_) => true,
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            match key.code {
                KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Enter
                | KeyCode::Tab => true,
                KeyCode::Left | KeyCode::Right => !key.modifiers.contains(KeyModifiers::SUPER),
                KeyCode::Up | KeyCode::Down => !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER),
                KeyCode::Char(c)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    !c.is_control()
                }
                KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    matches!(
                        c.to_ascii_lowercase(),
                        'a' | 'b' | 'e' | 'j' | 'k' | 'u' | 'v' | 'w'
                    )
                }
                KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::ALT) => {
                    matches!(c.to_ascii_lowercase(), 'b' | 'd' | 'f')
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// Whether `event` may pass through the frozen (terminal-too-small) state.
///
/// While frozen, only two event classes are honoured: a `Resize`, because it
/// is the one thing that can restore the geometry, and `Ctrl-C`, the escape
/// hatch that still lets the user quit. Every other event is dropped so it can
/// never mutate state the user cannot see (invisible typing, modal opens,
/// scroll moves). The dropped-Key SGR-leak tracker is still fed by the caller
/// so a stranded mouse sequence cannot leak once the UI resumes.
fn frozen_event_passthrough(event: &Event) -> bool {
    match event {
        Event::Resize(..) => true,
        Event::Key(key) => {
            key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        }
        _ => false,
    }
}

pub(crate) fn tool_verb_for(name: &str) -> crate::phase::ToolVerb {
    match name {
        "find_files" | "list_dir" | "read_image" | "read_text" | "read_file" | "read" | "use_skill" | "read_url" => {
            crate::phase::ToolVerb::Exploring
        }
        "search_text" => crate::phase::ToolVerb::Searching,
        "search_web" => crate::phase::ToolVerb::WebSearching,
        "write_file" | "edit_text" => crate::phase::ToolVerb::Editing,
        "execute_command" => crate::phase::ToolVerb::Running,
        "write_todos" | "update_todo" | "todo" | "todo_update" => {
            crate::phase::ToolVerb::UpdatingTasks
        }
        "spawn_agent" | "delegate_code" => crate::phase::ToolVerb::Delegating,
        n if n.starts_with("mcp__") => crate::phase::ToolVerb::Mcp,
        _ => crate::phase::ToolVerb::Generic,
    }
}

pub async fn run_app_loop(
    terminal: &mut Terminal<std::io::Stdout>,
    app: &mut App,
    runtime: UiRuntime,
    mutation_rx: mpsc::Receiver<AppMutation>,
    session: crate::SessionSource,
) -> io::Result<()> {
    let (copy_tx, mut copy_rx) =
        mpsc::unbounded_channel::<Result<clipboard::CopyOutcome, String>>();
    let copy_pending = Arc::new(AtomicUsize::new(0));

    let (paste_tx, mut paste_rx) = mpsc::unbounded_channel::<clipboard::ClipboardRead>();

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Event>();
    let _input_reader = InputReader::spawn(input_tx)?;

    let mut sgr_guard = input::SgrLeakGuard::default();

    let mut input_redraw_pending = true;
    let mut was_animating = true;
    let mut last_carousel_index = 0usize;
    let mut mutation_rx = mutation_rx;
    let mut terminal_resized = false;
    // Tracks whether the frozen notice is currently on screen, so the freeze
    // branch repaints it exactly on entry and on each geometry change rather
    // than every tick.
    let mut frozen_notice_painted = false;

    loop {
        if app.should_quit.load(Ordering::SeqCst) {
            tracing::info!(reason = "should_quit_flag", "app exiting");
            return Ok(());
        }

        // Freeze guard. Below the usable minimum the whole UI is replaced by a
        // centered notice. While frozen we keep applying daemon mutations (so
        // a streaming round is never lost) but block every user-originated
        // event except a resize and Ctrl-C, and paint nothing but the notice —
        // no spinner, carousel, or scroll motion can move state the user
        // cannot see. The live geometry is re-read every iteration, so a
        // resize out of the minimum lifts the freeze on the next pass.
        let (frozen_w, frozen_h) = terminal.size();
        if crate::design::below_minimum(frozen_w, frozen_h) {
            // Daemon-owned state stays current even while frozen.
            while let Ok(mutation) = mutation_rx.try_recv() {
                apply::apply(app, &runtime, mutation);
            }
            // Clipboard results are user-originated; drop them rather than
            // apply a paste the user cannot see. The unbounded channels are
            // drained each pass so they cannot accumulate.
            while copy_rx.try_recv().is_ok() {}
            while paste_rx.try_recv().is_ok() {}

            // Repaint the notice on entry and whenever the geometry changed
            // (the retained grid was invalidated by the resize).
            if !frozen_notice_painted || terminal_resized {
                terminal.draw(|f| crate::render::draw_too_small(f, &app.theme))?;
                frozen_notice_painted = true;
                terminal_resized = false;
            }

            let mut frozen_batch: Vec<Event> = Vec::with_capacity(8);
            tokio::select! {
                biased;
                Some(first_event) = input_rx.recv() => {
                    frozen_batch.push(first_event);
                    while let Ok(ev) = input_rx.try_recv() {
                        frozen_batch.push(ev);
                    }
                }
                _ = runtime.dirty_notify.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            }
            for event in frozen_batch {
                if matches!(event, Event::Resize(..)) {
                    terminal_resized = true;
                }
                if !frozen_event_passthrough(&event) {
                    if let Event::Key(_) = &event {
                        // Keep the SGR-leak tracker fed for dropped input too,
                        // so a stranded mouse sequence cannot leak on resume.
                        let _ = sgr_guard.feed(&event);
                    }
                    continue;
                }
                // Only a resize or Ctrl-C reaches here; resolve the live
                // session id exactly as the unfrozen path does.
                let viewed_session_id = session.session_id().await;
                let flow = process_one_event(
                    &event,
                    app,
                    terminal,
                    &runtime,
                    &session,
                    &viewed_session_id,
                    &copy_tx,
                    &copy_pending,
                    &paste_tx,
                    &mut sgr_guard,
                    &mut input_redraw_pending,
                )
                .await?;
                if matches!(flow, actions::ActionFlow::Exit) {
                    return Ok(());
                }
            }
            continue;
        }
        // Leaving the frozen state: drop the latch and force a clean full
        // repaint once the normal chrome resumes.
        if frozen_notice_painted {
            frozen_notice_painted = false;
            input_redraw_pending = true;
        }

        let mut frame_dirty = input_redraw_pending;
        input_redraw_pending = false;

        while let Ok(result) = copy_rx.try_recv() {
            clipboard_ops::set_copy_feedback(app, result);
            app.copy_toast_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(1800));
            frame_dirty = true;
        }

        while let Ok(read) = paste_rx.try_recv() {
            clipboard_ops::apply_clipboard_paste(app, read);
            frame_dirty = true;
        }

        // ADR-0197 M1: drain every pending translator mutation into `App`.
        // The applier is the sole `App` writer for daemon-originated state;
        // this pass runs before any reconciliation or render.
        while let Ok(mutation) = mutation_rx.try_recv() {
            if apply::apply(app, &runtime, mutation) {
                frame_dirty = true;
            }
        }

        sync_request_surfaces(app, &runtime);

        if tick_toast_timers(app) {
            frame_dirty = true;
        }

        if app.step_input_drag_scroll() {
            frame_dirty = true;
        }

        let (displayed_transcript_changed, viewed_session_id) =
            sync_transcripts_and_session(app, &runtime).await;

        let (open_sessions, open_tree, open_host) = consume_navigation_signals(app, &runtime);
        if open_sessions {
            crate::event_loop::actions::enter_panel(
                app,
                crate::surfaces::DialogKind::Sessions,
                &runtime,
                &viewed_session_id,
            );
        }
        if open_tree {
            crate::event_loop::actions::enter_panel(
                app,
                crate::surfaces::DialogKind::SessionTree,
                &runtime,
                &viewed_session_id,
            );
        }
        if open_host {
            crate::event_loop::actions::enter_scene(
                app,
                crate::surfaces::SceneKind::Dashboard,
                &runtime,
            );
        }

        if consume_completion_signal(app) {
            frame_dirty = true;
        }
        app.refresh_backend_completion_request();

        let resized_this_frame = terminal_resized;
        terminal_resized = false;

        if app.follow_bottom && !resized_this_frame {
            app.scroll = app.max_scroll;
        }

        let empty_state_showing =
            app.focused_messages().is_empty() && app.focus_stack.is_empty() && !app.in_side_view;
        if empty_state_showing {
            let current_carousel_index =
                crate::empty_state::carousel_page_for(app.carousel_epoch.elapsed().as_millis());
            if current_carousel_index != last_carousel_index {
                last_carousel_index = current_carousel_index;
                frame_dirty = true;
            }
        }

        let viewed_animating = app.viewed_chrome().responding;
        let animating = viewed_animating
            || app.has_live_transport_setback()
            || !app.pending_images.is_empty()
            || app.input_drag_scroll.is_some()
            || ((app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().refreshing
                || app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().refreshing)
                && matches!(
                    app.active_dialog(),
                    Some(
                        crate::surfaces::DialogKind::Models
                            | crate::surfaces::DialogKind::Connections
                    )
                ));

        let is_typing_active = app.last_key_press.elapsed() < std::time::Duration::from_millis(150);
        let animation_draw = animating && !is_typing_active;

        let needs_draw = frame_dirty
            || animation_draw
            || was_animating
            || runtime.dirty.swap(false, Ordering::AcqRel);
        was_animating = animation_draw;

        let stage_bottom_follow =
            (displayed_transcript_changed || resized_this_frame) && app.follow_bottom;
        let stage_settle = app.scroll_settle_pending && !stage_bottom_follow;

        let painted_scroll = app.scroll;
        if needs_draw {
            if stage_bottom_follow || stage_settle {
                terminal.stage(|f| render::render_frame(app, f, &viewed_session_id))?;
            } else {
                terminal.draw(|f| render::render_frame(app, f, &viewed_session_id))?;
                app.ui.commit();
            }
        }

        // Scroll anchoring (resize stability): on the settle pass that follows
        // a width change, the renderer resolved the pre-resize top-of-viewport
        // anchor against the new layout. Apply that offset here, before the
        // clamp below, so a manual reading position survives the reflow instead
        // of drifting to a raw offset that now points at different content. A
        // `None` (no resize armed a resolve, or the anchored message vanished)
        // leaves the existing offset untouched. Non-settle passes discard any
        // resolution outright — the follow-bottom path never needed it, and a
        // lingering value must not leak into a later, unrelated settle.
        if needs_draw {
            if stage_settle {
                if let Some(resolved) = app.layout_height_cache.take_resolved() {
                    app.scroll = resolved.min(u16::MAX as usize) as u16;
                }
            } else {
                app.layout_height_cache.take_resolved();
            }
        }

        // The renderer measured `max_scroll` from this frame's content and
        // viewport. Keep a manual position in bounds while preserving the
        // sticky-header exception used by collapsed subagent summaries.
        if !app.follow_bottom {
            // A collapsed sticky header may leave too little content below it
            // for `max_scroll` to reach the header line; while a pin is
            // active, allow scrolling up to that line so the header stays at
            // the top of the viewport instead of being dragged back down.
            let limit = app
                .pin_summary_line
                .map(|line| app.max_scroll.max(line.min(u16::MAX as usize) as u16))
                .unwrap_or(app.max_scroll);
            app.scroll = app.scroll.min(limit);
        }

        // The staged pass above measured the new content but emitted no bytes.
        // If the bottom moved, redraw immediately at the final offset. If it
        // did not, commit the already-final staged grid without a second layout.
        if stage_bottom_follow && needs_draw {
            if app.scroll != app.max_scroll {
                app.scroll = app.max_scroll;
                input_redraw_pending = true;
                continue;
            }
            terminal.commit_staged()?;
            app.ui.commit();
            // Committed: the staged rect observation is now the published
            // geometry. Nothing to do — the snapshot is simply dropped.
        }
        // A disclosure toggle's scroll target has now been validated against
        // the layout the staged pass just measured (the clamp above ran on the
        // fresh `content_lines`). If the clamp moved the offset, the staged
        // grid is stale — redraw at the settled position; otherwise commit the
        // staged grid, which is already laid out at the correct offset,
        // without a second layout pass.
        if stage_settle && needs_draw {
            app.scroll_settle_pending = false;
            if app.scroll != painted_scroll {
                input_redraw_pending = true;
                continue;
            }
            terminal.commit_staged()?;
            app.ui.commit();
        }
        // A transcript shrink can clamp a manually positioned viewport after
        // a normal draw. Repaint once at the newly valid offset instead of
        // leaving the just-painted frame beyond the new end of the content.
        if needs_draw && app.scroll != painted_scroll {
            input_redraw_pending = true;
            continue;
        }
        app.retain_visible_focused_target();

        let poll_interval = if animating {
            // Heartbeat aligned with spinner updates (100ms) and coalesced streaming deltas
            std::time::Duration::from_millis(100)
        } else {
            let mut next_timeout = std::time::Duration::from_millis(1000);

            // Toast / armed timeouts to wake exactly when state needs to clear
            let now = std::time::Instant::now();
            if let Some(until) = app.copy_toast_until {
                next_timeout = next_timeout.min(
                    until
                        .saturating_duration_since(now)
                        .max(std::time::Duration::from_millis(10)),
                );
            }
            if let Some(until) = app.notice_toast_until {
                next_timeout = next_timeout.min(
                    until
                        .saturating_duration_since(now)
                        .max(std::time::Duration::from_millis(10)),
                );
            }
            if let Some(until) = app.esc_armed_until {
                next_timeout = next_timeout.min(
                    until
                        .saturating_duration_since(now)
                        .max(std::time::Duration::from_millis(10)),
                );
            }

            // Empty-state carousel: sleep until the next slide boundary (up to CAROUSEL_SLIDE_SECS)
            if empty_state_showing {
                let slide_ms = (crate::empty_state::CAROUSEL_SLIDE_SECS as u128) * 1000;
                let elapsed_ms = app.carousel_epoch.elapsed().as_millis();
                let rem_ms = slide_ms.saturating_sub(elapsed_ms % slide_ms);
                next_timeout =
                    next_timeout.min(std::time::Duration::from_millis((rem_ms as u64).max(50)));
            }

            next_timeout
        };

        tokio::select! {
            biased;
            Some(first_event) = input_rx.recv() => {
                let mut batch = Vec::with_capacity(8);
                batch.push(first_event);
                while let Ok(ev) = input_rx.try_recv() {
                    batch.push(ev);
                }
                // A resize inside this batch may have shrunk the terminal
                // below the minimum. Re-evaluate and block the remaining
                // events so a single tick cannot mutate state behind a notice
                // that has not been painted yet.
                let mut frozen_now = false;
                for event in batch {
                    if matches!(event, Event::Resize(..)) {
                        terminal_resized = true;
                        let (w, h) = terminal.size();
                        frozen_now = crate::design::below_minimum(w, h);
                    }
                    if frozen_now && !frozen_event_passthrough(&event) {
                        if let Event::Key(_) = &event {
                            let _ = sgr_guard.feed(&event);
                        }
                        continue;
                    }
                    let flow = process_one_event(
                        &event,
                        app,
                        terminal,
                        &runtime,
                        &session,
                        &viewed_session_id,
                        &copy_tx,
                        &copy_pending,
                        &paste_tx,
                        &mut sgr_guard,
                        &mut input_redraw_pending,
                    ).await?;
                    if matches!(flow, actions::ActionFlow::Exit) {
                        return Ok(());
                    }
                }
            }
            Some(copy_result) = copy_rx.recv() => {
                clipboard_ops::set_copy_feedback(app, copy_result);
                app.copy_toast_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(1800));
                input_redraw_pending = true;
            }
            Some(read) = paste_rx.recv() => {
                clipboard_ops::apply_clipboard_paste(app, read);
                input_redraw_pending = true;
            }
            _ = runtime.dirty_notify.notified() => {
                input_redraw_pending = true;
            }
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)] // Keeps event-loop resources borrowed without a second state bundle.
async fn process_one_event(
    event: &Event,
    app: &mut App,
    terminal: &mut Terminal<std::io::Stdout>,
    runtime: &UiRuntime,
    session: &crate::SessionSource,
    viewed_session_id: &str,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
    paste_tx: &mpsc::UnboundedSender<clipboard::ClipboardRead>,
    sgr_guard: &mut input::SgrLeakGuard,
    input_redraw_pending: &mut bool,
) -> io::Result<actions::ActionFlow> {
    if let Event::Key(_) = event {
        app.last_key_press = std::time::Instant::now();
        if matches!(sgr_guard.feed(event), input::Feed::Drop) {
            return Ok(actions::ActionFlow::Handled);
        }
    }

    if let Event::Key(_) | Event::Paste(_) = event
        && !app.dev_toast_pinned
    {
        if app.copy_toast_until.is_some() {
            app.copy_toast_until = None;
        }
        if app.notice_toast_until.is_some() {
            app.notice_toast_until = None;
        }
    }

    let has_focused_target = app.focused_target.is_some();
    let transcript_focused = app.transcript_focused;
    let event_family = input::event_family(event, &app.ui, has_focused_target, transcript_focused);
    let keyboard_path = app.ui.keyboard_path_for(event_family);
    let active_overlay = match keyboard_path.first().copied() {
        Some(crate::ui::UiKey::Overlay(overlay)) => Some(overlay),
        Some(crate::ui::UiKey::ProviderDelete) => Some(crate::surfaces::OverlaySurface::Dialog(
            crate::surfaces::DialogKind::Connections,
        )),
        _ => app.surfaces.active_overlay(),
    };
    let is_responding = app.viewed_chrome().responding;
    // Scene-local liveness for the Subagent scene's Esc interrupt (ADR-0205):
    // the viewed child's own round state, precomputed here so it does not fight
    // the `&mut app.input` borrow in the dispatch call below.
    let focused_subagent_running = app.focused_subagent_running();
    let completion_kind = app.completion_kind();
    let active_sheet = keyboard_path.iter().find_map(|key| match key {
        crate::ui::UiKey::Sheet(kind) => Some(*kind),
        _ => None,
    });
    let suppress_completions = matches!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Switcher)
    ) || active_sheet.is_some();
    let completions = if suppress_completions {
        Vec::new()
    } else {
        app.completions()
    };
    let suggestion_count = completions.len();
    let has_exact_suggestion = completions
        .iter()
        .any(|c| c.insert_text == app.input || c.label == app.input);
    let suggestion_index = app.suggestion_index;
    let completion_dismissed = app.completion_dismissed;
    let has_trigger_text = app.completion_trigger_text_present();
    let permission_confirm_always = app.permission_confirm_always;
    let permission_show_details = app.permission_show_details;
    let in_history_recall = app.history_index.is_some();
    let history_searching = app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().search;
    let model_searching = app.picker_search();
    let custom_provider_field = if app
        .surfaces
        .contains_sheet(crate::surfaces::SheetKind::CustomProvider)
        && app.custom_text_field_focused()
    {
        Some(app.custom_field)
    } else {
        None
    };
    let editor_field = if app
        .surfaces
        .contains_sheet(crate::surfaces::SheetKind::ModelEditor)
    {
        Some(app.editor_field)
    } else {
        None
    };
    let question_other_highlighted = app
        .question
        .as_ref()
        .is_some_and(|q| q.is_other_highlighted());
    let host_prompting = app.host_prompting;
    let session_info_detail = app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_detail;
    let connection_info_detail = app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail;

    let modal_cmd_history: Option<String> = if matches!(event, Event::Key(k) if k.code == crossterm::event::KeyCode::Enter)
        && app.surfaces.active_overlay().is_none()
        && app.input.starts_with('/')
    {
        Some(app.input.clone())
    } else {
        None
    };

    let recognized_command = app.input.starts_with('/')
        && crate::completion::resolved_slash_command_len(&app.input, &app.command_catalog)
            .is_some();

    // `route_event` performs the hot-path text edits and caret motions
    // directly through mutable references. The edit target is the composer's
    // own buffer — except while a picker's search sub-layer is active, where
    // the dialog entity owns its embedded field and the composer line is never
    // borrowed (ADR-0035, `[INV-SURFACE-01]`).
    #[derive(Clone, Copy, PartialEq)]
    enum EditBuffer {
        Composer,
        Models,
        Connections,
        History,
    }
    let edit_buffer = if app.active_dialog() == Some(crate::surfaces::DialogKind::Models)
        && app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().search
    {
        EditBuffer::Models
    } else if app.active_dialog() == Some(crate::surfaces::DialogKind::Connections)
        && app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().search
    {
        EditBuffer::Connections
    } else if app.active_dialog() == Some(crate::surfaces::DialogKind::HistorySearch)
        && app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().search
    {
        EditBuffer::History
    } else {
        EditBuffer::Composer
    };
    let composer_edit_state_before = match edit_buffer {
        EditBuffer::Composer => (app.input.len(), app.cursor_position),
        EditBuffer::Models => (
            app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().query.text.len(),
            app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().query.cursor,
        ),
        EditBuffer::Connections => (
            app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().query.text.len(),
            app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().query.cursor,
        ),
        EditBuffer::History => (
            app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().query.text.len(),
            app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().query.cursor,
        ),
    };
    let composer_owned_before = app.caret_owner() == crate::CaretOwner::Composer;
    let current_scene = app.current_scene();

    let action = if let Some(component_action) = component_input::route(app, event, &keyboard_path)
    {
        component_action
    } else {
        let dispatch = input::Dispatch {
            overlay: active_overlay,
            sheet: active_sheet,
            pre_attach: app.pre_attach.is_some(),
            scene: current_scene,
            key_overrides: app.key_overrides.clone(),
            focused_target: has_focused_target,
            transcript_focused,
            scene_blocked: matches!(
                keyboard_path.first(),
                Some(crate::ui::UiKey::ConfigDropdown | crate::ui::UiKey::ProviderDelete)
            ),
            scene_namespace_armed: app.scene_namespace_armed,
        };
        let modal_keys = crate::modal_keys::ModalKeys {
            model_searching,
            history_searching,
            custom_provider_field,
            editor_field,
            config_focus: app.config_focus,
            session_info_detail,
            connection_info_detail,
            host_prompting,
            dialog_keys: app.dialog_keys(),
        };
        let sheet_keys = crate::sheet::SheetKeys {
            question_other_highlighted,
            permission_confirm_always,
            permission_show_details,
            focused_target: has_focused_target,
        };
        let scene_keys = crate::session::SceneKeys {
            is_responding,
            composer_send_mode: app.composer_send_mode,
            completion_kind,
            completion_dismissed,
            has_trigger_text,
            suggestion_count,
            suggestion_index,
            has_exact_suggestion,
            in_history_recall,
            surface_overrides: app.surface_overrides.clone(),
            focused_target: has_focused_target,
            transcript_focused,
            focused_subagent_running,
        };
        match edit_buffer {
            EditBuffer::Composer => input::route_event(
                event.clone(),
                &mut app.input,
                &mut app.cursor_position,
                dispatch,
                &modal_keys,
                &sheet_keys,
                &scene_keys,
                &mut app.drag,
            ),
            EditBuffer::Models => {
                let d = &mut app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>();
                input::route_event(
                    event.clone(),
                    &mut d.query.text,
                    &mut d.query.cursor,
                    dispatch,
                    &modal_keys,
                    &sheet_keys,
                    &scene_keys,
                    &mut app.drag,
                )
            }
            EditBuffer::Connections => {
                let d = &mut app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>();
                input::route_event(
                    event.clone(),
                    &mut d.query.text,
                    &mut d.query.cursor,
                    dispatch,
                    &modal_keys,
                    &sheet_keys,
                    &scene_keys,
                    &mut app.drag,
                )
            }
            EditBuffer::History => {
                let d = &mut app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>();
                input::route_event(
                    event.clone(),
                    &mut d.query.text,
                    &mut d.query.cursor,
                    dispatch,
                    &modal_keys,
                    &sheet_keys,
                    &scene_keys,
                    &mut app.drag,
                )
            }
        }
    };

    let action = if matches!(action, input::InputAction::SendSlash(_)) && !recognized_command {
        if let input::InputAction::SendSlash(text) = action {
            input::InputAction::SendChat(text)
        } else {
            action
        }
    } else {
        action
    };

    if action.is_text_modal_command()
        && let Some(entry) = modal_cmd_history
    {
        let (name, args) = actions::split_command_word(&entry);
        let mut msg =
            TranscriptMessage::pending_command(name, args).with_sent_at_ms(now_epoch_ms());
        msg.cancel_pending_command();
        app.messages.push(msg);
        app.layout_height_cache.clear();
        app.transcript_changed_pending = true;
        app.record_input_history(entry, Vec::new(), Vec::new());
    }

    let flow = actions::dispatch_action(
        app,
        terminal,
        action,
        &mut actions::ActionContext {
            runtime,
            session,
            viewed_session_id,
            copy_tx,
            copy_pending,
            paste_tx,
            sgr_guard,
        },
    )
    .await;

    let edit_state_after = match edit_buffer {
        EditBuffer::Composer => (app.input.len(), app.cursor_position),
        EditBuffer::Models => (
            app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().query.text.len(),
            app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().query.cursor,
        ),
        EditBuffer::Connections => (
            app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().query.text.len(),
            app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().query.cursor,
        ),
        EditBuffer::History => (
            app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().query.text.len(),
            app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().query.cursor,
        ),
    };
    if edit_state_after != composer_edit_state_before
        || (composer_owned_before && event_rearms_composer_follow(event))
    {
        app.input_scroll_follow_cursor = true;
    }

    if matches!(
        event,
        Event::Key(_) | Event::Mouse(_) | Event::Paste(_) | Event::Resize(..)
    ) {
        *input_redraw_pending = true;
    }

    let completions = if suppress_completions {
        Vec::new()
    } else {
        app.completions()
    };
    app.anchor_completion_selection(&completions);

    Ok(flow)
}

#[cfg(test)]
mod input_scroll_follow_tests {
    use super::*;
    use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn editing_intent_rearms_follow_without_treating_global_navigation_as_editing() {
        assert!(event_rearms_composer_follow(&key(
            KeyCode::End,
            KeyModifiers::NONE
        )));
        assert!(event_rearms_composer_follow(&key(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL
        )));
        assert!(!event_rearms_composer_follow(&key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(!event_rearms_composer_follow(&key(
            KeyCode::Up,
            KeyModifiers::CONTROL
        )));
        assert!(!event_rearms_composer_follow(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })));
    }
}

#[cfg(test)]
mod freeze_guard_tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};

    #[test]
    fn below_minimum_is_the_geometric_authority() {
        assert!(crate::design::below_minimum(
            crate::design::MIN_TERMINAL_COLS - 1,
            crate::design::MIN_TERMINAL_ROWS
        ));
        assert!(crate::design::below_minimum(
            crate::design::MIN_TERMINAL_COLS,
            crate::design::MIN_TERMINAL_ROWS - 1
        ));
        assert!(!crate::design::below_minimum(
            crate::design::MIN_TERMINAL_COLS,
            crate::design::MIN_TERMINAL_ROWS
        ));
        assert!(!crate::design::below_minimum(120, 40));
    }

    #[test]
    fn frozen_passthrough_allows_only_resize_and_ctrl_c() {
        // Ctrl-C is the documented escape hatch.
        assert!(frozen_event_passthrough(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        ))));
        assert!(frozen_event_passthrough(&Event::Resize(44, 12)));

        // Ctrl-C key-up must not slip through as a second press.
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            ..KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        };
        assert!(!frozen_event_passthrough(&Event::Key(release)));

        // No other event may mutate state while the notice is up.
        assert!(!frozen_event_passthrough(&Event::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
        ))));
        assert!(!frozen_event_passthrough(&Event::Key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL,
        ))));
        assert!(!frozen_event_passthrough(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))));
        assert!(!frozen_event_passthrough(&Event::Paste("x".into())));
        assert!(!frozen_event_passthrough(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 3,
            row: 3,
            modifiers: KeyModifiers::NONE,
        })));
    }
}
