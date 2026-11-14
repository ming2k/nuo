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
pub(crate) mod translators;

#[allow(unused_imports)]
pub(crate) use actions::{effective_reasoning_effort, modal_page_step};
/// The transcript edit vocabulary is re-exported for the shell's event loop and
/// for the test harness that drives it.
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use mutations::TranscriptEdit;
pub(crate) use mutations::{AppMutation, CompletionSignal};
#[cfg(test)]
pub(crate) use render::render_frame;
pub(crate) use runtime::{UiRuntime, now_epoch_ms};
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

use crate::shell::App;
use crate::shell::clipboard;
use crate::shell::clipboard_ops;
use crate::shell::input::{self};

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

pub(crate) fn tool_verb_for(name: &str) -> nuo_view::phase::ToolVerb {
    match name {
        "find_files" | "read" | "read_url" => nuo_view::phase::ToolVerb::Exploring,
        "search_text" => nuo_view::phase::ToolVerb::Searching,
        "search_web" => nuo_view::phase::ToolVerb::WebSearching,
        "write_file" | "edit_text" => nuo_view::phase::ToolVerb::Editing,
        "execute_command" => nuo_view::phase::ToolVerb::Running,
        "write_todos" | "update_todo" | "todo" | "todo_update" => {
            nuo_view::phase::ToolVerb::UpdatingTasks
        }
        "spawn_agent" | "delegate_code" => nuo_view::phase::ToolVerb::Delegating,
        n if n.starts_with("mcp__") => nuo_view::phase::ToolVerb::Mcp,
        _ => nuo_view::phase::ToolVerb::Generic,
    }
}

/// One pass of the frozen (terminal-too-small) state.
///
/// Below the usable minimum the whole UI is replaced by a centered notice.
/// Server mutations are still applied so a streaming round is never lost, but
/// user-originated clipboard results are discarded and only a resize or Ctrl-C
/// is allowed through. Geometry is re-read by the caller every iteration, so a
/// resize out of the minimum lifts the freeze on the next pass. The struct
/// borrows the loop's resources for exactly one pass; `run` reports the
/// passthrough event's flow so the caller can honor `Exit`.
struct FrozenFrame<'a, W: io::Write> {
    app: &'a mut App,
    terminal: &'a mut Terminal<W>,
    runtime: &'a UiRuntime,
    session: &'a crate::shell::HookStartSource,
    input: &'a mut input_reader::InputSource,
    mutation_rx: &'a mut mpsc::Receiver<AppMutation>,
    copy_rx: &'a mut mpsc::UnboundedReceiver<Result<clipboard::CopyOutcome, String>>,
    paste_rx: &'a mut mpsc::UnboundedReceiver<clipboard::ClipboardRead>,
    copy_tx: &'a mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &'a Arc<AtomicUsize>,
    paste_tx: &'a mpsc::UnboundedSender<clipboard::ClipboardRead>,
    sgr_guard: &'a mut input::SgrLeakGuard,
    input_redraw_pending: &'a mut bool,
    terminal_resized: &'a mut bool,
    frozen_notice_painted: &'a mut bool,
}

impl<W: io::Write> FrozenFrame<'_, W> {
    async fn run(self) -> io::Result<actions::ActionFlow> {
        // Server-owned state stays current even while frozen.
        while let Ok(mutation) = self.mutation_rx.try_recv() {
            apply::apply(self.app, self.runtime, mutation);
        }
        // Clipboard results are user-originated; drop them rather than apply a
        // paste the user cannot see. The unbounded channels are drained each
        // pass so they cannot accumulate.
        while self.copy_rx.try_recv().is_ok() {}
        while self.paste_rx.try_recv().is_ok() {}

        // Repaint the notice on entry and whenever the geometry changed (the
        // retained grid was invalidated by the resize).
        if !*self.frozen_notice_painted || *self.terminal_resized {
            self.terminal
                .draw(|f| crate::shell::render::draw_too_small(f, &self.app.surfaces.theme))?;
            *self.frozen_notice_painted = true;
            *self.terminal_resized = false;
        }

        let mut frozen_batch: Vec<Event> = Vec::with_capacity(8);
        tokio::select! {
            biased;
            Some(first_event) = self.input.recv() => {
                frozen_batch.push(first_event);
                while let Ok(ev) = self.input.try_recv() {
                    frozen_batch.push(ev);
                }
            }
            _ = self.runtime.dirty_notify.notified() => {}
            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
        }
        for event in frozen_batch {
            if matches!(event, Event::Resize(..)) {
                *self.terminal_resized = true;
            }
            if !frozen_event_passthrough(&event) {
                if let Event::Key(_) = &event {
                    // Keep the SGR-leak tracker fed for dropped input too, so
                    // a stranded mouse sequence cannot leak on resume.
                    let _ = self.sgr_guard.feed(&event);
                }
                continue;
            }
            // Only a resize or Ctrl-C reaches here; resolve the live session
            // id exactly as the unfrozen path does.
            let viewed_session_id = self.session.session_id().await;
            let flow = process_one_event(
                &event,
                self.app,
                self.terminal,
                self.runtime,
                self.session,
                &viewed_session_id,
                self.copy_tx,
                self.copy_pending,
                self.paste_tx,
                self.sgr_guard,
                self.input_redraw_pending,
            )
            .await?;
            if matches!(flow, actions::ActionFlow::Exit) {
                return Ok(flow);
            }
        }
        Ok(actions::ActionFlow::Handled)
    }
}

/// Drain the background channels and the per-frame surface timers.
///
/// Returns whether anything changed state that the next render must show.
/// Ordering matters: clipboard feedback lands before translator mutations, and
/// surface sync, toast expiry and drag motion run after the applier has written
/// the frame's server-originated state.
fn drain_background_channels(
    app: &mut App,
    runtime: &UiRuntime,
    mutation_rx: &mut mpsc::Receiver<AppMutation>,
    copy_rx: &mut mpsc::UnboundedReceiver<Result<clipboard::CopyOutcome, String>>,
    paste_rx: &mut mpsc::UnboundedReceiver<clipboard::ClipboardRead>,
) -> bool {
    let mut frame_dirty = false;

    while let Ok(result) = copy_rx.try_recv() {
        clipboard_ops::set_copy_feedback(app, result);
        app.surfaces.copy_toast_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(1800));
        frame_dirty = true;
    }

    while let Ok(read) = paste_rx.try_recv() {
        clipboard_ops::apply_clipboard_paste(app, read);
        frame_dirty = true;
    }

    // M1: drain every pending translator mutation into `App`.
    // The applier is the sole `App` writer for server-originated state;
    // this pass runs before any reconciliation or render.
    while let Ok(mutation) = mutation_rx.try_recv() {
        if apply::apply(app, runtime, mutation) {
            frame_dirty = true;
        }
    }

    sync_request_surfaces(app, runtime);

    if tick_toast_timers(app) {
        frame_dirty = true;
    }

    if app.step_input_drag_scroll() {
        frame_dirty = true;
    }

    frame_dirty
}

/// How long the loop may sleep before it must wake to service animation or a
/// toast/notice/escape timeout. `empty_state_showing` adds the carousel slide
/// boundary so the empty state advances on time even while otherwise idle.
fn next_poll_interval(
    app: &App,
    animating: bool,
    empty_state_showing: bool,
) -> std::time::Duration {
    if animating {
        // Heartbeat aligned with spinner updates (100ms) and coalesced
        // streaming deltas.
        return std::time::Duration::from_millis(100);
    }

    let mut next_timeout = std::time::Duration::from_millis(1000);

    // Toast / armed timeouts to wake exactly when state needs to clear
    let now = std::time::Instant::now();
    if let Some(until) = app.surfaces.copy_toast_until {
        next_timeout = next_timeout.min(
            until
                .saturating_duration_since(now)
                .max(std::time::Duration::from_millis(10)),
        );
    }
    if let Some(until) = app.surfaces.notice_toast_until {
        next_timeout = next_timeout.min(
            until
                .saturating_duration_since(now)
                .max(std::time::Duration::from_millis(10)),
        );
    }
    if let Some(until) = app.surfaces.esc_armed_until {
        next_timeout = next_timeout.min(
            until
                .saturating_duration_since(now)
                .max(std::time::Duration::from_millis(10)),
        );
    }

    // Empty-state carousel: sleep until the next slide boundary (up to
    // CAROUSEL_SLIDE_SECS).
    if empty_state_showing {
        let slide_ms = (nuo_view::empty_state::CAROUSEL_SLIDE_SECS as u128) * 1000;
        let elapsed_ms = app.transcript.carousel_epoch.elapsed().as_millis();
        let rem_ms = slide_ms.saturating_sub(elapsed_ms % slide_ms);
        next_timeout = next_timeout.min(std::time::Duration::from_millis((rem_ms as u64).max(50)));
    }

    next_timeout
}

pub(crate) async fn run_app_loop(
    terminal: &mut Terminal<std::io::Stdout>,
    app: &mut App,
    runtime: UiRuntime,
    mutation_rx: mpsc::Receiver<AppMutation>,
    session: crate::shell::HookStartSource,
    input: &mut input_reader::InputSource,
) -> io::Result<()> {
    let (copy_tx, mut copy_rx) =
        mpsc::unbounded_channel::<Result<clipboard::CopyOutcome, String>>();
    let copy_pending = Arc::new(AtomicUsize::new(0));

    let (paste_tx, mut paste_rx) = mpsc::unbounded_channel::<clipboard::ClipboardRead>();

    let mut sgr_guard = input::SgrLeakGuard::default();

    let mut input_redraw_pending = true;
    let mut was_animating = true;
    let mut last_carousel_index = 0usize;
    let mut mutation_rx = mutation_rx;
    let mut terminal_resized = false;
    let mut last_composer_height: Option<u16> = None;
    // Tracks whether the frozen notice is currently on screen, so the freeze
    // branch repaints it exactly on entry and on each geometry change rather
    // than every tick.
    let mut frozen_notice_painted = false;

    loop {
        if app.link.should_quit.load(Ordering::SeqCst) {
            tracing::info!(reason = "should_quit_flag", "app exiting");
            return Ok(());
        }

        // The link died. Ending this *attachment* — not the process — is what
        // makes recovery possible: the runner re-dials and re-attaches, and the
        // transcript, tabs and history ride `TuiOutcome` across the gap
        // (ADR-0136). Recovering here rather than on the next user gesture means
        // a transient socket failure costs the operator nothing.
        if app.link.relink_requested {
            tracing::warn!("link lost; ending attachment so the runner can re-dial");
            return Ok(());
        }

        // Freeze guard. Below the usable minimum the whole UI is replaced by a
        // centered notice; `FrozenFrame` owns that pass so the normal path
        // below only ever runs with visible chrome.
        let (frozen_w, frozen_h) = terminal.size();
        if nuo_ui_primitives::design::below_minimum(frozen_w, frozen_h) {
            let flow = FrozenFrame {
                app,
                terminal,
                runtime: &runtime,
                session: &session,
                input,
                mutation_rx: &mut mutation_rx,
                copy_rx: &mut copy_rx,
                paste_rx: &mut paste_rx,
                copy_tx: &copy_tx,
                copy_pending: &copy_pending,
                paste_tx: &paste_tx,
                sgr_guard: &mut sgr_guard,
                input_redraw_pending: &mut input_redraw_pending,
                terminal_resized: &mut terminal_resized,
                frozen_notice_painted: &mut frozen_notice_painted,
            }
            .run()
            .await?;
            if matches!(flow, actions::ActionFlow::Exit) {
                return Ok(());
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

        if drain_background_channels(app, &runtime, &mut mutation_rx, &mut copy_rx, &mut paste_rx) {
            frame_dirty = true;
        }

        let (displayed_transcript_changed, viewed_session_id) =
            sync_transcripts_and_session(app, &runtime).await;

        let (open_sessions, open_tree) = consume_navigation_signals(app, &runtime);
        if open_sessions {
            crate::shell::event_loop::actions::enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Threads,
                &runtime,
                &viewed_session_id,
            );
        }
        if open_tree {
            crate::shell::event_loop::actions::enter_panel(
                app,
                crate::shell::surfaces::DialogKind::SessionTree,
                &runtime,
                &viewed_session_id,
            );
        }

        if consume_completion_signal(app) {
            frame_dirty = true;
        }
        app.refresh_backend_completion_request();

        let resized_this_frame = terminal_resized;
        terminal_resized = false;

        let (term_w, term_h) = terminal.size();
        let viewport_rect = nuotc::Rect::new(
            0,
            0,
            term_w,
            term_h.saturating_sub(nuo_ui_primitives::design::PAGE_HEADER_ROWS),
        );
        let in_subagent = !app.transcript.focus_stack.is_empty();
        // A full-screen scene replaces the chat pane entirely. The log scene
        // belongs here: it paints its own page over the whole frame, so reserving
        // composer height behind it would leave the layout measuring a pane
        // nobody can see.
        let is_fullscreen_scene = matches!(
            app.current_scene(),
            crate::shell::surfaces::SceneKind::Dashboard
                | crate::shell::surfaces::SceneKind::Settings
                | crate::shell::surfaces::SceneKind::CommandLog
        );
        let composer_active = !is_fullscreen_scene && !in_subagent;
        let current_composer_height = if composer_active {
            crate::shell::render::calculate_composer_height(viewport_rect, &app.composer.input)
        } else {
            0
        };
        let composer_resized =
            last_composer_height.is_some_and(|prev| prev != current_composer_height);
        if let Some(prev_h) = last_composer_height {
            if prev_h != current_composer_height && app.transcript.view_height > 0 {
                let delta = current_composer_height as i32 - prev_h as i32;
                let estimated_view_height =
                    (app.transcript.view_height as i32 - delta).max(1) as u16;
                app.transcript.view_height = estimated_view_height;
                app.transcript.max_scroll = app
                    .transcript
                    .content_lines
                    .saturating_sub(estimated_view_height as usize)
                    .min(u16::MAX as usize) as u16;
                if !app.transcript.follow_bottom {
                    app.transcript.scroll_settle_pending = true;
                }
                frame_dirty = true;
            }
        }
        last_composer_height = Some(current_composer_height);

        let viewport_geometry_changed = resized_this_frame || composer_resized;

        if app.transcript.follow_bottom && !resized_this_frame {
            app.transcript.scroll = app.transcript.max_scroll;
        }

        let empty_state_showing =
            app.focused_messages().is_empty() && app.transcript.focus_stack.is_empty();
        if empty_state_showing {
            let current_carousel_index = nuo_view::empty_state::carousel_page_for(
                app.transcript.carousel_epoch.elapsed().as_millis(),
            );
            if current_carousel_index != last_carousel_index {
                last_carousel_index = current_carousel_index;
                frame_dirty = true;
            }
        }

        let viewed_animating = app.viewed_chrome().responding;
        // The Models dialog is live in two cases: a catalog refresh is in
        // flight (the header spinner), or the focused row is revealing a cell
        // that does not fit (`[INV-TUI-MARQUEE-01]`). The second is reported by
        // the draw itself, so an idle list — every row static, nothing hidden —
        // costs no repaints at all.
        let models_live = app.active_dialog() == Some(crate::shell::surfaces::DialogKind::Models)
            && {
                let d = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
                d.refreshing || d.reveal_active
            };
        let animating = viewed_animating
            || app.has_live_transport_setback()
            || !app.composer.pending_images.is_empty()
            || app.composer.input_drag_scroll.is_some()
            || models_live
            || (app
                .surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                .refreshing
                && app.active_dialog() == Some(crate::shell::surfaces::DialogKind::Connections));

        let is_typing_active =
            app.link.last_key_press.elapsed() < std::time::Duration::from_millis(150);
        let animation_draw = animating && !is_typing_active;

        let needs_draw = frame_dirty
            || animation_draw
            || was_animating
            || runtime.dirty.swap(false, Ordering::AcqRel);
        was_animating = animation_draw;

        let stage_bottom_follow = (displayed_transcript_changed || viewport_geometry_changed)
            && app.transcript.follow_bottom;
        let stage_settle = app.transcript.scroll_settle_pending && !stage_bottom_follow;

        let painted_scroll = app.transcript.scroll;
        if needs_draw {
            if stage_bottom_follow || stage_settle {
                terminal.stage(|f| render::render_frame(app, f, &viewed_session_id))?;
            } else {
                terminal.draw(|f| render::render_frame(app, f, &viewed_session_id))?;
                app.transcript.ui.commit();
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
                if let Some(resolved) = app.transcript.layout_height_cache.take_resolved() {
                    app.transcript.scroll = resolved.min(u16::MAX as usize) as u16;
                }
            } else {
                app.transcript.layout_height_cache.take_resolved();
            }
        }

        // The renderer measured `max_scroll` from this frame's content and
        // viewport. Keep a manual position in bounds while preserving the
        // sticky-header exception used by collapsed subagent summaries.
        if !app.transcript.follow_bottom {
            // A collapsed sticky header may leave too little content below it
            // for `max_scroll` to reach the header line; while a pin is
            // active, allow scrolling up to that line so the header stays at
            // the top of the viewport instead of being dragged back down.
            // When total content fits within the viewport, scroll collapses to 0.
            let limit = if app.transcript.content_lines <= app.transcript.view_height as usize {
                0
            } else {
                app.transcript
                    .pin_summary_line
                    .map(|line| {
                        app.transcript
                            .max_scroll
                            .max(line.min(u16::MAX as usize) as u16)
                    })
                    .unwrap_or(app.transcript.max_scroll)
            };
            app.transcript.scroll = app.transcript.scroll.min(limit);
        } else {
            app.transcript.scroll = app.transcript.max_scroll;
        }

        // The staged pass above measured the new content but emitted no bytes.
        // If the bottom moved, redraw immediately at the final offset. If it
        // did not, commit the already-final staged grid without a second layout.
        if stage_bottom_follow && needs_draw {
            if app.transcript.scroll != painted_scroll {
                input_redraw_pending = true;
                continue;
            }
            terminal.commit_staged()?;
            app.transcript.ui.commit();
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
            app.transcript.scroll_settle_pending = false;
            if app.transcript.scroll != painted_scroll {
                input_redraw_pending = true;
                continue;
            }
            terminal.commit_staged()?;
            app.transcript.ui.commit();
        }
        // A transcript shrink can clamp a manually positioned viewport after
        // a normal draw. Repaint once at the newly valid offset instead of
        // leaving the just-painted frame beyond the new end of the content.
        if needs_draw && app.transcript.scroll != painted_scroll {
            input_redraw_pending = true;
            continue;
        }
        app.retain_visible_focused_target();

        let poll_interval = next_poll_interval(app, animating, empty_state_showing);

        tokio::select! {
            biased;
            Some(first_event) = input.recv() => {
                let mut batch = Vec::with_capacity(8);
                batch.push(first_event);
                while let Ok(ev) = input.try_recv() {
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
                        frozen_now = nuo_ui_primitives::design::below_minimum(w, h);
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
                app.surfaces.copy_toast_until =
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

/// Whether `event` is an explicit "I have seen this" for a fault bubble.
///
/// `Esc` and its aliases only. The check is deliberately *narrow* and does not
/// consume the gesture: the same `Esc` still reaches the router, so the first
/// press of a two-press interrupt chord dismisses the bubble **and** arms the
/// chord, which is what the user meant by pressing it.
fn dismisses_notice_bubble(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    if key.kind == crossterm::event::KeyEventKind::Release {
        return false;
    }
    matches!(key.code, crossterm::event::KeyCode::Esc)
}

/// The buffer that receives this event's text edits.
#[derive(Clone, Copy, PartialEq)]
enum EditBuffer {
    Composer,
    Models,
    Connections,
}

/// Pick the edit target: a picker's embedded search field while its search
/// sub-layer is active, otherwise the composer's own buffer (ADR-0035,
/// `[INV-SURFACE-01]`).
fn edit_buffer(app: &mut App) -> EditBuffer {
    if app.active_dialog() == Some(crate::shell::surfaces::DialogKind::Models)
        && app
            .surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
            .search
    {
        EditBuffer::Models
    } else if app.active_dialog() == Some(crate::shell::surfaces::DialogKind::Connections)
        && app
            .surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
            .search
    {
        EditBuffer::Connections
    } else {
        EditBuffer::Composer
    }
}

/// Length and caret position of the active edit buffer. Comparing the state
/// before and after dispatch detects whether the buffer changed, which is what
/// re-arms the composer's follow-the-caret scroll.
fn edit_buffer_state(app: &mut App, buffer: EditBuffer) -> (usize, usize) {
    match buffer {
        EditBuffer::Composer => (app.composer.input.len(), app.composer.cursor_position),
        EditBuffer::Models => (
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
                .query
                .text
                .len(),
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
                .query
                .cursor,
        ),
        EditBuffer::Connections => (
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                .query
                .text
                .len(),
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                .query
                .cursor,
        ),
    }
}

/// Query text captured before dispatch.
///
/// `route_event` edits a picker's embedded search field through the dialog
/// entity, so the only way to tell the query changed is to compare against a
/// pre-dispatch copy. `reconcile` re-anchors a changed result list at its first
/// row.
struct QuerySnapshots {
    history: Option<String>,
    models: Option<String>,
    connections: Option<String>,
}

impl QuerySnapshots {
    fn capture(app: &App) -> Self {
        let history =
            if app.active_dialog() == Some(crate::shell::surfaces::DialogKind::HistorySearch) {
                Some(app.composer.input.clone())
            } else {
                None
            };
        let models = if app.active_dialog() == Some(crate::shell::surfaces::DialogKind::Models)
            && app
                .surfaces
                .surfaces
                .dlg::<crate::shell::surfaces::ModelsDialog>()
                .search
        {
            Some(
                app.surfaces
                    .surfaces
                    .dlg::<crate::shell::surfaces::ModelsDialog>()
                    .query
                    .text
                    .clone(),
            )
        } else {
            None
        };
        let connections = if app.active_dialog()
            == Some(crate::shell::surfaces::DialogKind::Connections)
            && app
                .surfaces
                .surfaces
                .dlg::<crate::shell::surfaces::ConnectionsDialog>()
                .search
        {
            Some(
                app.surfaces
                    .surfaces
                    .dlg::<crate::shell::surfaces::ConnectionsDialog>()
                    .query
                    .text
                    .clone(),
            )
        } else {
            None
        };
        Self {
            history,
            models,
            connections,
        }
    }

    fn reconcile(self, app: &mut App) {
        if let Some(before) = self.history
            && app.active_dialog() == Some(crate::shell::surfaces::DialogKind::HistorySearch)
            && before != app.composer.input
        {
            let d = &mut app
                .surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>();
            d.index = 0;
            d.scroll = 0;
            d.follow = true;
        }
        if let Some(before) = self.models
            && app.active_dialog() == Some(crate::shell::surfaces::DialogKind::Models)
            && before
                != app
                    .surfaces
                    .surfaces
                    .dlg::<crate::shell::surfaces::ModelsDialog>()
                    .query
                    .text
        {
            let d = &mut app
                .surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
            d.index = 0;
            d.scroll = 0;
            d.follow = true;
        }
        if let Some(before) = self.connections
            && app.active_dialog() == Some(crate::shell::surfaces::DialogKind::Connections)
            && before
                != app
                    .surfaces
                    .surfaces
                    .dlg::<crate::shell::surfaces::ConnectionsDialog>()
                    .query
                    .text
        {
            let d = &mut app
                .surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>();
            d.index = 0;
            d.scroll = 0;
            d.follow = true;
        }
    }
}

/// Detail-pane toggles read from the dialogs before dispatch.
///
/// `ModalKeys` needs all four by value; bundling the dialog reads keeps the
/// pre-dispatch snapshot in one place.
struct DialogInfoDetails {
    session_info_detail: bool,
    connection_info_detail: bool,
    tools_info_detail: bool,
    model_info_detail: bool,
}

impl DialogInfoDetails {
    fn capture(app: &mut App) -> Self {
        Self {
            session_info_detail: app
                .surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                .info_detail,
            connection_info_detail: app
                .surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                .info_detail,
            tools_info_detail: app
                .surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ToolsDialog>()
                .info_detail,
            model_info_detail: app
                .surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
                .info_detail,
        }
    }
}

#[allow(clippy::too_many_arguments)] // Keeps event-loop resources borrowed without a second state bundle.
async fn process_one_event<W: io::Write>(
    event: &Event,
    app: &mut App,
    terminal: &mut Terminal<W>,
    runtime: &UiRuntime,
    session: &crate::shell::HookStartSource,
    viewed_session_id: &str,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
    paste_tx: &mpsc::UnboundedSender<clipboard::ClipboardRead>,
    sgr_guard: &mut input::SgrLeakGuard,
    input_redraw_pending: &mut bool,
) -> io::Result<actions::ActionFlow> {
    if let Event::Key(_) = event {
        app.link.last_key_press = std::time::Instant::now();
        if matches!(sgr_guard.feed(event), input::Feed::Drop) {
            return Ok(actions::ActionFlow::Handled);
        }
    }

    // A keystroke dismisses an *acknowledgement* bubble and nothing else.
    //
    // Typing is how a user resumes work, so an ack — which only reports the
    // outcome of the action they just took — must not linger over the composer.
    // A *fault* bubble is different: the keystroke that would dismiss it is
    // often the very keystroke that says "I am moving on", and the fault has not
    // been read yet. Killing it on any key would silently defeat the whole dwell
    // budget (`fault_dwell_ms`): a fault would then live only as long as the
    // user happened to sit still, which is precisely the "it vanished before I
    // looked up" complaint.
    //
    // A fault therefore yields to `Esc` **only when `Esc` has no other job**.
    // With the notice viewer open, `Esc` closes the panel — and dismissing the
    // bubble on the same press would make one keystroke destroy both the
    // interruption and the place it points at. The same reasoning covers an open
    // overlay or sheet, where `Esc` means "back out of this".
    if let Event::Key(_) | Event::Paste(_) = event
        && !app.surfaces.dev_toast_pinned
    {
        app.surfaces.copy_toast_until = None;
        if app.surfaces.notice_toast_severity.is_fault() {
            if dismisses_notice_bubble(event)
                && !app.notice_viewer_open()
                && app.surfaces.surfaces.active_overlay().is_none()
                && app.surfaces.surfaces.active_sheet().is_none()
            {
                app.surfaces.notice_toast_until = None;
            }
        } else if app.surfaces.notice_toast_until.is_some() {
            app.surfaces.notice_toast_until = None;
        }
    }

    let focus_plane = app.transcript.focus.plane();
    let event_family = input::event_family(event, &app.transcript.ui, focus_plane);
    let keyboard_path = app.transcript.ui.keyboard_path_for(event_family);
    let active_overlay = match keyboard_path.first().copied() {
        Some(crate::shell::ui::UiKey::Overlay(overlay)) => Some(overlay),
        Some(crate::shell::ui::UiKey::ProviderDelete) => {
            Some(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Connections,
            ))
        }
        _ => app.surfaces.surfaces.active_overlay(),
    };
    let is_responding = app.viewed_chrome().responding;
    // Scene-local liveness for the Subagent scene's Esc interrupt:
    // the viewed child's own round state, precomputed here so it does not fight
    // the `&mut app.composer.input` borrow in the dispatch call below.
    let focused_subagent_running = app.focused_subagent_running();
    let completion_kind = app.completion_kind();
    let active_sheet = keyboard_path.iter().find_map(|key| match key {
        crate::shell::ui::UiKey::Sheet(kind) => Some(*kind),
        _ => None,
    });
    let suppress_completions = matches!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Switcher)
    ) || active_sheet.is_some();
    let completions = if suppress_completions {
        Vec::new()
    } else {
        app.completions()
    };
    let suggestion_count = completions.len();
    let has_exact_suggestion = completions
        .iter()
        .any(|c| c.insert_text == app.composer.input || c.label == app.composer.input);
    let suggestion_index = app.composer.suggestion_index;
    let completion_dismissed = app.composer.completion_dismissed;
    let has_trigger_text = app.completion_trigger_text_present();
    let permission_confirm_always = app.human.permission_confirm_always;
    let permission_show_details = app.human.permission_show_details;
    let in_history_recall = app.composer.history_index.is_some();
    let history_searching =
        app.active_dialog() == Some(crate::shell::surfaces::DialogKind::HistorySearch);
    let model_searching = app.picker_search();
    let custom_provider_field = if app
        .surfaces
        .surfaces
        .contains_sheet(crate::shell::surfaces::SheetKind::CustomProvider)
        && app.custom_text_field_focused()
    {
        Some(app.provider_editor.custom_field)
    } else {
        None
    };
    let editor_field = if app
        .surfaces
        .surfaces
        .contains_sheet(crate::shell::surfaces::SheetKind::ModelEditor)
    {
        Some(app.provider_editor.editor_field)
    } else {
        None
    };
    let question_other_highlighted = app
        .human
        .question
        .as_ref()
        .is_some_and(|q| q.is_other_highlighted());
    let host_prompting = app.surfaces.host_prompting;
    let dialog_details = DialogInfoDetails::capture(app);

    let modal_cmd_history: Option<String> = if matches!(event, Event::Key(k) if k.code == crossterm::event::KeyCode::Enter)
        && app.surfaces.surfaces.active_overlay().is_none()
        && app.composer.input.starts_with('/')
    {
        Some(app.composer.input.clone())
    } else {
        None
    };

    // `route_event` performs the hot-path text edits and caret motions
    // directly through mutable references. The edit target is the composer's
    // own buffer — except while a picker's search sub-layer is active, where
    // the dialog entity owns its embedded field and the composer line is never
    // borrowed (ADR-0035, `[INV-SURFACE-01]`).
    let edit_buffer = edit_buffer(app);
    let composer_edit_state_before = edit_buffer_state(app, edit_buffer);
    let queries_before = QuerySnapshots::capture(app);
    let composer_owned_before = app.caret_owner() == crate::shell::CaretOwner::Composer;
    let current_scene = app.current_scene();

    let action = if let Some(component_action) = component_input::route(app, event, &keyboard_path)
    {
        component_action
    } else {
        let dispatch = input::Dispatch {
            overlay: active_overlay,
            sheet: active_sheet,
            pre_attach: app.surfaces.pre_attach.is_some(),
            scene: current_scene,
            key_overrides: app.surfaces.key_overrides.clone(),
            focus: focus_plane,
            scene_blocked: matches!(
                keyboard_path.first(),
                Some(
                    crate::shell::ui::UiKey::ConfigDropdown
                        | crate::shell::ui::UiKey::ProviderDelete
                )
            ),
            notice_viewer_open: app.notice_viewer_open(),
        };
        let modal_keys = crate::shell::modal_keys::ModalKeys {
            model_searching,
            history_searching,
            custom_provider_field,
            editor_field,
            config_focus: app.surfaces.config_focus,
            session_info_detail: dialog_details.session_info_detail,
            connection_info_detail: dialog_details.connection_info_detail,
            tools_info_detail: dialog_details.tools_info_detail,
            model_info_detail: dialog_details.model_info_detail,
            host_prompting,
            dialog_keys: app.dialog_keys(),
        };
        let sheet_keys = crate::shell::sheet::SheetKeys {
            question_other_highlighted,
            permission_confirm_always,
            permission_show_details,
            transcript_plane: focus_plane == crate::shell::focus::FocusPlane::Transcript,
        };
        let scene_keys = crate::shell::session::SceneKeys {
            is_responding,
            // The caret's ↑/↓ walk the composer's *wrapped* rows, so the
            // resolver gets the geometry the panel is drawn at this frame.
            composer_text_width: app.composer_grid_width(),
            composer_send_mode: app.composer.composer_send_mode,
            completion_kind,
            completion_dismissed,
            has_trigger_text,
            suggestion_count,
            suggestion_index,
            has_exact_suggestion,
            in_history_recall,
            surface_overrides: app.surfaces.surface_overrides.clone(),
            focus: focus_plane,
            entry_focus: app.transcript.focus.entry(),
            focused_subagent_running,
        };
        match edit_buffer {
            EditBuffer::Composer => input::route_event_motion(
                event.clone(),
                &mut app.composer.input,
                &mut app.composer.cursor_position,
                &mut app.composer.caret_motion,
                dispatch,
                &modal_keys,
                &sheet_keys,
                &scene_keys,
                &mut app.transcript.drag,
            ),
            // The modal query fields are single-line: they have no visual row
            // to walk, so the arrow keys keep their list/edge meaning and the
            // motion memory stays local (and therefore untouched).
            EditBuffer::Models => {
                let mut motion = crate::shell::composer_grid::CaretMotion::default();
                let d = &mut app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
                input::route_event_motion(
                    event.clone(),
                    &mut d.query.text,
                    &mut d.query.cursor,
                    &mut motion,
                    dispatch,
                    &modal_keys,
                    &sheet_keys,
                    &scene_keys,
                    &mut app.transcript.drag,
                )
            }
            EditBuffer::Connections => {
                let mut motion = crate::shell::composer_grid::CaretMotion::default();
                let d = &mut app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>();
                input::route_event_motion(
                    event.clone(),
                    &mut d.query.text,
                    &mut d.query.cursor,
                    &mut motion,
                    dispatch,
                    &modal_keys,
                    &sheet_keys,
                    &scene_keys,
                    &mut app.transcript.drag,
                )
            }
        }
    };

    // No client-side "unknown command" downgrade: a `/`-prefixed draft is a
    // slash command, full stop. The client cannot know the server's whole
    // vocabulary (built-ins are compiled into `nuo-server`; an embedding adds
    // more through `SlashCommandRegistry`), so judging it here would both
    // swallow typos into the transcript as if they were prompts **and** deny
    // registered commands it has never heard of. `AgentRequest::SlashCommand` is
    // the single admission authority: it answers an unrecognized name with a
    // typed `CommandResult::UnknownCommand` ("Unknown command: /modles. Did you
    // mean /models?"), which routes to top-right notice toast instead of
    // polluting the transcript — and the applier then hands the draft back to
    // this composer, so a typo costs one edit rather than a retype (ADR-0119,
    // ADR-0149, [INV-CMD-02], [INV-CMD-03], [INV-CMD-09]).

    if action.is_text_modal_command()
        && let Some(entry) = modal_cmd_history
    {
        // ADR-0149 `[INV-CMD-10]`: Modal commands intercept locally to open an L3
        // dialog. They must never emit, stage, or append any message to L0 transcript.
        // They only record composer input history for key recall.
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
            key_event: matches!(event, Event::Key(_)),
        },
    )
    .await;

    // A click on a folded command body's `copy` verb published its intent (the
    // pointer handler holds no clipboard channel). Serve it here, through the
    // same `fold_hidden_rows_copy` resolution the identical keyboard verb uses —
    // one verb, one implementation, one receipt.
    if let Some(mi) = app.surfaces.pending_fold_copy.take() {
        if let Some((text, feedback)) = crate::shell::copy::fold_hidden_rows_copy(app, mi) {
            clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
            app.surfaces.copy_toast_message = feedback;
            app.surfaces.copy_toast_failed = false;
            app.surfaces.copy_toast_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
        } else {
            clipboard_ops::show_copy_failure(app, "this step has nothing hidden to copy");
        }
    }

    let edit_state_after = edit_buffer_state(app, edit_buffer);
    if edit_state_after != composer_edit_state_before
        || (composer_owned_before && event_rearms_composer_follow(event))
    {
        app.composer.input_scroll_follow_cursor = true;
    }
    queries_before.reconcile(app);

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
        assert!(nuo_ui_primitives::design::below_minimum(
            nuo_ui_primitives::design::MIN_TERMINAL_COLS - 1,
            nuo_ui_primitives::design::MIN_TERMINAL_ROWS
        ));
        assert!(nuo_ui_primitives::design::below_minimum(
            nuo_ui_primitives::design::MIN_TERMINAL_COLS,
            nuo_ui_primitives::design::MIN_TERMINAL_ROWS - 1
        ));
        assert!(!nuo_ui_primitives::design::below_minimum(
            nuo_ui_primitives::design::MIN_TERMINAL_COLS,
            nuo_ui_primitives::design::MIN_TERMINAL_ROWS
        ));
        assert!(!nuo_ui_primitives::design::below_minimum(120, 40));
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

#[cfg(test)]
pub(crate) async fn process_event_for_test(app: &mut App, event: &Event, viewed_session_id: &str) {
    let runtime = UiRuntime::minimal_for_test();
    let session = crate::shell::HookStartSource::Remote {
        session_id: viewed_session_id.to_string(),
    };
    let (copy_tx, _copy_rx) = tokio::sync::mpsc::unbounded_channel();
    let copy_pending = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (paste_tx, _paste_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut sgr_guard = input::SgrLeakGuard::default();
    let mut input_redraw_pending = false;
    let backend = nuotc::Backend::new(Vec::new()).with_bce(nuotc::Bce::No);
    let mut terminal = nuotc::Terminal::new(backend);
    let _ = process_one_event(
        event,
        app,
        &mut terminal,
        &runtime,
        &session,
        viewed_session_id,
        &copy_tx,
        &copy_pending,
        &paste_tx,
        &mut sgr_guard,
        &mut input_redraw_pending,
    )
    .await;
}
